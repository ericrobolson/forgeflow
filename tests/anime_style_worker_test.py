"""Installer recovery tests; no downloads, GPU, or installed Python packages required.
Run with: python3 -m unittest discover -s tests -p '*_test.py'
"""
import hashlib
import importlib.util
import io
from pathlib import Path
from types import SimpleNamespace
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location("anime_worker", Path(__file__).parents[1] / "assets/anime_style_bridge.py")
worker = importlib.util.module_from_spec(spec)
spec.loader.exec_module(worker)


class InstallationTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.args = SimpleNamespace(root=str(self.root), style="hayao", gpu=False)
        self.model = b"test model bytes"
        self.checksum = hashlib.sha256(self.model).hexdigest()
        self.patches = [
            patch.dict(worker.MODELS, {"hayao": ("model.onnx", self.checksum)}),
            patch.object(worker, "run", return_value=""),
            patch.object(worker.urllib.request, "urlopen", side_effect=lambda *a, **kw: io.BytesIO(self.model)),
            patch.object(worker, "progress"),
        ]
        self.models, self.run, self.download, _ = [p.start() for p in self.patches]
        for p in self.patches:
            self.addCleanup(p.stop)

    def test_healthy_second_install_is_offline_and_does_not_rewrite_marker(self):
        worker.install(self.args)
        marker = self.root / "installation.json"
        stamp = marker.stat().st_mtime_ns
        self.run.reset_mock()
        self.download.reset_mock()
        worker.install(self.args)
        self.download.assert_not_called()
        self.assertEqual(marker.stat().st_mtime_ns, stamp)
        self.assertFalse(any("pip" in [str(a) for a in call.args[0]] for call in self.run.call_args_list))

    def test_corrupt_model_is_repaired(self):
        worker.install(self.args)
        model = self.root / "models/model.onnx"
        model.write_bytes(b"corrupt")
        self.download.reset_mock()
        worker.install(self.args)
        self.download.assert_called_once()
        self.assertEqual(model.read_bytes(), self.model)

    def test_failed_download_leaves_no_success_marker_and_can_retry(self):
        class Interrupted(io.BytesIO):
            def read(self, *args):
                raise OSError("interrupted download")
        self.download.side_effect = lambda *a, **kw: Interrupted()
        with self.assertRaisesRegex(OSError, "interrupted"):
            worker.install(self.args)
        self.assertFalse((self.root / "installation.json").exists())
        self.assertFalse((self.root / "models/model.partial").exists())
        self.download.side_effect = lambda *a, **kw: io.BytesIO(self.model)
        worker.install(self.args)
        self.assertTrue((self.root / "installation.json").exists())

    def test_broken_environment_is_repaired_despite_existing_marker(self):
        worker.install(self.args)
        self.run.reset_mock()
        calls = 0
        def run(args):
            nonlocal calls
            calls += 1
            if calls == 1:
                raise RuntimeError("broken imports")
            return ""
        self.run.side_effect = run
        worker.install(self.args)
        self.assertTrue(any("pip" in [str(a) for a in call.args[0]] for call in self.run.call_args_list))


class ArcaneExportTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        (self.root / "models").mkdir()
        self.source = self.root / "models/ArcaneGANv0.4.jit"
        self.source.write_bytes(b"pinned source")
        self.model = self.source.with_suffix(".onnx")

    def test_converted_model_is_reused_and_corruption_triggers_rebuild(self):
        def run(args):
            if "--export-arcane" in args:
                self.model.write_bytes(b"converted ONNX")
            return ""
        with patch.object(worker, "run", side_effect=run) as command, patch.object(worker, "progress"):
            worker.ensure_arcane_export(self.root, "python", "bridge", self.source)
            command.reset_mock()
            worker.ensure_arcane_export(self.root, "python", "bridge", self.source)
            command.assert_not_called()
            self.model.write_bytes(b"damaged")
            worker.ensure_arcane_export(self.root, "python", "bridge", self.source)
            self.assertEqual(self.model.read_bytes(), b"converted ONNX")
            self.assertTrue(any("--export-arcane" in call.args[0] for call in command.call_args_list))

    def test_interrupted_conversion_is_not_marked_successful(self):
        def run(args):
            if "--export-arcane" in args:
                raise RuntimeError("conversion interrupted")
            return ""
        with patch.object(worker, "run", side_effect=run), patch.object(worker, "progress"):
            with self.assertRaisesRegex(RuntimeError, "interrupted"):
                worker.ensure_arcane_export(self.root, "python", "bridge", self.source)
        self.assertFalse((self.root / "arcane-export.json").exists())


class CompletionTests(unittest.TestCase):
    def test_timestamps_drive_variable_rate_estimate_and_counts_are_fallback(self):
        self.assertEqual(worker.completion(25, 1, 0, 100, 4), 24)
        self.assertEqual(worker.completion(50, 0, 0, 100, None), 48)
        self.assertEqual(worker.completion(100, 10, 0, 100, 4), 95)
        self.assertIsNone(worker.completion(10, 1, 0, None, None))


if __name__ == "__main__":
    unittest.main()
