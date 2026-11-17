"""Outlook .msg files: a minimal reader over ``olefile`` following MS-OXMSG.

Reads the subject, sender, recipients, submit time, body, and by-value attachments. The body is
taken from the plain-text property; when a message carries only HTML or only compressed RTF, the
HTML is converted to text, or the RTF is decompressed (MS-OXRTFCP) and reduced to text by a small
converter that handles common RTF and HTML encapsulated in RTF. That conversion is approximate and
is reported with a ``refs.msg-rtf-body`` diagnostic. Embedded messages and OLE objects attached by
reference are skipped with a diagnostic.
"""

from __future__ import annotations

import re
import struct
from datetime import UTC, datetime, timedelta
from pathlib import Path
from typing import Any

from ...errors import UNSUPPORTED_REFERENCE, RpcError
from ..model import Ctx, diag
from .eml import mail_doc

P_SUBJECT, P_SENDER_NAME, P_SENDER_EMAIL, P_SENDER_SMTP = 0x0037, 0x0C1A, 0x0C1F, 0x5D01
P_SENT_REPR_NAME, P_SENT_REPR_EMAIL = 0x0042, 0x0065
P_DISPLAY_TO, P_DISPLAY_CC = 0x0E04, 0x0E03
P_SUBMIT_TIME, P_DELIVERY_TIME = 0x0039, 0x0E06
P_BODY, P_HTML, P_RTF = 0x1000, 0x1013, 0x1009
P_CODEPAGE, P_INTERNET_CPID = 0x3FFD, 0x3FDE
P_ATT_DATA, P_ATT_LONG_NAME, P_ATT_SHORT_NAME, P_ATT_DISPLAY = 0x3701, 0x3707, 0x3704, 0x3001
P_RECIP_TYPE, P_RECIP_NAME, P_RECIP_EMAIL, P_RECIP_SMTP = 0x0C15, 0x3001, 0x3003, 0x39FE

_LZFU, _MELA = 0x75465A4C, 0x414C454D
_RTF_PREBUF = (
    b"{\\rtf1\\ansi\\mac\\deff0\\deftab720{\\fonttbl;}{\\f0\\fnil \\froman \\fswiss \\fmodern \\fscript "
    b"\\fdecor MS Sans SerifSymbolArialTimes New RomanCourier{\\colortbl\\red0\\green0\\blue0\r\n\\par "
    b"\\pard\\plain\\f0\\fs20\\b\\i\\u\\tab\\tx"
)


def rtf_decompress(data: bytes) -> bytes:
    """MS-OXRTFCP: the ``LZFu`` compressed and ``MELA`` uncompressed containers. The CRC is not checked."""
    if len(data) < 16:
        raise ValueError("compressed RTF header is truncated")
    comp_size, raw_size, magic, _crc = struct.unpack_from("<IIII", data)
    if magic == _MELA:
        return data[16:16 + raw_size]
    if magic != _LZFU:
        raise ValueError("not a compressed RTF stream")
    window = bytearray(4096)
    window[:len(_RTF_PREBUF)] = _RTF_PREBUF
    wpos = len(_RTF_PREBUF)
    out = bytearray()
    pos, end = 16, min(len(data), comp_size + 4)
    while pos < end:
        control = data[pos]
        pos += 1
        for bit in range(8):
            if control & (1 << bit):
                if pos + 2 > end:
                    return bytes(out)
                ref = (data[pos] << 8) | data[pos + 1]
                pos += 2
                offset, length = ref >> 4, (ref & 0xF) + 2
                if offset == wpos:
                    return bytes(out)
                for k in range(length):
                    c = window[(offset + k) % 4096]
                    out.append(c)
                    window[wpos] = c
                    wpos = (wpos + 1) % 4096
            else:
                if pos >= end:
                    return bytes(out)
                c = data[pos]
                pos += 1
                out.append(c)
                window[wpos] = c
                wpos = (wpos + 1) % 4096
    return bytes(out)


_RTF_TOKEN = re.compile(rb"\\([a-zA-Z]{1,32})(-?\d{1,10})?[ ]?|\\'([0-9a-fA-F]{2})|\\([^a-zA-Z])|([{}])|[\r\n]+|([^\\{}\r\n]+)")
_RTF_SKIP = frozenset(
    ["fonttbl", "colortbl", "stylesheet", "info", "pict", "header", "footer", "headerl", "headerr", "headerf", "footerl", "footerr", "footerf", "generator", "datastore", "themedata", "colorschememapping", "latentstyles", "listtable", "listoverridetable", "rsidtbl", "xmlnstbl", "object", "objdata", "mmathPr", "filetbl", "revtbl", "pgdsctbl", "datafield", "fldinst", "nonshppict", "wgrffmtfilter", "pnseclvl", "background", "operator", "author", "title", "subject", "keywords", "comment"]
)
_RTF_CHARS = {"par": "\n", "line": "\n", "sect": "\n", "page": "\n", "row": "\n", "tab": "\t", "cell": "\t",
              "emdash": "—", "endash": "–", "lquote": "‘", "rquote": "’", "ldblquote": "“", "rdblquote": "”",
              "bullet": "•", "emspace": " ", "enspace": " "}  # fmt: skip


