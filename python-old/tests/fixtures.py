"""Build small fake macOS images for tests (no Mac or root needed)."""
import plistlib
import struct
import uuid
import zlib

from sycros import gpt as G

BLOCK = 4096


def hfs_volume(label: str, blocks: int = 2048, marker: bytes = b"SYCROS-TEST") -> bytes:
    """A tiny HFS+ volume: header + a catalog whose first leaf holds the root folder name."""
    vol = bytearray(blocks * BLOCK)
    h = bytearray(512)
    h[0:2] = b"H+"
    struct.pack_into(">H", h, 2, 4)
    struct.pack_into(">II", h, 40, BLOCK, blocks)
    # catalog file fork at 272: logicalSize, clump, totalBlocks, extent[0]=(start=2,count=2)
    struct.pack_into(">QII", h, 272, 2 * BLOCK, BLOCK, 2)
    struct.pack_into(">II", h, 272 + 16, 2, 2)
    vol[1024:1536] = h
    cat = 2 * BLOCK
    node_size = BLOCK
    # node 0: header node. descriptor(14) then header record: depth(2) root(4) leafRecords(4) firstLeaf(4)...
    struct.pack_into(">IIbbHH", vol, cat, 0, 0, 1, 0, 3, 0)
    struct.pack_into(">HIII", vol, cat + 14, 1, 1, 1, 1)  # firstLeafNode = 1 at +10
    struct.pack_into(">H", vol, cat + 14 + 18, node_size)
    # node 1: leaf with one record: key(keyLength, parentID=1, name)
    leaf = cat + node_size
    name = label.encode("utf-16-be")
    struct.pack_into(">IIbbHH", vol, leaf, 0, 0, -1, 1, 1, 0)
    rec = 14
    key = struct.pack(">HIH", 6 + len(name), 1, len(label)) + name
    vol[leaf + rec:leaf + rec + len(key)] = key
    struct.pack_into(">H", vol, leaf + node_size - 2, rec)
    vol[-BLOCK:-BLOCK + len(marker)] = marker  # recognisable bytes at the end
    return bytes(vol)


def apfs_container(blocks: int = 1024) -> bytes:
    c = bytearray(blocks * BLOCK)
    c[32:36] = b"NXSB"
    struct.pack_into("<IQ", c, 36, BLOCK, blocks)
    return bytes(c)


def gpt_disk(volume: bytes, ptype=G.APPLE_HFS, name="Install macOS Test") -> bytes:
    """Wrap a volume in a GPT disk image with an EFI partition first (like hdiutil makes)."""
    vol_sectors = len(volume) // 512
    total = 40 + 409600 // 512 * 0 + 2048 + 2048 + vol_sectors + 2048
    total = -(-total // 2048) * 2048 + 2048
    img = bytearray(total * 512)

    class Mem:
        def read_at(self, o, n):
            return bytes(img[o:o + n])

        def write_at(self, o, d):
            img[o:o + len(d)] = d

    g = G.GPT(total)
    g.add(G.Partition(G.EFI_SYSTEM, uuid.uuid4(), 40, 2047, 0, "EFI System Partition"))
    g.add(G.Partition(ptype, uuid.uuid4(), 4096, 4096 + vol_sectors - 1, 0, name))
    g.write(Mem())
    img[446:462] = G.protective_mbr_entry(total)
    img[510:512] = b"\x55\xaa"
    img[4096 * 512:4096 * 512 + len(volume)] = volume
    return bytes(img)


def apm_disk(volume: bytes, block=512) -> bytes:
    """Old Apple Partition Map image with one Apple_HFS partition."""
    start = 64
    img = bytearray(start * 512 + len(volume))
    img[0:2] = b"ER"
    struct.pack_into(">H", img, 2, block)
    e = bytearray(512)
    e[0:2] = b"PM"
    struct.pack_into(">III", e, 4, 1, start, len(volume) // 512)
    e[16:16 + 9] = b"disk image"
    e[48:48 + 9] = b"Apple_HFS"
    img[512:1024] = e
    img[start * 512:] = volume
    return bytes(img)


def udif(raw: bytes, chunk_sectors: int = 256) -> bytes:
    """Encode raw bytes as a UDIF .dmg using zlib / raw / zero chunks."""
    sectors = len(raw) // 512
    data = bytearray()
    chunks = []
    for i, s in enumerate(range(0, sectors, chunk_sectors)):
        n = min(chunk_sectors, sectors - s)
        piece = raw[s * 512:(s + n) * 512]
        if piece.count(0) == len(piece):
            chunks.append((0x00000000, s, n, len(data), 0))
        elif i % 3 == 1:
            chunks.append((0x00000001, s, n, len(data), len(piece)))
            data += piece
        else:
            z = zlib.compress(piece)
            chunks.append((0x80000005, s, n, len(data), len(z)))
            data += z
    chunks.append((0xFFFFFFFF, sectors, 0, len(data), 0))
    mish = bytearray(204)
    mish[0:4] = b"mish"
    struct.pack_into(">IQQQII", mish, 4, 1, 0, sectors, 0, 0, 0)
    struct.pack_into(">I", mish, 200, len(chunks))
    for kind, s, n, off, ln in chunks:
        mish += struct.pack(">IIQQQQ", kind, 0, s, n, off, ln)
    plist = plistlib.dumps({"resource-fork": {"blkx": [{"Name": "whole disk", "Data": bytes(mish)}]}})
    koly = bytearray(512)
    koly[0:4] = b"koly"
    struct.pack_into(">III", koly, 4, 4, 512, 1)
    struct.pack_into(">QQ", koly, 24, 0, len(data))
    struct.pack_into(">QQ", koly, 216, len(data), len(plist))
    struct.pack_into(">Q", koly, 492, sectors)
    return bytes(data) + plist + bytes(koly)
