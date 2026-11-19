"""No real services/accounts/processes: packaging and pre-execution safety fixtures."""
import importlib.util
import io
import gzip
import json
import os
import subprocess
from pathlib import Path
import struct
import tarfile
import tempfile
import unittest
from unittest.mock import patch, Mock

spec = importlib.util.spec_from_file_location("node_package", Path(__file__).with_name("node-package.py"))
package = importlib.util.module_from_spec(spec)
spec.loader.exec_module(package)


def elf():
    data = bytearray(64)
    data[:6] = b"\x7fELF\x02\x01"
    data[18:20] = struct.pack("<H", 62)
    return bytes(data)


class Packages(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="atrium-package-tests-")
        self.addCleanup(self.temp.cleanup)
        # Even a hypothetical broken extractor's ../escape stays inside our
        # owned outer fixture, never colliding with an unrelated /tmp file.
        self.root = Path(self.temp.name) / "inner"
        self.root.mkdir()
        self.meta = {"version": "fixture", "target": "x86_64-unknown-linux-gnu"}

    def archive(self, name="node.tar.gz"):
        path = self.root / name
        package.make_archive(elf(), self.meta, path)
        return path

    def rewrite(self, path, transform):
        with tarfile.open(path, "r:gz") as source:
            entries = [(member.name, source.extractfile(member).read()) for member in source]
        path.unlink()  # owned test fixture, never a caller's output
        with tarfile.open(path, "w:gz") as output:
            for name, data in transform(entries):
                info = tarfile.TarInfo(name); info.size = len(data)
                output.addfile(info, io.BytesIO(data))

    def test_byte_reproducibility_and_manifest_hash(self):
        one, two = self.archive(), self.archive("second.tar.gz")
        self.assertEqual(one.read_bytes(), two.read_bytes())
        manifest, binary, hashes = package.verify(one)
        self.assertEqual(binary, elf())
        self.assertEqual(hashes["archive_sha256"], package.sha(one.read_bytes()))
        self.assertEqual(hashes["manifest_sha256"], package.sha(package.canonical(manifest)))
        with tarfile.open(one) as source:
            self.assertEqual({m.name for m in source}, package.MEMBERS)

    def test_collision_and_symlink_never_overwritten_or_removed(self):
        path = self.archive(); original = path.read_bytes()
        with self.assertRaises(FileExistsError):
            package.make_archive(elf(), self.meta, path)
        self.assertEqual(path.read_bytes(), original)
        link = self.root / "link"; link.symlink_to(path)
        with self.assertRaises(FileExistsError):
            package.make_archive(elf(), self.meta, link)
        self.assertTrue(link.is_symlink())
        with self.assertRaises(ValueError):
            package.verify(link)

    def test_binary_tampering_and_expected_archive_hash_are_rejected(self):
        path = self.archive()
        with self.assertRaises(ValueError):
            package.verify(path, "0" * 64)
        self.rewrite(path, lambda entries: [(name, data + b"changed" if name == package.BINARY else data) for name, data in entries])
        with self.assertRaises(ValueError):
            package.verify(path)

    def test_unexpected_path_duplicate_and_symlink_members_are_rejected(self):
        for name in ["../escape", "node.toml", "target/debug/mock-codex", "manifest.json"]:
            with self.subTest(name=name):
                path = self.archive("bad.tar.gz")
                self.rewrite(path, lambda entries: entries + [(name, b"not allowed")])
                with self.assertRaises(ValueError): package.verify(path)
                path.unlink()
        path = self.root / "symlink.tar.gz"
        with tarfile.open(path, "w:gz") as output:
            info = tarfile.TarInfo(package.BINARY); info.type = tarfile.SYMTYPE; info.linkname = "/bin/sh"
            output.addfile(info)
        with self.assertRaises(ValueError): package.verify(path)
        self.assertFalse((self.root.parent / "escape").exists())

    def test_target_mismatch_and_smoke_preflight_never_execute(self):
        path = self.root / "wrong-target.tar.gz"
        package.make_archive(elf(), dict(self.meta, target="aarch64-apple-darwin"), path)
        with patch.object(package.subprocess, "Popen") as spawn:
            with self.assertRaises(ValueError): package.smoke(path)
            spawn.assert_not_called()
        path = self.archive()
        with patch.object(package, "host_identity", return_value={"os": "Darwin", "arch": "aarch64"}), patch.object(package.subprocess, "Popen") as spawn:
            with self.assertRaises(ValueError): package.smoke(path)
            spawn.assert_not_called()

    def test_oversized_member_header_is_rejected_before_reading_body(self):
        path = self.root / "large-header.tar.gz"
        with tarfile.open(path, "w:gz") as output:
            member = tarfile.TarInfo(package.BINARY)
            member.size = package.MAX_BINARY + 1
            output.addfile(member)
        with self.assertRaises(ValueError): package.verify(path)

    def test_extension_headers_rejected_before_payload_parser(self):
        valid = self.archive()
        with tarfile.open(valid) as source:
            entries = [(item.name, source.extractfile(item).read()) for item in source]
        for kind in [tarfile.XHDTYPE, tarfile.XGLTYPE, tarfile.GNUTYPE_LONGNAME, tarfile.GNUTYPE_LONGLINK]:
            with self.subTest(kind=kind):
                header = tarfile.TarInfo("extension")
                header.type, header.size = kind, 128 * 1024
                raw = header.tobuf() + b"x" * header.size
                path = self.root / "extension.tar.gz"
                path.write_bytes(gzip.compress(raw))
                with patch.object(package.tarfile.TarInfo, "frombuf", side_effect=AssertionError("extension parser reached")):
                    with self.assertRaises(ValueError): package.verify(path)
        # Reproduce a fully valid PAX archive with large comment and both members.
        path = self.root / "pax.tar.gz"
        with tarfile.open(path, "w:gz", format=tarfile.PAX_FORMAT, pax_headers={"comment": "x" * 131072}) as target:
            for name, data in entries:
                item = tarfile.TarInfo(name); item.size = len(data)
                target.addfile(item, io.BytesIO(data))
        with self.assertRaises(ValueError): package.verify(path)

    def test_trailing_expansion_rejected_with_bounded_tail(self):
        path = self.archive()
        path.write_bytes(path.read_bytes() + gzip.compress(bytes(131072)))
        with self.assertRaises(ValueError): package.verify(path)

    def test_descriptor_reads_reject_fifo_and_bound_growth(self):
        fifo = self.root / "fifo"; os.mkfifo(fifo)
        with self.assertRaises(ValueError): package.read_regular_bounded(fifo, 16)
        path = self.root / "growing"; path.write_bytes(b"1234")
        real_fstat = os.fstat
        def grow(fd):
            info = real_fstat(fd)
            path.write_bytes(b"x" * 32)
            return info
        with patch.object(package.os, "fstat", side_effect=grow):
            with self.assertRaises(ValueError): package.read_regular_bounded(path, 16)
        # Replacement after open cannot redirect the already opened descriptor.
        path.write_bytes(b"original")
        replacement = self.root / "replacement"; replacement.write_bytes(b"foreign")
        def replace(fd):
            info = real_fstat(fd)
            path.unlink(); path.symlink_to(replacement)
            return info
        with patch.object(package.os, "fstat", side_effect=replace):
            self.assertEqual(package.read_regular_bounded(path, 16), b"original")
        self.assertEqual(replacement.read_bytes(), b"foreign")

    def test_failed_build_does_not_unlink_replacement(self):
        path = self.root / "failed.tar.gz"
        def replace_output(**kwargs):
            path.unlink()
            path.write_bytes(b"replacement")
            raise RuntimeError("injected compression failure")
        with patch.object(package.gzip, "GzipFile", side_effect=replace_output):
            with self.assertRaises(RuntimeError):
                package.make_archive(elf(), self.meta, path)
        self.assertEqual(path.read_bytes(), b"replacement")

    def test_partial_startup_announcement_has_deadline(self):
        read_fd, write_fd = os.pipe()
        with os.fdopen(read_fd, "rb", buffering=0) as reader, os.fdopen(write_fd, "wb", buffering=0) as writer:
            writer.write(b"listening on partial")
            with self.assertRaises(TimeoutError):
                package.startup_line(reader, timeout=0.02)

    def test_cleanup_only_uses_owned_handle_and_failure_is_not_success(self):
        child = Mock()
        child.poll.return_value = None
        package.stop_owned(child)
        child.terminate.assert_called_once_with()
        child.wait.assert_called_once_with(timeout=12)
        child.kill.assert_not_called()
        failed = Mock()
        failed.poll.return_value = None
        failed.wait.side_effect = [subprocess.TimeoutExpired("owned-node", 12), -9]
        with self.assertRaises(TimeoutError): package.stop_owned(failed)
        failed.kill.assert_called_once_with()
        self.assertEqual(failed.wait.call_count, 2)


if __name__ == "__main__":
    unittest.main()
