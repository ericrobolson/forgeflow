"""Download recovery checks; no Torch, network, or GPU required."""
import hashlib
import importlib.util
import io
import json
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location("lora_train", Path(__file__).parents[1] / "assets/lora_train.py")
backend = importlib.util.module_from_spec(spec)
spec.loader.exec_module(backend)


class Response(io.BytesIO):
    def __init__(self, content, status=200):
        super().__init__(content)
        self.status = status


class Downloads(unittest.TestCase):
    def test_resumes_partial_components_and_reuses_a_complete_install_offline(self):
        files = {f"{folder}/weights.safetensors": (folder + " checkpoint").encode()
                 for folder in ("tokenizer", "vae", "text_encoder")}
        requests = []

        def fetch(request, **kwargs):
            if isinstance(request, str):
                folder = request.rsplit("/", 1)[-1]
                entries = [{"type": "file", "path": name, "size": len(data),
                            "lfs": {"oid": hashlib.sha256(data).hexdigest()}}
                           for name, data in files.items() if name.startswith(folder + "/")]
                return Response(json.dumps(entries).encode())
            name = request.full_url.split(backend.QWEN_REVISION + "/", 1)[1]
            start = int(request.headers.get("Range", "bytes=0-")[6:-1])
            requests.append((name, start))
            return Response(files[name][start:], 206 if start else 200)

        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            partial = root / "extras/text_encoder/weights.safetensors.partial"
            partial.parent.mkdir(parents=True)
            partial.write_bytes(files["text_encoder/weights.safetensors"][:4])
            with patch("urllib.request.urlopen", side_effect=fetch):
                target = backend.extras(root)
            self.assertIn(("text_encoder/weights.safetensors", 4), requests)
            self.assertFalse(partial.exists())
            for name, data in files.items():
                self.assertEqual((target / name).read_bytes(), data)
            with patch("urllib.request.urlopen", side_effect=AssertionError("offline")):
                self.assertEqual(backend.extras(root), target)

    def test_damaged_download_is_not_installed(self):
        def fetch(request, **kwargs):
            if isinstance(request, str):
                return Response(json.dumps([{"type": "file", "path": "tokenizer/test.safetensors",
                                             "size": 4, "lfs": {"oid": hashlib.sha256(b"good").hexdigest()}}]).encode())
            return Response(b"oops")
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            with patch("urllib.request.urlopen", side_effect=fetch):
                with self.assertRaisesRegex(RuntimeError, "Checksum mismatch"):
                    backend.extras(root)
            self.assertFalse((root / "extras/tokenizer/test.safetensors").exists())
            self.assertFalse((root / "extras/tokenizer/test.safetensors.partial").exists())


@unittest.skipUnless(os.environ.get("FORGEFLOW_TORCH_TESTS") == "1", "requires the managed CUDA trainer environment")
class QuantizedEncoder(unittest.TestCase):
    def test_frozen_shared_weight_views_can_move_without_changing_outputs(self):
        import torch
        from optimum.quanto import quantize, freeze, qfloat8
        spec = importlib.util.spec_from_file_location("lora_worker", Path(__file__).parents[1] / "assets/lora_worker.py")
        worker = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(worker)
        model = torch.nn.Sequential(torch.nn.Linear(32, 32)).to(dtype=torch.bfloat16).eval()
        quantize(model, weights=qfloat8)
        freeze(model)
        model.requires_grad_(False)
        held_view = model[0].weight.view(32, 32)
        inputs = torch.randn(2, 32, dtype=torch.bfloat16)
        expected = model(inputs).detach()
        flag = torch.__future__.get_swap_module_params_on_conversion()
        try:
            torch.__future__.set_swap_module_params_on_conversion(True)
            for device in ("cuda:0", "cpu", "cuda:0", "cpu"):
                worker.frozen_to(torch.nn.Module.to, model, device)
                torch.testing.assert_close(model(inputs.to(device)).detach().cpu(), expected, rtol=.03, atol=.03)
                self.assertTrue(torch.__future__.get_swap_module_params_on_conversion())
            self.assertEqual(held_view.shape, (32, 32))
        finally:
            torch.__future__.set_swap_module_params_on_conversion(flag)


if __name__ == "__main__":
    unittest.main()
