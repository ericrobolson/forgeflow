"""ForgeFlow's bridge to FaceFusion, run with FaceFusion's own Python environment.

    faces <media> <every-seconds>   print JSON listing the faces in sampled frames, in the
                                    order FaceFusion's reference selector numbers them
    distance <media> <every-seconds> <frame> <position>
                                    print the loosest reference distance that still keeps
                                    every other person's face out, as JSON
    run <facefusion arguments...>   run FaceFusion's command line

Both first load the CUDA and cuDNN libraries that pip installed beside onnxruntime, which
FaceFusion does not do itself, so its models run on the GPU.
"""
import json
import os
import sys

import numpy
import onnxruntime

try:
    onnxruntime.preload_dlls()
except Exception:
    pass  # Older runtimes or CPU-only installs: FaceFusion falls back on its own.

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
# FaceFusion finds its processors relative to the working folder; callers pass absolute paths.
os.chdir(HERE)
VIDEO_EXTENSIONS = ('.mp4', '.webm', '.mov', '.mkv', '.avi', '.m4v')


def setup(media):
    """FaceFusion's settings at their defaults, as a real run would have them, with models ready."""
    from facefusion import face_classifier, face_detector, face_landmarker, face_recognizer, state_manager
    from facefusion.args import apply_args
    from facefusion.program import create_program

    program = create_program()
    args = vars(program.parse_args(['headless-run', '-t', media, '-o', os.devnull, '--execution-providers', 'cuda', '--log-level', 'error']))
    apply_args(args, state_manager.init_item)
    for module in (face_detector, face_landmarker, face_recognizer, face_classifier):
        if not module.pre_check():
            raise SystemExit('could not download FaceFusion face models')


def sampled_faces(media, every):
    """(frame number, seconds, frame, faces in selector order) for frames every `every` seconds."""
    from facefusion.face_creator import get_static_faces
    from facefusion.face_selector import sort_and_filter_faces
    from facefusion.vision import count_video_frame_total, detect_video_fps, read_static_image, read_video_frame

    if media.lower().endswith(VIDEO_EXTENSIONS):
        fps = detect_video_fps(media) or 25.0
        total = count_video_frame_total(media)
        step = max(1, round(fps * every))
        frames = ((n, n / fps, read_video_frame(media, n)) for n in (list(range(0, total, step)) or [0]))
    else:
        frames = [(0, 0.0, read_static_image(media))]
    for number, time, frame in frames:
        if frame is not None:
            yield number, time, frame, sort_and_filter_faces([], get_static_faces([frame]))


def distance(media, every, frame_number, position):
    """The chosen face's distance to faces that must be other people, and a threshold below it.

    In a frame showing several faces, the one nearest the chosen face is taken to be that person
    and the rest are others; the threshold sits safely under the nearest other, so turns and blur
    of the chosen person still match while nobody else does."""
    from facefusion.face_creator import get_one_face, get_static_faces
    from facefusion.face_selector import calculate_face_distance, sort_and_filter_faces
    from facefusion.vision import read_static_image, read_video_frame

    setup(media)
    is_video = media.lower().endswith(VIDEO_EXTENSIONS)
    frame = read_video_frame(media, frame_number) if is_video else read_static_image(media)
    reference = get_one_face(sort_and_filter_faces([], get_static_faces([frame])), position)
    if reference is None:
        raise SystemExit('no face at that position in that frame')
    def compared(face):
        # The scale --reference-face-distance is compared on: FaceFusion maps [0, 2] to [0, 1].
        return float(numpy.interp(calculate_face_distance(face, reference), [0, 2], [0, 1]))

    same, others = [], []
    for _, _, _, found in sampled_faces(media, every):
        distances = sorted(compared(face) for face in found)
        # A lone face could be anyone, so only frames showing several faces tell people apart.
        if len(found) >= 2:
            same.append(distances[0])
            others.extend(distances[1:])
    nearest_other = min(others) if others else None
    farthest_same = max(same) if same else None
    if nearest_other is None:
        threshold = 0.5
    elif farthest_same is not None and farthest_same < nearest_other:
        # Midway between the chosen person's hardest match and the nearest other person.
        threshold = (farthest_same + nearest_other) / 2
    else:
        threshold = nearest_other * 0.8
    # FaceFusion takes 0.05 steps; round down so the other person stays out. Its default is 0.3.
    threshold = max(0.3, min(0.5, int(threshold * 20) / 20))
    round3 = lambda v: None if v is None else round(v, 3)
    print(json.dumps({'distance': round(threshold, 2), 'nearest_other': round3(nearest_other), 'farthest_same': round3(farthest_same)}))


def faces(media, every):
    setup(media)
    samples = []
    for number, time, frame, found in sampled_faces(media, every):
        samples.append({
            'frame': number,
            'time': round(time, 3),
            'width': int(frame.shape[1]),
            'height': int(frame.shape[0]),
            'faces': [[round(float(v), 1) for v in face.bounding_box] for face in found],
        })
    print(json.dumps({'samples': samples}))


def run(arguments):
    from facefusion import core
    sys.argv = ['facefusion.py'] + arguments
    core.cli()


if __name__ == '__main__':
    command = sys.argv[1] if len(sys.argv) > 1 else ''
    if command == 'faces':
        faces(sys.argv[2], float(sys.argv[3]))
    elif command == 'distance':
        distance(sys.argv[2], float(sys.argv[3]), int(sys.argv[4]), int(sys.argv[5]))
    elif command == 'run':
        run(sys.argv[2:])
    else:
        raise SystemExit(__doc__)
