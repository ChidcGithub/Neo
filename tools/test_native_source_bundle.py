import io
import json
from pathlib import Path
import stat
import tarfile
import tempfile
import unittest
from unittest.mock import patch
import zipfile

if __package__:
    from . import native_source_bundle as bundle
else:
    import native_source_bundle as bundle


class NativeSourceBundleTests(unittest.TestCase):
    def tar(self, members):
        stream = io.BytesIO()
        with tarfile.open(fileobj=stream, mode="w:gz") as archive:
            for name, content, kind in members:
                info = tarfile.TarInfo(name)
                info.type = kind
                info.size = len(content) if kind == tarfile.REGTYPE else 0
                info.linkname = "outside"
                archive.addfile(info, io.BytesIO(content) if info.size else None)
        return stream.getvalue()

    def inspect(self, data, **kwargs):
        return bundle.archive_inventory(data, "source.tar.gz", bundle.sha(data), "eigen", **kwargs)

    def test_complete_tar_inventory(self):
        data = self.tar([("eigen/a.h", b"source", tarfile.REGTYPE)])
        self.assertEqual(self.inspect(data), {"a.h": {"size": 6, "sha256": bundle.sha(b"source")}})

    def test_hash_mismatch(self):
        with self.assertRaisesRegex(ValueError, "hash"):
            bundle.archive_inventory(b"wrong", "a.tar.gz", "0" * 64, "eigen")

    def test_unsafe_names(self):
        for name in ("/abs", "../evil", "eigen/../evil", "C:/evil", "eigen\\evil", "eigen//a", "eigen/./a", "eigen/\na"):
            with self.subTest(name=name), self.assertRaises(ValueError):
                bundle.safe_name(name)

    def test_links_and_devices_rejected(self):
        for kind in (tarfile.SYMTYPE, tarfile.LNKTYPE, tarfile.CHRTYPE, tarfile.FIFOTYPE):
            with self.subTest(kind=kind), self.assertRaises(ValueError):
                self.inspect(self.tar([("eigen/link", b"", kind)]))

    def test_duplicate_casefolded_members_rejected(self):
        with self.assertRaises(ValueError):
            self.inspect(self.tar([("eigen/A", b"a", tarfile.REGTYPE), ("eigen/a", b"b", tarfile.REGTYPE)]))

    def test_wrong_root(self):
        with self.assertRaises(ValueError):
            self.inspect(self.tar([("other/a", b"a", tarfile.REGTYPE)]))

    def test_expansion_budget(self):
        data = self.tar([("eigen/a", b"a" * 10000, tarfile.REGTYPE)])
        with self.assertRaisesRegex(ValueError, "Expanded"):
            self.inspect(data, max_bytes=1000)

    def test_zip_symlink(self):
        stream = io.BytesIO()
        with zipfile.ZipFile(stream, "w") as archive:
            info = zipfile.ZipInfo("eigen/a")
            info.external_attr = (stat.S_IFLNK | 0o777) << 16
            archive.writestr(info, "outside")
        data = stream.getvalue()
        with self.assertRaises(ValueError):
            bundle.archive_inventory(data, "a.zip", bundle.sha(data), "eigen")

    def test_tree_modifications_and_extras(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            path = root / "a"
            path.write_bytes(b"a")
            inventory = {"a": {"size": 1, "sha256": bundle.sha(b"a")}}
            bundle.compare_tree(root, inventory)
            path.write_bytes(b"b")
            with self.assertRaises(ValueError):
                bundle.compare_tree(root, inventory)
            path.write_bytes(b"a")
            (root / "extra").write_bytes(b"x")
            with self.assertRaises(ValueError):
                bundle.compare_tree(root, inventory)

    def test_notice_hash_and_no_recursive_copy(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "LICENSE").write_bytes(b"MIT")
            (root / "private.json").write_text("do not publish")
            (root / "index.json").write_text(json.dumps({"texts": {"LICENSE": {"size": 3, "sha256": bundle.sha(b"MIT")}}}))
            self.assertEqual(set(bundle.public_texts(root)), {"notices/LICENSE", "notices/index.json"})
            (root / "LICENSE").write_bytes(b"changed")
            with self.assertRaisesRegex(ValueError, "changed"):
                bundle.public_texts(root)

    def test_deterministic_no_overwrite(self):
        with tempfile.TemporaryDirectory() as directory:
            a, b = Path(directory) / "a.zip", Path(directory) / "b.zip"
            bundle.write_bundle(a, {"notice.txt": b"notice"})
            bundle.write_bundle(b, {"notice.txt": b"notice"})
            self.assertEqual(a.read_bytes(), b.read_bytes())
            with self.assertRaises(ValueError):
                bundle.write_bundle(a, {})

    def test_verify_rejects_unlisted_content(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "bundle.zip"
            bundle.write_bundle(path, {"private-evidence.json": b"private"})
            with patch.object(bundle, "public_texts", return_value={}):
                with self.assertRaisesRegex(ValueError, "unexpected"):
                    bundle.verify(path)

    def test_verify_rejects_tampering(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "bundle.zip"
            payload = {"sources/" + name: b"candidate" for name in bundle.SOURCES}
            payload["source-records.json"] = b"{}"
            payload["SHA256SUMS"] = b"wrong"
            with zipfile.ZipFile(path, "w") as archive:
                for name, data in payload.items():
                    archive.writestr(name, data)
            with patch.object(bundle, "public_texts", return_value={}):
                with self.assertRaisesRegex(ValueError, "checksum"):
                    bundle.verify(path)

    def test_real_public_texts(self):
        texts = bundle.public_texts()
        self.assertIn("notices/eigen-5.0.1-COPYING.MPL2", texts)
        self.assertIn("notices/onnxruntime-1.28.2-LICENSE", texts)
        self.assertIn("notices/ort-1.28.2-eigen-s390x-build.patch", texts)
        self.assertNotIn("notices/onnxruntime-1.30.0-LICENSE", texts)


if __name__ == "__main__":
    unittest.main()
