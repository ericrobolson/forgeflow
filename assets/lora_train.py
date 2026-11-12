"""Managed, pinned Qwen LoRA training backend. Invoked by src/lora.rs."""
import json
import os
from pathlib import Path
import subprocess
import sys
import shutil
import signal

TOOLKIT_REVISION = "4a7f28532ed6fe81d9f8c00718878bb3b8739015"
QWEN_REVISION = "75e0b4be04f60ec59a75f475837eced720f823b6"


def extras(root):
    """Fetch only the text encoder, tokenizer and VAE, never a second DiT."""
    import hashlib
    import urllib.request
    target = root / "extras"
    manifest = root / "extras-manifest.json"
    cached = json.loads(manifest.read_text()) if manifest.exists() else None
    if cached and cached.get("revision") == QWEN_REVISION:
        if all((target / entry["path"]).is_file() and (target / entry["path"]).stat().st_size == entry["size"] for entry in cached["files"]):
            return target
    all_entries = []
    for folder in ("tokenizer", "vae", "text_encoder"):
        url = f"https://huggingface.co/api/models/Qwen/Qwen-Image/tree/{QWEN_REVISION}/{folder}"
        entries = json.load(urllib.request.urlopen(url, timeout=60))
        all_entries.extend(entry for entry in entries if entry["type"] == "file")
        for entry in entries:
            if entry["type"] != "file":
                continue
            relative = entry["path"]
            if relative.split("/")[0] != folder or ".." in Path(relative).parts:
                raise RuntimeError("invalid model repository path")
            path = target / relative
            if path.exists() and path.stat().st_size == entry["size"]:
                continue
            path.parent.mkdir(parents=True, exist_ok=True)
            partial = path.with_suffix(path.suffix + ".partial")
            print(f"Downloading Qwen training component: {relative} ({entry['size'] / 1024**3:.2f} GiB)", flush=True)
            url = f"https://huggingface.co/Qwen/Qwen-Image/resolve/{QWEN_REVISION}/{relative}"
            # Interrupted installations keep partial downloads and resume them.
            offset = partial.stat().st_size if partial.exists() else 0
            request = urllib.request.Request(url, headers={"Range": f"bytes={offset}-"} if offset else {})
            with urllib.request.urlopen(request, timeout=120) as response:
                append = offset > 0 and response.status == 206
                with partial.open("ab" if append else "wb") as stream:
                    shutil.copyfileobj(response, stream, 1024 * 1024)
            if partial.stat().st_size != entry["size"]:
                raise RuntimeError(f"Incomplete download of {relative}; retry to resume")
            expected = entry.get("lfs", {}).get("oid")
            if expected:
                with partial.open("rb") as stream:
                    digest = hashlib.file_digest(stream, "sha256").hexdigest()
                if digest != expected:
                    partial.unlink()
                    raise RuntimeError(f"Checksum mismatch for {relative}")
            partial.replace(path)
    manifest.write_text(json.dumps({"revision": QWEN_REVISION, "files": all_entries}, indent=2))
    return target


def run(args, **kwargs):
    print("Running:", " ".join(map(str, args)), flush=True)
    subprocess.run(list(map(str, args)), check=True, **kwargs)