def rtf_to_text(rtf: bytes) -> tuple[str, bool]:
    """Plain text of an RTF document, and whether it was HTML encapsulated in RTF (then the
    returned text is the HTML source)."""
    html = b"\\fromhtml" in rtf[:4096]
    codec = "cp1252"
    out: list[str] = []
    pending = bytearray()
    stack: list[tuple[bool, int, bool]] = []
    skip, uc, in_tag = False, 1, False  # skip group, \uc value, inside {\*\htmltag}
    star = False
    html_off = False  # between \htmlrtf and \htmlrtf0
    to_skip = 0

    def flush() -> None:
        if pending:
            out.append(pending.decode(codec, errors="replace"))
            pending.clear()

    for m in _RTF_TOKEN.finditer(rtf):
        word, arg, hexa, symbol, brace, text = m.group(1), m.group(2), m.group(3), m.group(4), m.group(5), m.group(6)
        if brace == b"{":
            stack.append((skip, uc, in_tag))
            star = False
        elif brace == b"}":
            if stack:
                skip, uc, in_tag = stack.pop()
            star = False
        elif word is not None:
            name = word.decode("ascii")
            if name == "ansicpg" and arg:
                try:
                    "".encode(f"cp{int(arg)}")
                    codec = f"cp{int(arg)}"
                except LookupError:
                    pass
            elif name == "htmlrtf":
                html_off = arg != b"0"
            elif name == "htmltag" and html:
                skip, in_tag = False, True
            elif name in _RTF_SKIP or (star and not (html and name in ("htmltag", "mhtmltag"))):
                skip = True
            elif skip or (html and html_off):
                pass
            elif name == "uc" and arg:
                uc = int(arg)
            elif name == "u" and arg:
                flush()
                code = int(arg)
                out.append(chr(code + 65536 if code < 0 else code))
                to_skip = uc
            elif name in _RTF_CHARS:
                flush()
                out.append(_RTF_CHARS[name])
            star = False
        elif hexa is not None:
            if to_skip:
                to_skip -= 1
            elif not skip and not (html and html_off):
                pending += bytes([int(hexa, 16)])
        elif symbol is not None:
            if symbol == b"*":
                star = True
            elif not skip and not (html and html_off):
                if symbol == b"~":
                    flush()
                    out.append("\u00a0")
                elif symbol in (b"{", b"}", b"\\"):
                    pending += symbol
                elif symbol == b"_":  # non-breaking hyphen; "\\-" (optional hyphen) produces nothing
                    pending += b"-"
        elif text is not None and not skip and not (html and html_off):
            if to_skip:
                text, to_skip = text[to_skip:], max(0, to_skip - len(text))
            pending += text
    flush()
    return "".join(out), html


class MsgFile:
    """Property access over the storages and streams of a .msg file."""

    def __init__(self, ole) -> None:
        self.ole = ole
        self.codec = "cp1252"
        props = self.fixed((), 32)
        for key in (P_INTERNET_CPID, P_CODEPAGE):
            if key in props:
                codepage = struct.unpack_from("<I", props[key])[0]
                try:
                    "".encode(f"cp{codepage}")
                except LookupError:
                    continue
                self.codec = "utf-8" if codepage == 65001 else f"cp{codepage}"
                break

    def stream(self, storage: tuple[str, ...], prop: int, kind: str) -> bytes | None:
        path = [*storage, f"__substg1.0_{prop:04X}{kind}"]
        if not self.ole.exists(path):
            return None
        with self.ole.openstream(path) as handle:
            return handle.read()

    def string(self, prop: int, storage: tuple[str, ...] = ()) -> str:
        data = self.stream(storage, prop, "001F")
        if data is not None:
            return data.decode("utf-16-le", errors="replace").rstrip("\x00")
        data = self.stream(storage, prop, "001E")
        if data is not None:
            return data.decode(self.codec, errors="replace").rstrip("\x00")
        return ""

    def binary(self, prop: int, storage: tuple[str, ...] = ()) -> bytes | None:
        return self.stream(storage, prop, "0102")

    def fixed(self, storage: tuple[str, ...], header: int) -> dict[int, bytes]:
        """Fixed-size properties from ``__properties_version1.0``: property id -> 8 value bytes."""
        path = [*storage, "__properties_version1.0"]
        if not self.ole.exists(path):
            return {}
        with self.ole.openstream(path) as handle:
            data = handle.read()
        out: dict[int, bytes] = {}
        for pos in range(header, len(data) - 15, 16):
            _type, prop = struct.unpack_from("<HH", data, pos)
            out[prop] = data[pos + 8:pos + 16]
        return out

    def storages(self, prefix: str) -> list[str]:
        names = {entry[0] for entry in self.ole.listdir(streams=True, storages=True) if entry and entry[0].startswith(prefix)}
        return sorted(names)


