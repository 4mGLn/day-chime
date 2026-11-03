import importlib.util
from pathlib import Path
import tempfile
import unittest

spec = importlib.util.spec_from_file_location("native_host_manifest", Path(__file__).with_name("native_host_manifest.py"))
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)


class NativeHostManifestTests(unittest.TestCase):
    def test_exact_origin_and_absolute_executable(self):
        with tempfile.TemporaryDirectory() as directory:
            binary = Path(directory) / "day-chime-browser-host"
            binary.write_text("test fixture", encoding="utf-8")
            binary.chmod(0o700)
            data = module.manifest(binary, "a" * 32)
            self.assertEqual(data["path"], str(binary))
            self.assertEqual(data["allowed_origins"], [f"chrome-extension://{'a' * 32}/"])

    def test_rejects_invalid_id_and_relative_or_missing_path(self):
        for binary, extension_id in [(Path("host"), "a" * 32), (Path("/missing-day-chime-host"), "a" * 32), (Path("/tmp"), "*"), (Path("/tmp"), "q" * 32)]:
            with self.assertRaises(ValueError):
                module.manifest(binary, extension_id)


if __name__ == "__main__":
    unittest.main()
