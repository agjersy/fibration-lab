"""A minimal Compound File Binary (MS-CFB v3) writer for tests.

``olefile`` can only read, so the MSG fixture is produced with this writer: 512-byte sectors,
a mini stream for streams under 4096 bytes, and one balanced sibling tree per storage. It writes
only what the tests need (no DIFAT sectors, so files stay under about 6.8 MB) and is not meant
for anything but fixtures.
"""

from __future__ import annotations

import struct

SECTOR = 512
MINI = 64
CUTOFF = 4096
FREE, END, FATSECT = 0xFFFFFFFF, 0xFFFFFFFE, 0xFFFFFFFD
NOSTREAM = 0xFFFFFFFF


def _key(name: str) -> tuple[int, str]:
    return (len(name.encode("utf-16-le")), name.upper())


class _Entry:
    def __init__(self, name: str, kind: int, data: bytes = b"") -> None:
        self.name, self.kind, self.data = name, kind, data
        self.children: dict[str, _Entry] = {}
        self.left = self.right = self.child = NOSTREAM
        self.start, self.size, self.sid = END, 0, 0


def build_cfb(tree: dict) -> bytes:
    """``tree`` maps names to bytes (a stream) or to a nested dict (a storage)."""
    root = _Entry("Root Entry", 5)

    def add(parent: _Entry, node: dict) -> None:
        for name, value in node.items():
            if isinstance(value, dict):
                entry = _Entry(name, 1)
                add(entry, value)
            else:
                entry = _Entry(name, 2, bytes(value))
            parent.children[name] = entry

    add(root, tree)
    entries: list[_Entry] = [root]

    def number(parent: _Entry) -> None:
        kids = sorted(parent.children.values(), key=lambda e: _key(e.name))
        for kid in kids:
            kid.sid = len(entries)
            entries.append(kid)

        def balance(lo: int, hi: int) -> int:
            if lo >= hi:
                return NOSTREAM
            mid = (lo + hi) // 2
            kids[mid].left, kids[mid].right = balance(lo, mid), balance(mid + 1, hi)
            return kids[mid].sid

        parent.child = balance(0, len(kids))
        for kid in kids:
            if kid.kind == 1:
                number(kid)

    number(root)

    # Mini stream: every stream below the cutoff, in 64-byte mini sectors.
    mini = bytearray()
    minifat: list[int] = []
    for e in entries:
        if e.kind == 2 and 0 < len(e.data) < CUTOFF:
            first = len(mini) // MINI
            count = -(-len(e.data) // MINI)
            mini += e.data + b"\0" * (count * MINI - len(e.data))
            minifat += [*range(first + 1, first + count), END]
            e.start, e.size = first, len(e.data)

    sectors: list[bytes] = []
    fat: list[int] = []

    def put(data: bytes, marker: int | None = None) -> int:
        """Append data as a sector chain; returns the first sector index."""
        first = len(sectors)
        count = max(1, -(-len(data) // SECTOR))
        for i in range(count):
            sectors.append(data[i * SECTOR:(i + 1) * SECTOR].ljust(SECTOR, b"\0"))
            fat.append(marker if marker is not None else (first + i + 1 if i + 1 < count else END))
        return first

    for e in entries:
        if e.kind == 2 and len(e.data) >= CUTOFF:
            e.start, e.size = put(e.data), len(e.data)
    if mini:
        root.start, root.size = put(bytes(mini)), len(mini)
    minifat_start, minifat_count = END, 0
    if minifat:
        raw = b"".join(struct.pack("<I", v) for v in minifat)
        raw = raw.ljust(-(-len(raw) // SECTOR) * SECTOR, b"\xff")
        minifat_start, minifat_count = put(raw), len(raw) // SECTOR

    dir_raw = bytearray()
    for e in entries:
        name = e.name.encode("utf-16-le") + b"\0\0"
        assert len(name) <= 64, e.name
        dir_raw += struct.pack(
            "<64sHBBIII16sIQQIQ", name.ljust(64, b"\0"), len(name), e.kind, 1, e.left, e.right, e.child,
            b"\0" * 16, 0, 0, 0, 0 if e.kind == 1 else e.start, 0 if e.kind == 1 else e.size,
        )  # fmt: skip
    while len(dir_raw) % SECTOR:
        dir_raw += struct.pack("<64sHBBIII16sIQQIQ", b"", 0, 0, 0, NOSTREAM, NOSTREAM, NOSTREAM, b"\0" * 16, 0, 0, 0, 0, 0)
    dir_start = put(bytes(dir_raw))

    # The FAT must also describe its own sectors.
    per = SECTOR // 4
    nfat = 1
    while nfat * per < len(fat) + nfat:
        nfat += 1
    assert nfat <= 109, "file too large for this writer"
    fat_first = len(sectors)
    fat += [FATSECT] * nfat
    fat_raw = b"".join(struct.pack("<I", v) for v in fat).ljust(nfat * SECTOR, b"\xff")
    for i in range(nfat):
        sectors.append(fat_raw[i * SECTOR:(i + 1) * SECTOR])

    difat = [fat_first + i for i in range(nfat)] + [FREE] * (109 - nfat)
    header = struct.pack(
        "<8s16sHHHHH6sIIIIIIIII", b"\xd0\xcf\x11\xe0\xa1\xb1\x1a\xe1", b"\0" * 16, 0x3E, 3, 0xFFFE, 9, 6, b"\0" * 6,
        0, nfat, dir_start, 0, CUTOFF, minifat_start, minifat_count, END, 0,
    )  # fmt: skip
    header += b"".join(struct.pack("<I", v) for v in difat)
    assert len(header) == SECTOR
    return header + b"".join(sectors)


def _props(pairs: list[tuple[int, int, bytes]], header_len: int) -> bytes:
    out = bytearray(b"\0" * header_len)
    for prop_id, prop_type, value in pairs:
        out += struct.pack("<HHI", prop_type, prop_id, 6) + value.ljust(8, b"\0")[:8]
    return bytes(out)


def build_msg(*, subject: str, sender_name: str, sender_email: str, to: str, body: str | None,
              submit_filetime: int, attachments: list[tuple[str, bytes]] = (), unicode: bool = True,
              rtf_compressed: bytes | None = None, recipients: list[tuple[str, str]] = ()) -> bytes:
    """An Outlook .msg (MS-OXMSG) with the handful of properties the reader looks at."""
    suffix, enc = ("001F", "utf-16-le") if unicode else ("001E", "cp1252")
    kind = 0x001F if unicode else 0x001E

    def s(prop: str, value: str) -> tuple[str, bytes]:
        return (f"__substg1.0_{prop}{suffix}", value.encode(enc))

    tree: dict = dict([s("0037", subject), s("0C1A", sender_name), s("0C1F", sender_email), s("0E04", to),
                       s("001A", "IPM.Note")])  # fmt: skip
    fixed = [(0x0039, 0x0040, struct.pack("<Q", submit_filetime))]
    var = [(0x0037, kind), (0x0C1A, kind), (0x0C1F, kind), (0x0E04, kind), (0x001A, kind)]
    if body is not None:
        name, data = s("1000", body)
        tree[name] = data
        var.append((0x1000, kind))
    if rtf_compressed is not None:
        tree["__substg1.0_10090102"] = rtf_compressed
        var.append((0x1009, 0x0102))
    for prop_id, prop_type in var:
        key = f"__substg1.0_{prop_id:04X}{prop_type:04X}"
        fixed.append((prop_id, prop_type, struct.pack("<II", len(tree[key]) + (2 if prop_type == 0x1F else 1 if prop_type == 0x1E else 0), 0)))
    tree["__properties_version1.0"] = _props(fixed, 32)
    for i, (name, email) in enumerate(recipients):
        tree[f"__recip_version1.0_#{i:08X}"] = {
            **dict([s("3001", name), s("39FE", email)]),
            "__properties_version1.0": _props([(0x0C15, 0x0003, struct.pack("<I", 1))], 8),
        }
    for i, (filename, data) in enumerate(attachments):
        tree[f"__attach_version1.0_#{i:08X}"] = {
            **dict([s("3707", filename), s("3704", filename[:12])]),
            "__substg1.0_37010102": data,
            "__properties_version1.0": _props([(0x3705, 0x0003, struct.pack("<I", 1)), (0x3701, 0x0102, struct.pack("<II", len(data), 0))], 8),
        }
    return build_cfb(tree)


def rtf_compress_literal(rtf: bytes) -> bytes:
    """MS-OXRTFCP 'LZFu' container using only literal tokens (valid, just not small)."""
    out = bytearray()
    for i in range(0, len(rtf), 8):
        block = rtf[i:i + 8]
        if len(block) == 8:
            out += b"\x00" + block
        else:
            # Final run: literals, then the end marker (a reference to the write position).
            pos = (207 + len(rtf)) % 4096
            out += bytes([1 << len(block)]) + block + struct.pack(">H", pos << 4)
            break
    else:
        pos = (207 + len(rtf)) % 4096
        out += b"\x01" + struct.pack(">H", pos << 4)
    # The CRC field is left at zero: the reader does not verify it.
    return struct.pack("<IIII", len(out) + 12, len(rtf), 0x75465A4C, 0) + bytes(out)