def install(root):
    import fcntl
    root.mkdir(parents=True, exist_ok=True)
    with (root / ".install.lock").open("w") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        repo = root.parent / "ai-toolkit"
        if not (repo / ".git").exists():
            run(["git", "clone", "--no-checkout", "https://github.com/ostris/ai-toolkit.git", repo])
        current = subprocess.check_output(["git", "-C", str(repo), "rev-parse", "HEAD"], text=True).strip()
        if current != TOOLKIT_REVISION:
            dirty = subprocess.check_output(["git", "-C", str(repo), "status", "--porcelain"], text=True)
            if dirty.strip():
                raise RuntimeError(f"Trainer has local changes at {repo}; refusing to replace them")
            run(["git", "-C", repo, "fetch", "origin", TOOLKIT_REVISION])
            run(["git", "-C", repo, "checkout", "--detach", TOOLKIT_REVISION])
        python = repo / ".venv/bin/python"
        if not python.exists():
            run([sys.executable, "-m", "venv", repo / ".venv"])
        stamp = root / "installed.json"
        expected = {"revision": TOOLKIT_REVISION, "recipe": 1}
        if stamp.exists() and json.loads(stamp.read_text()) == expected:
            run([python, "-c", "import torch, diffusers, transformers; assert torch.cuda.is_available(), 'CUDA unavailable'"])
            return repo, python
        # Keep binary versions aligned with the pinned toolkit's own installation guide.
        run([python, "-m", "pip", "install", "--no-cache-dir", "torch==2.13.0", "torchvision==0.28.0", "torchaudio==2.11.0", "--index-url", "https://download.pytorch.org/whl/cu130"])
        run([python, "-m", "pip", "install", "--no-cache-dir", "-r", repo / "requirements.txt"])
        run([python, "-c", "import torch, diffusers, transformers; assert torch.cuda.is_available(), 'CUDA unavailable'"])
        stamp.write_text(json.dumps(expected))
        return repo, python


def main():
    if sys.platform != "linux":
        raise RuntimeError("Managed Qwen LoRA training currently requires Linux with an NVIDIA CUDA GPU")
    config_path = Path(sys.argv[1]).resolve()
    models = Path(sys.argv[2]).resolve()
    managed = models / "lora-training"
    repo, python = install(managed)
    import fcntl
    training_lock = (managed / ".training.lock").open("w")
    try:
        fcntl.flock(training_lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
    except BlockingIOError:
        raise RuntimeError("Another ForgeFlow LoRA training job is active")
    config = json.loads(config_path.read_text())
    process = config["config"]["process"][0]
    weights = models / "qwen-image/qwen_image_bf16.safetensors"
    if not weights.is_file():
        raise RuntimeError("Qwen training weights are missing; run forgeflow pull qwen-image")
    process["model"]["name_or_path"] = str(weights)
    process["model"]["extras_name_or_path"] = str(extras(managed))
    runtime = config_path.with_name("trainer-runtime.json")
    runtime.write_text(json.dumps(config, indent=2))
    env = dict(os.environ, PYTHONUNBUFFERED="1", TOKENIZERS_PARALLELISM="false",
               HF_HOME=str(managed / "huggingface"), MODELS_PATH=str(managed / "models"),
               OMP_NUM_THREADS="4", MKL_NUM_THREADS="4")
    print("Starting local Qwen training; logs and checkpoints remain with this run.", flush=True)
    worker = config_path.with_name("worker.py")
    if not worker.is_file():
        raise RuntimeError("Training worker is missing from the run snapshot")
    args = [str(python), str(worker), str(repo), str(runtime)]
    os.chdir(repo)
    # Bound host memory as well as GPU memory: block offloading must not freeze
    # the desktop. On Linux desktops use a dedicated, cancellable user scope.
    unit = "forgeflow-lora-" + config_path.parent.name
    scoped = sys.platform == "linux" and shutil.which("systemd-run") and subprocess.run(
        ["systemctl", "--user", "show-environment"], stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL).returncode == 0
    if scoped:
        total_kib = int(next(line.split()[1] for line in Path("/proc/meminfo").read_text().splitlines() if line.startswith("MemTotal:")))
        limit = int(total_kib * 1024 * 0.75)
        args = ["systemd-run", "--user", "--scope", "--quiet", "--unit=" + unit,
                "-p", f"MemoryMax={limit}", "-p", "MemorySwapMax=536870912", *args]
        print(f"Training memory limit: {limit // 1024**3} GiB RAM, 512 MiB swap", flush=True)
    process = subprocess.Popen(args, env=env)

    def stop(signum, frame):
        if scoped:
            subprocess.run(["systemctl", "--user", "stop", unit + ".scope"], check=False)
        else:
            process.terminate()
        process.wait()
        raise SystemExit(128 + signum)

    signal.signal(signal.SIGTERM, stop)
    signal.signal(signal.SIGINT, stop)
    raise SystemExit(process.wait())


if __name__ == "__main__":
    main()
