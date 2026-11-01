"""sycrOS tests. Run:  python3 -m unittest discover -s tests -v

Most tests need no internet and no root. The full-disk test (TestImageDisk)
needs the Ventoy boot engine (downloaded once, or SYCROS_TARBALL=path) and
Linux with sgdisk/sfdisk + an exFAT formatter.
"""
import json
import os
import shutil
import subprocess
import sys
import tempfile
import unittest
import uuid

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))

from sycros import gpt as G  # noqa: E402
from sycros import library, macimg, udif, layout, engine  # noqa: E402
from tests import fixtures as F  # noqa: E402


def _read(path):
    with open(path, encoding="utf-8") as f:
        return f.read()


class Mem:
    def __init__(self, size):
        self.buf = bytearray(size)
        self.size = size
        self.sectors = size // 512

    def read_at(self, o, n):
        return bytes(self.buf[o:o + n])

    def write_at(self, o, d):
        self.buf[o:o + len(d)] = d


class TestGPT(unittest.TestCase):
    def test_roundtrip_and_backup(self):
        m = Mem(64 * 1024 * 1024)
        g = G.GPT(m.sectors)
        g.add(G.Partition(G.MS_BASIC_DATA, uuid.uuid4(), 2048, 4095, 0, "Ventoy"))
        g.add(G.Partition(G.APPLE_HFS, uuid.uuid4(), 8192, 9000, 0, "Install macOS Ünïcode"))
        g.write(m)
        r = G.GPT.read(m, m.sectors)
        self.assertEqual([(n, p.name) for n, p in r.partitions()],
                         [(1, "Ventoy"), (2, "Install macOS Ünïcode")])
        m.buf[512:1024] = b"\0" * 512  # destroy primary header -> backup is used
        r2 = G.GPT.read(m, m.sectors)
        self.assertEqual(r2.slots[1].type_guid, G.APPLE_HFS)

    def test_overlap_and_free_space(self):
        g = G.GPT(100000)
        g.add(G.Partition(G.MS_BASIC_DATA, uuid.uuid4(), 2048, 10239))
        with self.assertRaises(G.GPTError):
            g.add(G.Partition(G.MS_BASIC_DATA, uuid.uuid4(), 10000, 12000))
        self.assertEqual(g.find_space(2048, 10240), 10240)
        self.assertEqual(g.find_space(2048, 10241), 12288)
        self.assertIsNone(g.find_space(10 ** 6, 0))