def filetime_iso(value: bytes) -> str:
    ticks = struct.unpack_from("<Q", value)[0]
    if not ticks:
        return ""
    try:
        moment = datetime(1601, 1, 1, tzinfo=UTC) + timedelta(microseconds=ticks // 10)
    except OverflowError:
        return ""
    return moment.isoformat(timespec="seconds").replace("+00:00", "Z")


def read_msg(ole) -> dict[str, Any]:
    """Subject, sender, recipients, date, body, attachments, and warnings from an open OLE file."""
    msg = MsgFile(ole)
    warnings: list[dict[str, Any]] = []
    props = msg.fixed((), 32)
    sender_name = msg.string(P_SENDER_NAME) or msg.string(P_SENT_REPR_NAME)
    sender_email = msg.string(P_SENDER_SMTP) or msg.string(P_SENDER_EMAIL) or msg.string(P_SENT_REPR_EMAIL)
    recipients: dict[int, list[str]] = {1: [], 2: [], 3: []}
    for storage in msg.storages("__recip_version1.0_#"):
        name = msg.string(P_RECIP_NAME, (storage,))
        address = msg.string(P_RECIP_SMTP, (storage,)) or msg.string(P_RECIP_EMAIL, (storage,))
        kind_raw = msg.fixed((storage,), 8).get(P_RECIP_TYPE)
        kind = struct.unpack_from("<I", kind_raw)[0] if kind_raw else 1
        label = f"{name} <{address}>" if name and address and name != address else (name or address)
        if label:
            recipients.setdefault(kind, []).append(label)
    date = ""
    for key in (P_SUBMIT_TIME, P_DELIVERY_TIME):
        if key in props and not date:
            date = filetime_iso(props[key])

    body = msg.string(P_BODY)
    if not body.strip():
        html = msg.binary(P_HTML)
        html_text = msg.string(P_HTML) if html is None else None
        rtf = msg.binary(P_RTF)
        if html or html_text:
            from .html import html_to_text

            body = html_to_text(html if html else html_text)
        elif rtf:
            try:
                text, was_html = rtf_to_text(rtf_decompress(rtf))
                if was_html:
                    from .html import html_to_text

                    text = html_to_text(text)
                body = "\n".join(line.rstrip() for line in text.split("\n")).strip()
                warnings.append(diag("refs.msg-rtf-body", "the message has only an RTF body; its text was recovered by an approximate RTF conversion", "info"))
            except Exception as exc:
                warnings.append(diag("refs.msg-rtf-body", f"the message has only an RTF body, which could not be converted ({exc})"))

    attachments: list[tuple[str, bytes]] = []
    for storage in msg.storages("__attach_version1.0_#"):
        name = (msg.string(P_ATT_LONG_NAME, (storage,)) or msg.string(P_ATT_SHORT_NAME, (storage,))
                or msg.string(P_ATT_DISPLAY, (storage,)))  # fmt: skip
        data = msg.binary(P_ATT_DATA, (storage,))
        if data is None:
            warnings.append(diag("refs.attachment-skipped", f"attachment {name or storage!r} is an embedded message or object, not a file, and was not imported", "info"))
            continue
        attachments.append((name, data))
    return {
        "subject": msg.string(P_SUBJECT),
        "from": f"{sender_name} <{sender_email}>" if sender_name and sender_email else (sender_name or sender_email),
        "to": "; ".join(recipients[1]) or msg.string(P_DISPLAY_TO),
        "cc": "; ".join(recipients[2]) or msg.string(P_DISPLAY_CC),
        "date": date,
        "body": body,
        "attachments": attachments,
        "warnings": warnings,
    }


def extract(path: Path, ctx: Ctx) -> dict[str, Any]:
    import olefile

    if not olefile.isOleFile(str(path)):
        raise RpcError(UNSUPPORTED_REFERENCE, f"{ctx.label}: not an Outlook .msg file (no OLE container)")
    ole = olefile.OleFileIO(str(path))
    try:
        data = read_msg(ole)
    finally:
        ole.close()
    headers = [("Subject", data["subject"]), ("From", data["from"]), ("To", data["to"]), ("Cc", data["cc"]),
               ("Date", data["date"])]  # fmt: skip
    doc = mail_doc("msg", ctx, data["subject"], headers, data["body"], data["attachments"])
    doc["warnings"] = data["warnings"] + doc["warnings"]
    return doc
