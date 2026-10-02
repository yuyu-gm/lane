"""Validate deferred application only against temporary local destinations."""
import ctypes
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import tomllib
import unittest

ROOT = Path(__file__).resolve().parents[1]
VERSION = tomllib.loads((ROOT / "Cargo.toml").read_text(encoding="utf-8"))["package"]["version"]
BUNDLE = ROOT / "dist" / f"lane-{VERSION}"


class PackageTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="lane-apply-")
        self.addCleanup(self.temp.cleanup)
        self.base = Path(self.temp.name)
        self.bundle = self.base / "bundle"
        shutil.copytree(BUNDLE, self.bundle, ignore=shutil.ignore_patterns("backups"))
        self.bin = self.base / "bin"
        self.guide = self.base / "guide"

    def invoke(self, *args, bundle=None, success=True):
        bundle = bundle or self.bundle
        r = subprocess.run(["powershell", "-NoProfile", "-File", str(bundle / "install.ps1"), "-InstallDir", str(self.bin), "-GuideDir", str(self.guide), *args], capture_output=True)
        if success:
            self.assertEqual(r.returncode, 0, (r.stdout, r.stderr))
        else:
            self.assertNotEqual(r.returncode, 0)
        return r

    def existing(self):
        self.bin.mkdir(exist_ok=True)
        self.guide.mkdir(exist_ok=True)
        (self.bin / "lane.exe").write_bytes(b"old executable")
        (self.guide / "LANE.md").write_bytes(b"old guide")
        (self.guide / "AGENTS.md").write_bytes(b"Existing instructions\n")

    def test_preview_creates_no_destinations(self):
        self.invoke()
        self.assertFalse(self.bin.exists())
        self.assertFalse(self.guide.exists())

    def test_payload_hashes_and_guide_copy(self):
        hashes = json.loads((BUNDLE / "SHA256SUMS.json").read_text(encoding="utf-8"))
        for name, expected in hashes.items():
            self.assertEqual(hashlib.sha256((BUNDLE / name).read_bytes()).hexdigest(), expected)
        self.assertEqual((BUNDLE / "LANE.md").read_bytes(), (ROOT / "docs/LANE.md").read_bytes())
        self.assertEqual((BUNDLE / "INSTALL.md").read_bytes(), (ROOT / "docs/installation.md").read_bytes())
        self.assertIn("LICENSE", hashes)
        self.assertEqual((BUNDLE / "LICENSE").read_bytes(), (ROOT / "LICENSE").read_bytes())

    def test_apply_replaces_only_explicit_temp_targets(self):
        self.existing()
        self.invoke("-Apply")
        self.assertEqual((self.bin / "lane.exe").read_bytes(), (BUNDLE / "lane.exe").read_bytes())
        self.assertEqual((self.guide / "LANE.md").read_bytes(), (BUNDLE / "LANE.md").read_bytes())
        agents = (self.guide / "AGENTS.md").read_text(encoding="utf-8")
        self.assertTrue(agents.startswith("Existing instructions\n"))
        self.assertIn("[LANE.md](LANE.md)", agents)
        r = subprocess.run([str(self.bin / "lane.exe"), "--version"], capture_output=True, check=True)
        self.assertEqual(json.loads(r.stdout)["data"]["version"], VERSION)

    def test_failed_guide_update_rolls_back_executable(self):
        self.bin.mkdir()
        (self.bin / "lane.exe").write_bytes(b"old executable")
        self.guide.write_bytes(b"directory creation must fail")
        self.invoke("-Apply", success=False)
        self.assertEqual((self.bin / "lane.exe").read_bytes(), b"old executable")
        self.assertEqual(self.guide.read_bytes(), b"directory creation must fail")

    def test_corrupted_bundle_refuses_all_writes(self):
        for name in ("LANE.md", "LICENSE"):
            with self.subTest(payload=name):
                copy = self.base / ("corrupt-" + name)
                shutil.copytree(BUNDLE, copy, ignore=shutil.ignore_patterns("backups"))
                (copy / name).write_text("corrupt")
                self.invoke("-Apply", bundle=copy, success=False)
                self.assertFalse(self.bin.exists())
                self.assertFalse(self.guide.exists())

    @unittest.skipUnless(os.name == "nt", "Windows executable lock")
    def test_locked_destination_is_preserved(self):
        self.existing()
        kernel = ctypes.WinDLL("kernel32", use_last_error=True)
        kernel.CreateFileW.argtypes = [ctypes.c_wchar_p, ctypes.c_uint32, ctypes.c_uint32, ctypes.c_void_p, ctypes.c_uint32, ctypes.c_uint32, ctypes.c_void_p]
        kernel.CreateFileW.restype = ctypes.c_void_p
        kernel.CloseHandle.argtypes = [ctypes.c_void_p]
        handle = kernel.CreateFileW(str(self.bin / "lane.exe"), 0x80000000, 1, None, 3, 0, None)
        self.assertNotEqual(handle, ctypes.c_void_p(-1).value)
        try:
            self.invoke("-Apply", success=False)
        finally:
            kernel.CloseHandle(handle)
        self.assertEqual((self.bin / "lane.exe").read_bytes(), b"old executable")
        self.assertEqual((self.guide / "LANE.md").read_bytes(), b"old guide")
        self.assertEqual((self.guide / "AGENTS.md").read_bytes(), b"Existing instructions\n")

    def test_missing_agents_is_created_beside_guide(self):
        self.invoke("-Apply")
        agents = (self.guide / "AGENTS.md").read_text(encoding="utf-8")
        self.assertIn("[LANE.md](LANE.md)", agents)
        self.assertTrue((self.guide / "LANE.md").exists())

    def test_agents_bom_crlf_and_surrounding_content_are_preserved(self):
        self.existing()
        path = self.guide / "AGENTS.md"
        original = "既存の指示\r\n".encode("utf-8-sig")
        path.write_bytes(original)
        self.invoke("-Apply")
        first = path.read_bytes()
        self.assertTrue(first.startswith(original))
        self.assertNotIn(b"\n", first.replace(b"\r\n", b""))
        self.invoke("-Apply")
        self.assertEqual(path.read_bytes(), first)
        text = first.decode("utf-8-sig").replace("## Lane", "## Old Lane")
        path.write_bytes((text + "後続の指示\r\n").encode("utf-8-sig"))
        self.invoke("-Apply")
        updated = path.read_bytes().decode("utf-8-sig")
        self.assertTrue(updated.startswith("既存の指示\r\n"))
        self.assertTrue(updated.endswith("後続の指示\r\n"))
        self.assertEqual(updated.count("lane-managed-reference:start"), 1)
        self.assertNotIn("## Old Lane", updated)

    def test_ambiguous_or_invalid_utf8_agents_refuse_all_writes(self):
        for content in (b"<!-- lane-managed-reference:start -->", b"\xff"):
            with self.subTest(content=content):
                self.existing()
                path = self.guide / "AGENTS.md"
                path.write_bytes(content)
                self.invoke("-Apply", success=False)
                self.assertEqual(path.read_bytes(), content)
                self.assertEqual((self.bin / "lane.exe").read_bytes(), b"old executable")
                self.assertEqual((self.guide / "LANE.md").read_bytes(), b"old guide")

    @unittest.skipUnless(os.name == "nt", "Windows instructions lock")
    def test_locked_agents_rolls_back_executable_and_guide(self):
        self.existing()
        kernel = ctypes.WinDLL("kernel32", use_last_error=True)
        kernel.CreateFileW.argtypes = [ctypes.c_wchar_p, ctypes.c_uint32, ctypes.c_uint32, ctypes.c_void_p, ctypes.c_uint32, ctypes.c_uint32, ctypes.c_void_p]
        kernel.CreateFileW.restype = ctypes.c_void_p
        kernel.CloseHandle.argtypes = [ctypes.c_void_p]
        handle = kernel.CreateFileW(str(self.guide / "AGENTS.md"), 0x80000000, 1, None, 3, 0, None)
        self.assertNotEqual(handle, ctypes.c_void_p(-1).value)
        try:
            self.invoke("-Apply", success=False)
        finally:
            kernel.CloseHandle(handle)
        self.assertEqual((self.bin / "lane.exe").read_bytes(), b"old executable")
        self.assertEqual((self.guide / "LANE.md").read_bytes(), b"old guide")
        self.assertEqual((self.guide / "AGENTS.md").read_bytes(), b"Existing instructions\n")


if __name__ == "__main__":
    unittest.main()
