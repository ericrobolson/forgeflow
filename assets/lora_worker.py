"""Small-memory process wrapper around the pinned upstream trainer."""
import ctypes
import gc
from pathlib import Path
import runpy
import sys


def frozen_to(original, module, *args, **kwargs):
    """Move a frozen quantized encoder without PyTorch's parameter-swap path.

    Quanto's frozen tensor subclasses can have shared references; torch 2.13's
    swap_tensors checks then ask for a gradient edge on a non-gradient tensor.
    Replacing frozen encoder parameters is safe: no optimizer tracks them.
    Keep the normal conversion behavior for trainable modules.
    """
    import torch
    if any(parameter.requires_grad for parameter in module.parameters()):
        return original(module, *args, **kwargs)
    device, dtype, non_blocking, memory_format = torch._C._nn._parse_to(*args, **kwargs)
    moved = {}

    def convert(tensor, parameter=False):
        key = id(tensor)
        if key not in moved:
            options = dict(device=device, non_blocking=non_blocking)
            if dtype is not None and (tensor.is_floating_point() or tensor.is_complex()):
                options["dtype"] = dtype
            if memory_format is not None and tensor.dim() in (4, 5):
                options["memory_format"] = memory_format
            value = tensor.to(**options)
            if parameter:
                value = torch.nn.Parameter(value, requires_grad=False)
            moved[key] = value
        return moved[key]

    # Module._apply forces swap_tensors for traceable wrapper subclasses even
    # when its future flag is disabled. Replace frozen parameters explicitly,
    # preserving tied parameter/buffer identities via the conversion memo.
    with torch.no_grad():
        for child in module.modules():
            for name, value in list(child._parameters.items()):
                if value is not None:
                    child._parameters[name] = convert(value, parameter=True)
            for name, value in list(child._buffers.items()):
                if value is not None:
                    child._buffers[name] = convert(value)
    return module

def main():
    repo = Path(sys.argv[1]).resolve()
    config = Path(sys.argv[2]).resolve()
    sys.path.insert(0, str(repo))

    import toolkit.basic

    original_flush = toolkit.basic.flush
    try:
        trim = ctypes.CDLL(None).malloc_trim
        trim.argtypes = [ctypes.c_size_t]
        trim.restype = ctypes.c_int
    except AttributeError:
        trim = None


    def flush():
        original_flush()
        gc.collect()
        if trim is not None:
            trim(0)


    # Quantization and device changes free large host buffers. Return that unused
    # allocator memory to Linux before entering the next stage.
    toolkit.basic.flush = flush
    import toolkit.dataloader_mixins

    # The upstream cache batches 16 prompts, which is unnecessary for this small-GPU
    # recipe and increases peak memory independently of the training batch size.
    toolkit.dataloader_mixins.TEXT_EMBED_CACHE_BATCH = 1

    from toolkit.models.v2.text_encoders.qwen25_vl import Qwen25VLTextEncoder

    encoder_to = Qwen25VLTextEncoder.to
    Qwen25VLTextEncoder.to = lambda self, *args, **kwargs: frozen_to(encoder_to, self, *args, **kwargs)

    sys.argv = [str(repo / "run.py"), str(config)]
    runpy.run_path(str(repo / "run.py"), run_name="__main__")


if __name__ == "__main__":
    main()
