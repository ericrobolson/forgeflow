"""ForgeFlow anime-style worker (AnimeGANv3 and ArcaneGAN). Installation uses only the Python standard library.
Models: https://github.com/TachibanaYoshino/AnimeGANv3/releases/tag/v1.1.0
Upstream models are licensed for non-commercial use; see the repository README.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import signal
import tempfile
import time
import urllib.request

MODELS = {
    "hayao": ("AnimeGANv3_Hayao_36.onnx", "95ba7b219073fd5b12f569bc38056ffd3019cf4caf15b1feb9f73d1286c9f69d"),
    "shinkai": ("AnimeGANv3_Shinkai_37.onnx", "8dc4d789c44db472447806583c56809e155280225b991bd79f9e2cb20793dcb9"),
}
MODELS["arcane"] = ("ArcaneGANv0.4.jit", "8130be6621be513bc8f14e8febc1991a904fb02c86921b4491d7fcb5a0c306f8")
MODEL_URLS = {"arcane": "https://github.com/Sxela/ArcaneGAN/releases/download/v0.4/ArcaneGANv0.4.jit"}
REVISION = 2

MODELS['jp-face'] = ('AnimeGANv3_JP_face_v1.0.onnx', '1e5f85aa230c64df96278b26bf51bc75d4327156e2f8606e613fa1574d8a6fff')
MODEL_URLS['jp-face'] = 'https://github.com/TachibanaYoshino/AnimeGANv3_gui.exe/releases/download/V1.1/AnimeGANv3_JP_face_v1.0.onnx'

MODELS['cute'] = ('AnimeGANv3_tiny_Cute.onnx', '3ef02d6b2939a0e81621abc5beb0e741bb58654570033cd0e97f6b6ef37c1171')
MODEL_URLS['cute'] = 'https://github.com/TachibanaYoshino/AnimeGANv3_gui.exe/releases/download/V1.1/AnimeGANv3_tiny_Cute.onnx'

MODELS['ghibli'] = ('AnimeGANv3_large_Ghibli_c1_e299.onnx', 'ad950eabe2a556424b1cacf0f4b7847777ea44033482e673771da878054c1e37')
MODEL_URLS['ghibli'] = 'https://github.com/TachibanaYoshino/AnimeGANv3_Portrait_Inference/releases/download/1.0/AnimeGANv3_large_Ghibli_c1_e299.onnx'

MODELS['sketch'] = ('AnimeGANv3_PortraitSketch_25.onnx', '86de643d216a387ae94d6881fc4197e3427e9dca31a030386a1526f8077c55d8')
MODEL_URLS['sketch'] = 'https://github.com/TachibanaYoshino/AnimeGANv3_Portrait_Inference/releases/download/1.0/AnimeGANv3_PortraitSketch_25.onnx'

MODELS.update({name: MODELS["hayao"] for name in ("8bit", "comic")})


def progress(message):
    print(message, flush=True)


def run(args):
    process = subprocess.Popen([str(a) for a in args], stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    try:
        stdout, stderr = process.communicate()
        if process.returncode:
            raise RuntimeError(f"{args[0]} failed: {stderr[-4000:]}")
        return stdout
    finally:
        if process.poll() is None:
            process.terminate()
            try:
                process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait()


def digest(path):
    with open(path, "rb") as file:
        checksum = hashlib.sha256()
        for block in iter(lambda: file.read(1024 * 1024), b""):
            checksum.update(block)
        return checksum.hexdigest()


def install(args):
    import fcntl
    root = Path(args.root)
    root.mkdir(parents=True, exist_ok=True)
    # OS releases this lock even if installation is interrupted.
    with open(root / ".install.lock", "w") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        python = root / ".venv/bin/python"
        runtime = "onnxruntime-gpu[cuda,cudnn]==1.24.4" if args.gpu else "onnxruntime==1.24.4"
        packages = [runtime, "numpy==2.2.6", "Pillow==11.3.0", "av==16.1.0"]
        wanted = {"revision": REVISION, "packages": packages, "python": sys.version.split()[0]}
        marker = root / "installation.json"
        distributions = {"onnxruntime-gpu" if args.gpu else "onnxruntime": "1.24.4",
                         "numpy": "2.2.6", "Pillow": "11.3.0", "av": "16.1.0"}
        health_check = ("import onnxruntime, numpy, PIL, av; import importlib.metadata as m; "
                        f"assert all(m.version(k) == v for k, v in {distributions!r}.items())")
        try:
            healthy = json.loads(marker.read_text()) == wanted
            if healthy:
                run([python, "-c", health_check])
        except (OSError, ValueError, RuntimeError):
            healthy = False
        if not healthy:
            progress(f"Installing anime-style runtime in {root} ({'CUDA' if args.gpu else 'CPU'})…")
            if not python.exists():
                run([sys.executable, "-m", "venv", root / ".venv"])
            run([python, "-m", "pip", "install", "--disable-pip-version-check", *packages])
            run([python, "-c", health_check])
        name, checksum = MODELS[args.style]
        model = root / "models" / name
        model.parent.mkdir(exist_ok=True)
        if not model.exists() or digest(model) != checksum:
            progress(f"Downloading {args.style} model…")
            partial = model.with_suffix(".partial")
            try:
                url = MODEL_URLS.get(args.style, f"https://github.com/TachibanaYoshino/AnimeGANv3/releases/download/v1.1.0/{name}")
                with urllib.request.urlopen(url, timeout=120) as response, open(partial, "wb") as file:
                    shutil.copyfileobj(response, file)
                if digest(partial) != checksum:
                    raise RuntimeError(f"checksum mismatch for {name}")
                os.replace(partial, model)
            finally:
                partial.unlink(missing_ok=True)
        bridge = root / "forgeflow_bridge.py"
        source = Path(__file__).read_bytes()
        if not bridge.exists() or bridge.read_bytes() != source:
            temporary = root / "bridge.partial"
            temporary.write_bytes(source)
            os.replace(temporary, bridge)
        if args.style == "arcane":
            ensure_arcane_export(root, python, bridge, model)
        # A real inference validates the model before recording a successful install.
        validation = root / (args.style + ".validated")
        if not healthy or not validation.exists() or validation.read_text() != checksum:
            run([python, bridge, "--root", root, "--style", args.style, "--check", *( ["--gpu"] if args.gpu else [])])
            (root / (args.style + ".validated")).write_text(checksum)
        if not healthy:
            temporary = root / "installation.partial"
            temporary.write_text(json.dumps(wanted, indent=2))
            os.replace(temporary, marker)
    return python, bridge


def ensure_arcane_export(root, python, bridge, source):
    model = root / "models/ArcaneGANv0.4.onnx"
    record = root / "arcane-export.json"
    try:
        metadata = json.loads(record.read_text())
        if metadata["revision"] == REVISION and metadata["source"] == digest(source) and metadata["model"] == digest(model):
            return
    except (OSError, ValueError, KeyError, TypeError):
        pass
    progress("Preparing ArcaneGAN for ONNX (one-time CPU PyTorch installation and conversion)…")
    try:
        run([python, "-c", "import torch, onnx; assert torch.__version__.split('+')[0] == '2.6.0'; assert onnx.__version__ == '1.17.0'"])
    except RuntimeError:
        run([python, "-m", "pip", "install", "--extra-index-url", "https://download.pytorch.org/whl/cpu", "torch==2.6.0", "onnx==1.17.0"])
    run([python, bridge, "--root", root, "--export-arcane"])
    temporary = record.with_suffix(".partial")
    temporary.write_text(json.dumps({"revision": REVISION, "source": digest(source), "model": digest(model)}))
    os.replace(temporary, record)


def export_arcane(root):
    import torch
    torch.set_num_threads(min(4, os.cpu_count() or 1))
    source = Path(root) / "models/ArcaneGANv0.4.jit"
    model = torch.jit.load(str(source), map_location="cpu").eval()
    destination = source.with_suffix(".onnx")
    temporary = source.with_suffix(".partial.onnx")
    try:
        with torch.inference_mode():
            torch.onnx.export(model, torch.zeros(1, 3, 256, 256, dtype=torch.float16), str(temporary),
                              opset_version=17, input_names=["image"], output_names=["output"],
                              dynamic_axes={"image": {2: "height", 3: "width"}, "output": {2: "height", 3: "width"}},
                              dynamo=False, do_constant_folding=False)
        os.replace(temporary, destination)
    finally:
        temporary.unlink(missing_ok=True)


def session(args):
    import onnxruntime as ort
    if args.gpu and hasattr(ort, "preload_dlls"):
        ort.preload_dlls()
    providers = ["CUDAExecutionProvider", "CPUExecutionProvider"] if args.gpu else ["CPUExecutionProvider"]
    options = ort.SessionOptions()
    options.intra_op_num_threads = min(os.cpu_count() or 1, 8)
    result = ort.InferenceSession(str(Path(args.root) / "models" / ("ArcaneGANv0.4.onnx" if args.style == "arcane" else MODELS[args.style][0])), sess_options=options, providers=providers)
    progress(f"{args.style}: {result.get_providers()[0]}")
    return Renderer(result, args.style)


class Renderer:
    def __init__(self, runtime, style):
        self.runtime = runtime
        self.style = style
        self.palette = None


def stylize(image, model):
    import numpy as np
    from PIL import Image, ImageChops, ImageFilter, ImageOps
    width, height = image.size
    rgb = np.asarray(image.convert("RGB"))
    # ArcaneGAN needs multiples of 32; 16 also accommodates AnimeGAN's tiny models.
    multiple = 32 if model.style == "arcane" else 16
    padded = np.pad(rgb, ((0, max(256, (height + multiple - 1) // multiple * multiple) - height),
                         (0, max(256, (width + multiple - 1) // multiple * multiple) - width), (0, 0)), mode="edge")
    if model.style == "arcane":
        means = np.array([0.485, 0.456, 0.406], dtype=np.float32)
        stds = np.array([0.229, 0.224, 0.225], dtype=np.float32)
        tensor = ((padded.astype(np.float32) / 255 - means) / stds).transpose(2, 0, 1)[None]
    else:
        tensor = padded.astype(np.float32)[None] / 127.5 - 1.0
    runtime = model.runtime
    # Exported ArcaneGAN is FP16; public AnimeGAN models use FP32.
    if runtime.get_inputs()[0].type == "tensor(float16)":
        tensor = tensor.astype(np.float16)
    output = runtime.run(None, {runtime.get_inputs()[0].name: tensor})[0][0]
    if model.style == "arcane":
        output = (output.transpose(1, 2, 0).astype(np.float32) * stds + means) * 255
    else:
        output = (output + 1) * 127.5
    result = Image.fromarray(np.clip(output[:height, :width], 0, 255).astype(np.uint8))
    if model.style == "8bit":
        small = result.resize((max(1, width // 4), max(1, height // 4)), Image.Resampling.BOX)
        if model.palette is None:
            model.palette = small.quantize(colors=32, dither=Image.Dither.NONE)
        small = small.quantize(palette=model.palette, dither=Image.Dither.NONE)
        result = small.convert("RGB").resize((width, height), Image.Resampling.NEAREST)
    elif model.style == "comic":
        edges = result.convert("L").filter(ImageFilter.GaussianBlur(0.6)).filter(ImageFilter.FIND_EDGES)
        # Remove FIND_EDGES' artificial outer frame before applying the ink layer.
        edges.paste(0, (0, 0, width, 1))
        edges.paste(0, (0, height - 1, width, height))
        edges.paste(0, (0, 0, 1, height))
        edges.paste(0, (width - 1, 0, width, height))
        ink = edges.point(lambda p: max(40, 255 - max(0, p - 15) * 5)).convert("RGB")
        result = ImageChops.multiply(ImageOps.posterize(result, 4), ink)
    if "A" in image.getbands() or "transparency" in image.info:
        result.putalpha(image.convert("RGBA").getchannel("A"))
    return result


def completion(count, elapsed, frame_duration, total_frames, total_seconds):
    """Rendering occupies 95% of the job; encoding/audio/publication finish the rest.

    Timestamp-based estimates handle variable-rate footage. Counts are a fallback
    for files whose container does not declare its duration.
    """
    if total_seconds and total_seconds > 0:
        fraction = max(0, elapsed + frame_duration) / total_seconds
    elif total_frames and total_frames > 0:
        fraction = count / total_frames
    else:
        return None
    return min(95, max(0, round(fraction * 95)))


def convert(args):
    from PIL import Image, ImageOps
    import av
    from fractions import Fraction
    source, destination = Path(args.input), Path(args.output)
    video = args.video
    suffix = destination.suffix.lower()
    allowed = (".mp4", ".webm", ".mkv", ".mov") if video else (".png", ".jpg", ".jpeg", ".webp")
    if suffix not in allowed:
        raise RuntimeError(f"unsupported output format {suffix!r}; choose {', '.join(allowed)}")
    model = session(args)
    destination.parent.mkdir(parents=True, exist_ok=True)
    # Work beside destination so the final rename is atomic, even across filesystems.
    with tempfile.TemporaryDirectory(prefix=".anime-style-", dir=destination.parent) as work:
        work = Path(work)
        ready = work / ("result" + suffix)
        if not video:
            progress("Rendering image: 0% complete…")
            with Image.open(source) as image:
                image = ImageOps.exif_transpose(image)
                result = stylize(image, model)
                if suffix in (".jpg", ".jpeg"):
                    result = result.convert("RGB")
                progress("Saving image: 95% complete…")
                result.save(ready)
        else:
            silent = work / "silent.mkv"
            with av.open(str(source)) as original, av.open(str(silent), "w") as encoded:
                stream = original.streams.video[0]
                rotation = int(float(stream.metadata.get("rotate", "0"))) % 360
                output = encoded.add_stream("libx264", rate=stream.average_rate or 25)
                output.pix_fmt = "yuv420p"
                output.options = {"crf": "18", "preset": "fast"}
                clock = Fraction(1, 90000)
                output.time_base = clock
                output.codec_context.time_base = clock
                durations = {}

                def mux(packet):
                    if packet.pts is not None:
                        key = round(packet.pts * packet.time_base / clock)
                        if key in durations:
                            packet.duration = max(1, round(durations.pop(key) / packet.time_base))
                    encoded.mux(packet)

                count = 0
                first_time = None
                total_frames = stream.frames or None
                total_seconds = (float(stream.duration * stream.time_base) if stream.duration is not None
                                 else float(original.duration / av.time_base) if original.duration is not None else None)
                last_progress = 0.0
                progress("Rendering video: ~0% complete…")
                for frame in original.decode(stream):
                    image = frame.to_image()
                    # Newer FFmpeg exposes display-matrix rotation on the frame.
                    angle = getattr(frame, "rotation", 0) or rotation
                    if angle:
                        image = image.rotate(angle, expand=True)
                    image = stylize(image, model)
                    if count == 0:
                        output.width = image.width + image.width % 2
                        output.height = image.height + image.height % 2
                    if image.size != (output.width, output.height):
                        padded = Image.new("RGB", (output.width, output.height))
                        padded.paste(image)
                        image = padded
                    styled = av.VideoFrame.from_image(image)
                    timestamp = frame.pts * frame.time_base if frame.pts is not None else Fraction(count, 1) / (stream.average_rate or 25)
                    if first_time is None:
                        first_time = timestamp
                    styled.pts = round((timestamp - first_time) / clock)
                    styled.time_base = clock
                    if frame.duration:
                        styled.duration = max(1, round(frame.duration * frame.time_base / clock))
                        durations[styled.pts] = frame.duration * frame.time_base
                    for packet in output.encode(styled):
                        mux(packet)
                    count += 1
                    now = time.monotonic()
                    if count == 1 or now - last_progress >= 1:
                        elapsed = float(timestamp - first_time)
                        frame_seconds = float(frame.duration * frame.time_base) if frame.duration else 0
                        percent = completion(count, elapsed, frame_seconds, total_frames, total_seconds)
                        estimate = f"~{percent}% complete; " if percent is not None else ""
                        frames = f"{count}/{total_frames}" if total_frames else str(count)
                        progress(f"Rendering video: {estimate}{frames} frames ({elapsed:.1f}s)…")
                        last_progress = now
                if count == 0:
                    raise RuntimeError("input contains no decodable video frames")
                for packet in output.encode():
                    mux(packet)
            progress("Encoding video and restoring audio: ~95% complete…")
            codec = ["-c:v", "libvpx-vp9", "-crf", "24", "-b:v", "0", "-c:a", "libopus"] if suffix == ".webm" else ["-c:v", "copy", "-c:a", "aac"]
            run(["ffmpeg", "-hide_banner", "-loglevel", "error", "-y", "-i", silent, "-i", source,
                 "-map", "0:v:0", "-map", "1:a:0?", *codec, *( ["-movflags", "+faststart"] if suffix in (".mp4", ".mov") else []), ready])
            progress(f"Styled {count} frames; original audio restored.")
        # Atomic publication without overwriting another job's output.
        os.link(ready, destination)
        ready.unlink()
    if video and not destination.with_suffix(".jpg").exists():
        try:
            run(["ffmpeg", "-hide_banner", "-loglevel", "error", "-y", "-i", destination, "-frames:v", "1", destination.with_suffix(".jpg")])
        except RuntimeError as error:
            progress(f"Video saved, but poster creation failed: {error}")

    progress("anime-style: 100% complete.")


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--root", required=True)
    parser.add_argument("--style", choices=MODELS, default="hayao")
    parser.add_argument("--gpu", action="store_true")
    parser.add_argument("--ready", action="store_true")
    parser.add_argument("--video", action="store_true")
    parser.add_argument("--check", action="store_true")
    parser.add_argument("--export-arcane", action="store_true")
    parser.add_argument("--input")
    parser.add_argument("--output")
    args = parser.parse_args()
    if args.export_arcane:
        export_arcane(args.root)
    elif args.check:
        from PIL import Image
        stylize(Image.new("RGB", (256, 256)), session(args))
    elif not args.ready:
        python, bridge = install(args)
        os.execv(str(python), [str(python), str(bridge), *sys.argv[1:], "--ready"])
    else:
        convert(args)


if __name__ == "__main__":
    def interrupted(signum, frame):
        raise KeyboardInterrupt
    signal.signal(signal.SIGTERM, interrupted)
    try:
        main()
    except KeyboardInterrupt:
        sys.exit(130)
    except Exception as error:
        print(f"anime-style: {error}", file=sys.stderr, flush=True)
        sys.exit(1)