class TestMacImages(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.mkdtemp()
        self.vol = F.hfs_volume("Install macOS Sonoma")

    def tearDown(self):
        shutil.rmtree(self.tmp)

    def _file(self, name, data):
        p = os.path.join(self.tmp, name)
        with open(p, "wb") as f:
            f.write(data)
        return p

    def _check(self, path, label="Install macOS Sonoma"):
        v = macimg.find_volume(path, self.tmp)
        try:
            self.assertEqual(v.fs, "hfs")
            self.assertEqual(v.label, label)
            self.assertEqual(v.length, len(self.vol))
            self.assertEqual(v.kind, "installer")
            with open(v.source, "rb") as f:
                f.seek(v.offset)
                self.assertEqual(f.read(v.length), self.vol)
        finally:
            v.cleanup()

    def test_plain_hfs_iso(self):  # mkmaciso-style ISO: HFS+ at byte 1024
        self._check(self._file("Sonoma.iso", self.vol))

    def test_gpt_dmg_img(self):  # mkmaciso .dmg.img (raw GPT)
        self._check(self._file("macOS_Sonoma.dmg.img", F.gpt_disk(self.vol)))

    def test_compressed_udif_dmg(self):
        raw = F.gpt_disk(self.vol)
        p = self._file("Sonoma.dmg", F.udif(raw))
        self.assertTrue(udif.is_udif(p))
        self._check(p)

    def test_apple_partition_map(self):
        self._check(self._file("old.cdr", F.apm_disk(self.vol)))

    def test_apfs(self):
        c = F.apfs_container()
        v = macimg.find_volume(self._file("macOS Base System.img", F.gpt_disk(c, G.APPLE_APFS, "")), self.tmp)
        self.assertEqual((v.fs, v.length, v.kind), ("apfs", len(c), "recovery"))

    def test_recovery_kind(self):
        vol = F.hfs_volume("macOS Base System")
        v = macimg.find_volume(self._file("rec.iso", vol))
        self.assertEqual(v.kind, "recovery")

    def test_not_mac(self):
        p = self._file("ubuntu.iso", b"\0" * 32768 + b"\x01CD001" + b"\0" * 4096)
        self.assertFalse(macimg.looks_like_mac(p))
        with self.assertRaises(ValueError):
            macimg.find_volume(p)

    def test_adc(self):
        # literal "abc" then copy 4 bytes from distance 3 -> "abcabca"
        self.assertEqual(udif.adc_decompress(bytes([0x82]) + b"abc" + bytes([0x04, 0x02])), b"abcabca")


class TestLibrary(unittest.TestCase):
    def test_classify(self):
        cases = {
            "Win11_24H2_English_x64.iso": "windows",
            "en-us_windows_server_2025.iso": "windows",
            "boot.wim": "windows",
            "ubuntu-24.04.1-desktop-amd64.iso": "linux",
            "archlinux-2026.10.01-x86_64.iso": "linux",
            "Fedora-Workstation-Live-41.iso": "linux",
            "memtest86-usb.img": "tools",
            "shell.efi": "tools",
        }
        with tempfile.TemporaryDirectory() as d:
            for name, want in cases.items():
                p = os.path.join(d, name)
                open(p, "wb").close()
                self.assertEqual(library.classify(p), want, name)

    def test_iso_label_used(self):
        with tempfile.TemporaryDirectory() as d:
            p = os.path.join(d, "x.iso")
            pvd = bytearray(2048)
            pvd[1:6] = b"CD001"
            pvd[40:72] = b"CCCOMA_X64FRE_EN-US_DV9".ljust(32)
            with open(p, "wb") as f:
                f.write(b"\0" * 32768 + pvd)
            self.assertEqual(library.classify(p), "windows")

    def test_ventoy_json_merge_and_mac_menu(self):
        with tempfile.TemporaryDirectory() as root:
            os.makedirs(os.path.join(root, "ventoy"))
            with open(os.path.join(root, "ventoy", "ventoy.json"), "w") as f:
                json.dump({"control": [{"VTOY_DEFAULT_MENU_MODE": "0"}], "theme": {"x": 1}}, f)
            library.ensure_layout(root)
            data = json.loads(_read(os.path.join(root, "ventoy", "ventoy.json")))
            self.assertEqual(data["theme"], {"x": 1})  # user settings kept
            self.assertIn({"VTOY_DEFAULT_MENU_MODE": "0"}, data["control"])  # not overridden
            self.assertIn({"VTOY_FILT_DOT_UNDERSCORE_FILE": "1"}, data["control"])
            for d in ("Windows", "Linux", "Tools"):
                self.assertTrue(os.path.isdir(os.path.join(root, d)))

            cfg = os.path.join(root, "ventoy", "ventoy_grub.cfg")
            with open(cfg, "w") as f:
                f.write("menuentry 'my own' { true }\n")
            library.write_mac_menu(root, [(3, "Install macOS Sonoma", "hfs"), (4, "Base", "apfs")])
            text = _read(cfg)
            self.assertIn("set root=(${vtoydev},gpt3)", text)
            self.assertIn("my own", text)
            self.assertIn("VTOY_RET", text)
            library.write_mac_menu(root, [])
            text = _read(cfg)
            self.assertNotIn("gpt3", text)
            self.assertIn("my own", text)


def _engine_or_skip():
    try:
        return engine.get_engine(tarball=os.environ.get("SYCROS_TARBALL"), quiet=True)
    except Exception as e:  # offline etc.
        raise unittest.SkipTest(f"boot engine not available: {e}")


@unittest.skipUnless(sys.platform.startswith("linux") and shutil.which("sgdisk"), "needs Linux + sgdisk")
class TestImageDisk(unittest.TestCase):
    """Full flow on a sparse image file: init -> add macOS -> remove."""

    def setUp(self):
        self.eng = _engine_or_skip()
        self.tmp = tempfile.mkdtemp()
        self.img = os.path.join(self.tmp, "stick.img")
        self.vol = F.hfs_volume("Install macOS Tahoe", blocks=4096)  # 16 MiB

    def tearDown(self):
        shutil.rmtree(self.tmp)

    def _verify(self):
        out = subprocess.run(["sgdisk", "-v", self.img], capture_output=True, text=True).stdout
        self.assertIn("No problems found", out)
        return json.loads(subprocess.check_output(["sfdisk", "--json", self.img]))["partitiontable"]["partitions"]

    def test_full_flow(self):
        with layout.Disk(self.img, create_size=1024 * 1024 * 1024) as d:
            d.data_folder = lambda: None  # don't mount in tests
            layout.init(d, self.eng, 200 * 1024 * 1024, log=lambda *a: None)
        parts = self._verify()
        self.assertEqual([p["name"] for p in parts], ["Ventoy", "VTOYEFI"])
        self.assertEqual(parts[1]["size"], 65536)
        self.assertEqual((parts[1]["start"]) % 8, 0)
        with open(self.img, "rb") as f:
            mbr = f.read(512)
            self.assertEqual((mbr[92], mbr[450], mbr[510:]), (0x22, 0xEE, b"\x55\xaa"))
            f.seek(parts[0]["start"] * 512 + 3)
            self.assertEqual(f.read(8), b"EXFAT   ")

        dmg = os.path.join(self.tmp, "Tahoe.dmg")
        with open(dmg, "wb") as f:
            f.write(F.udif(F.gpt_disk(self.vol)))
        iso = os.path.join(self.tmp, "Recovery.iso")
        rec = F.hfs_volume("macOS Base System", blocks=2048)
        with open(iso, "wb") as f:
            f.write(rec)

        with layout.Disk(self.img) as d:
            n1 = layout.add_mac(d, dmg, log=lambda *a: None)
            n2 = layout.add_mac(d, iso, log=lambda *a: None)
            self.assertEqual((n1, n2), (3, 4))
        parts = self._verify()
        self.assertEqual(parts[2]["type"], str(G.APPLE_HFS).upper())
        self.assertEqual(parts[2]["name"], "Install macOS Tahoe")
        self.assertEqual(parts[3]["name"], "macOS Base System")
        with open(self.img, "rb") as f:
            f.seek(parts[2]["start"] * 512)
            self.assertEqual(f.read(len(self.vol)), self.vol)
            f.seek(parts[3]["start"] * 512)
            self.assertEqual(f.read(len(rec)), rec)

        # too big for the space left -> clear error, disk unchanged
        big = os.path.join(self.tmp, "big.iso")
        with open(big, "wb") as f:
            f.write(F.hfs_volume("Install macOS Huge", blocks=60000))
        with layout.Disk(self.img) as d, self.assertRaises(layout.LayoutError):
            layout.add_mac(d, big, log=lambda *a: None)

        with layout.Disk(self.img) as d:
            layout.remove_mac(d, 3, log=lambda *a: None)
            n = layout.add_mac(d, iso, name="Recovery again", log=lambda *a: None)
            self.assertEqual(n, 3)  # freed slot + space reused
        parts = self._verify()
        self.assertEqual(sorted(p["name"] for p in parts)[0], "Recovery again")


if __name__ == "__main__":
    unittest.main()
