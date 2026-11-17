"""W9: reference ingestion, locations, the reader, and the refs.* RPC methods."""

from __future__ import annotations

import hashlib
import itertools
import json
import re
import shutil
import sys
import time
from pathlib import Path

import pytest

FIXTURES = Path(__file__).resolve().parent / "fixtures" / "refs"
sys.path.insert(0, str(FIXTURES))

import make_fixtures  # noqa: E402

from present_kernel.errors import RpcError  # noqa: E402
from present_kernel.kernel import Kernel  # noqa: E402
from present_kernel.refs import chunker, locs  # noqa: E402
from present_kernel.refs.reader import ProjectRefs  # noqa: E402

NAMES = ["report.pdf", "bookmarks.pdf", "review.docx", "update.pptx", "figures.xlsx", "units.csv", "handbook.md",
         "notes.txt", "chart.png", "mail.eml", "mail.msg", "mail-rtf.msg", "brief.html", "brief.mhtml"]  # fmt: skip
KINDS = {"report.pdf": "pdf", "bookmarks.pdf": "pdf", "review.docx": "docx", "update.pptx": "pptx",
         "figures.xlsx": "xlsx", "units.csv": "csv", "handbook.md": "markdown", "notes.txt": "text",
         "chart.png": "image", "mail.eml": "eml", "mail.msg": "msg", "mail-rtf.msg": "msg", "brief.html": "html",
         "brief.mhtml": "html"}  # fmt: skip
REGION_COLUMNS = ["Region", "Q1", "Q2", "Q3"]
NORTH = ["North", 120, 135, 150]
CHAR_KINDS = ("pdf", "docx", "markdown", "text", "html", "eml", "msg")


def add_reference(project: Path, source: Path) -> str:
    """Copy a file into references/ the way the core does: ``<first 12 hex of sha256>.<ext>``."""
    data = source.read_bytes()
    rel = f"references/{hashlib.sha256(data).hexdigest()[:12]}{source.suffix}"
    (project / "references").mkdir(parents=True, exist_ok=True)
    (project / rel).write_bytes(data)
    return rel


class Env:
    def __init__(self, root: Path) -> None:
        self.project = root / "project"
        self.project.mkdir(parents=True)
        self.kernel = Kernel(project=self.project, home=root / "home")
        self.paths = {name: add_reference(self.project, FIXTURES / name) for name in NAMES}
        self.results: dict[str, dict] = {}

    def call(self, method: str, params: dict | None = None):
        return self.kernel.dispatcher.call(method, params or {})

    def ingest(self, name: str) -> dict:
        if name not in self.results:
            self.results[name] = self.call("refs.ingest", {"path": self.paths[name]})
        return self.results[name]


@pytest.fixture(scope="module")
def env(tmp_path_factory) -> Env:
    made = Env(tmp_path_factory.mktemp("refs"))
    yield made
    made.kernel.close()


def error_code(fn) -> int:
    with pytest.raises(RpcError) as info:
        fn()
    return info.value.code


def headings(outline: list[dict]) -> list[str]:
    out: list[str] = []
    for node in outline:
        out.append(node["title"])
        out += headings(node["children"])
    return out


# ---- every kind ---------------------------------------------------------------------------------
@pytest.mark.parametrize("name", NAMES)
def test_ingest_result_shape(env, name):
    result = env.ingest(name)
    assert result["kind"] == KINDS[name]
    assert re.fullmatch(r"r-[0-9a-f]{8}", result["ref_id"])
    assert result["version"] == Path(env.paths[name]).stem and result["file"] == env.paths[name]
    assert isinstance(result["title"], str) and result["title"]
    for key in ("outline", "chunks", "tables", "images", "children", "warnings"):
        assert isinstance(result[key], list), key
    for chunk in result["chunks"]:
        assert set(chunk) == {"loc", "heading_path", "text"} and chunk["text"].strip()
    for table in result["tables"]:
        assert table["rows"] >= 1 and table["cols"] >= 1
    for image in result["images"]:
        assert (env.project / image["png"]).is_file() and image["png"].startswith(f".present/cache/refs/{result['ref_id']}/")
        assert image["width"] > 0 and image["height"] > 0
    for warning in result["warnings"]:
        assert {"rule", "severity", "message"} <= set(warning)
    json.dumps(result)


@pytest.mark.parametrize("name", NAMES)
def test_chunk_locs_round_trip_through_refs_read(env, name):
    result = env.ingest(name)
    for chunk in result["chunks"]:
        got = env.call("refs.read", {"ref_id": result["ref_id"], "loc": chunk["loc"]})
        assert got["loc"] == chunk["loc"] and got["truncated"] is False
        if result["kind"] == "pptx" and len(got["text"]) > chunker.MAX_CHARS:
            assert chunk["text"] in got["text"]  # a long shape: several chunks share the shape's loc
        else:
            assert got["text"] == chunk["text"]
        cite = locs.make_cite(result["ref_id"], result["version"], chunk["loc"])
        assert chunk["text"] in env.kernel.refs.text(cite)


@pytest.mark.parametrize("name", NAMES)
def test_chunk_sizes_follow_protocol_8_1(env, name):
    result = env.ingest(name)
    chunks = result["chunks"]
    assert all(len(c["text"]) <= chunker.MAX_CHARS for c in chunks)
    if result["kind"] not in CHAR_KINDS:
        return
    spans = []
    for c in chunks:
        m = re.fullmatch(r"(?:(p\d+):)?c(\d+)-(\d+)", c["loc"])
        assert m, c["loc"]
        spans.append((m.group(1), int(m.group(2)), int(m.group(3)), c["heading_path"]))
        assert int(m.group(3)) - int(m.group(2)) == len(c["text"])
    for (page_a, start_a, end_a, path_a), (page_b, start_b, _, path_b) in itertools.pairwise(spans):
        if page_a == page_b and path_a == path_b and start_b < end_a:
            # Same page and section: the earlier chunk is full-sized and the two overlap by at most 150.
            assert end_a - start_a >= chunker.MIN_CHARS
            assert 0 < end_a - start_b <= chunker.OVERLAP
        else:
            assert start_b >= end_a or page_a != page_b


@pytest.mark.parametrize("name", NAMES)
def test_table_refs_resolve(env, name):
    result = env.ingest(name)
    refs = ProjectRefs(env.project)
    for ref in result["tables"]:
        table = refs.table(locs.make_cite(result["ref_id"], result["version"], ref["loc"]))
        assert table["loc"] == ref["loc"] and len(table["columns"]) == ref["cols"]
        assert len(table["rows"]) in (ref["rows"], ref["rows"] - 1)
        assert all(len(row) == ref["cols"] for row in table["rows"])
        assert env.call("refs.table", {"ref_id": result["ref_id"], "loc": ref["loc"]}) == table
        assert env.call("refs.read", {"ref_id": result["ref_id"], "loc": ref["loc"]})["text"]


@pytest.mark.parametrize("name", NAMES)
def test_golden_outline_and_chunks(env, name, request):
    result = env.ingest(name)
    actual = {k: result.get(k) for k in ("kind", "title", "pages", "outline", "chunks", "tables")}
    path = FIXTURES / "golden" / f"{name}.json"
    if request.config.getoption("--update-golden"):
        path.parent.mkdir(exist_ok=True)
        path.write_text(json.dumps(actual, indent=1, ensure_ascii=False) + "\n", encoding="utf-8")
    assert json.loads(path.read_text(encoding="utf-8")) == actual


# ---- per kind -----------------------------------------------------------------------------------
def test_pdf(env):
    result = env.ingest("report.pdf")
    assert result["title"] == "Quarterly Operations Report" and result["pages"] == 4
    titles = headings(result["outline"])
    assert titles[:2] == ["Chapter 1: Margin review", "Section 1.1 Details"] and "Section 2.2 Regional figures" in titles
    assert [n["title"] for n in result["outline"]] == [f"Chapter {i}: {w} review" for i, w in ((1, "Margin"), (2, "Quarter"), (3, "Growth"))]
    assert len(result["chunks"]) >= 9
    assert {c["loc"].split(":")[0] for c in result["chunks"]} == {"p1", "p2", "p3"}  # never across pages
    deep = next(c for c in result["chunks"] if c["heading_path"] == ["Chapter 2: Quarter review", "Section 2.2 Regional figures"])
    assert "North 120 135 150" in deep["text"]
    assert result["tables"] == [{"loc": "p2/t1", "rows": 6, "cols": 4}]
    table = env.call("refs.table", {"ref_id": result["ref_id"], "loc": "p2/t1"})
    assert table["columns"] == REGION_COLUMNS and table["rows"][0] == NORTH and table["rows"][-1] == ["Total", 365, 386, 431]
    assert [(i["loc"], i["width"], i["height"]) for i in result["images"]] == [("p1", 320, 200), ("p4", 400, 300)]
    assert [w["rule"] for w in result["warnings"]] == ["refs.no-text"] and "page 4" in result["warnings"][0]["message"]
    # The outline's heading locs read back as the heading text.
    node = result["outline"][1]["children"][0]
    assert env.call("refs.read", {"ref_id": result["ref_id"], "loc": node["loc"]})["text"] == node["title"]
    page = env.call("refs.read", {"ref_id": result["ref_id"], "loc": "p2"})["text"]
    assert page.startswith("Chapter 2: Quarter review") and env.call("refs.read", {"ref_id": result["ref_id"], "loc": "p4"})["text"] == ""
    cut = env.call("refs.read", {"ref_id": result["ref_id"], "loc": "p2:c10-500", "max_chars": 40})
    assert cut == {"text": page[10:50], "loc": "p2:c10-50", "truncated": True}
    assert error_code(lambda: env.call("refs.read", {"ref_id": result["ref_id"], "loc": "p9"})) == 2000
    assert error_code(lambda: env.call("refs.read", {"ref_id": result["ref_id"], "loc": "s1"})) == -32602


def test_pdf_bookmarks_become_the_outline(env):
    result = env.ingest("bookmarks.pdf")
    assert headings(result["outline"]) == ["Chapter 1: Margin review", "Section 1.1 Details", "Chapter 2: Quarter review",
                                           "Section 2.1 Details", "Section 2.2 Regional figures"]  # fmt: skip
    assert result["outline"][0]["children"][0]["loc"].startswith("p1:c") and result["warnings"] == []


def test_pdf_page_image(env):
    from PIL import Image

    result = env.ingest("report.pdf")
    out = env.call("refs.page_image", {"ref_id": result["ref_id"], "page": 4, "dpi": 72})
    with Image.open(env.project / out["png"]) as image:
        assert image.size == (595, 842) or abs(image.size[0] - 595) <= 1
    assert error_code(lambda: env.call("refs.page_image", {"ref_id": result["ref_id"], "page": 9})) == 2000
    docx = env.ingest("review.docx")
    assert error_code(lambda: env.call("refs.page_image", {"ref_id": docx["ref_id"], "page": 1})) == 2009
    png = env.ingest("chart.png")
    assert env.call("refs.page_image", {"ref_id": png["ref_id"], "page": 1})["png"] == env.paths["chart.png"]


def test_docx(env):
    result = env.ingest("review.docx")
    assert result["title"] == "Supplier Review"
    assert headings(result["outline"]) == ["Supplier Review", "Overview", "Regional figures", "Risks"]
    assert result["outline"][0]["children"][0]["children"][0]["title"] == "Regional figures"
    assert len(result["chunks"]) >= 5
    assert ["Supplier Review", "Overview", "Regional figures"] in [c["heading_path"] for c in result["chunks"]]
    assert len(result["tables"]) == 1 and re.fullmatch(r"c\d+-\d+/t1", result["tables"][0]["loc"])
    table = env.call("refs.table", {"ref_id": result["ref_id"], "loc": result["tables"][0]["loc"]})
    assert table["columns"] == REGION_COLUMNS and table["rows"][0] == NORTH and len(table["rows"]) == 5
    assert env.call("refs.table", {"ref_id": result["ref_id"], "loc": "t1"})["rows"] == table["rows"]
    assert len(result["images"]) == 1 and result["images"][0]["width"] == 160


def test_pptx(env):
    result = env.ingest("update.pptx")
    assert result["title"] == "Market Update" and result["pages"] == 5
    assert [n["title"] for n in result["outline"]] == ["Market Update", "Key messages", "Units by region", "Shipments table",
                                                       "Long commentary"]  # fmt: skip
    assert [n["loc"] for n in result["outline"]] == ["s1", "s2", "s3", "s4", "s5"]
    assert all(re.fullmatch(r"s\d+(/sh\d+)?", c["loc"]) for c in result["chunks"])
    slide2 = env.call("refs.read", {"ref_id": result["ref_id"], "loc": "s2"})["text"]
    assert "Freight costs fell 12%" in slide2 and slide2.endswith("Notes: Mention the supplier contract renewal.")
    assert env.call("refs.read", {"ref_id": result["ref_id"], "loc": "s2/sh0"})["text"] == "Mention the supplier contract renewal."
    chart, grid = result["tables"]
    assert chart["title"] == "Units by region" and re.fullmatch(r"s3/sh\d+/t1", chart["loc"])
    data = env.call("refs.table", {"ref_id": result["ref_id"], "loc": chart["loc"]})
    assert data["columns"] == ["Category", "2025", "2026"] and data["rows"] == [["North", 120, 150], ["South", 80, 91], ["East", 64, 77]]
    assert env.call("refs.table", {"ref_id": result["ref_id"], "loc": grid["loc"]})["rows"][0] == NORTH
    # The long slide is split by shape, and its notes are addressable.
    long_locs = [c["loc"] for c in result["chunks"] if c["loc"].startswith("s5/")]
    assert len(set(long_locs)) >= 3 and "s5/sh0" in long_locs and len(long_locs) > len(set(long_locs))
    assert [i["loc"] for i in result["images"]] == [next(loc for loc in [i["loc"] for i in result["images"]] if loc.startswith("s4/sh"))]
    assert error_code(lambda: env.call("refs.read", {"ref_id": result["ref_id"], "loc": "s9"})) == 2000


def test_xlsx(env):
    result = env.ingest("figures.xlsx")
    assert [(t["loc"], t["rows"], t["cols"]) for t in result["tables"]] == [("Q3!A1:D6", 6, 4), ("Data Log!B2:D62", 61, 3), ("name:Totals", 1, 4)]
    assert [n["title"] for n in result["outline"]] == ["Q3", "Data Log"] and result["outline"][0]["children"][0]["loc"] == "name:Totals"
    assert result["chunks"][0] == {"loc": "Q3!A1:D6", "heading_path": ["Q3"], "text": "Region\tQ1\tQ2\tQ3\nNorth\t120\t135\t150\nSouth\t80\t82\t91\nEast\t64\t70\t77\nWest\t101\t99\t113\nTotal\t365\t386\t431"}
    assert len([c for c in result["chunks"] if c["heading_path"] == ["Data Log"]]) >= 2
    rid = result["ref_id"]
    table = env.call("refs.table", {"ref_id": rid, "loc": "Q3!A1:D6"})
    assert table["columns"] == REGION_COLUMNS and table["rows"][0] == NORTH
    assert env.call("refs.table", {"ref_id": rid, "loc": "Q3!B2:C3"}) == {"columns": ["Q1", "Q2"], "rows": [[120, 135], [80, 82]], "loc": "Q3!B2:C3"}
    assert env.call("refs.table", {"ref_id": rid, "loc": "name:Totals"})["rows"] == [["Total", 365, 386, 431]]
    assert env.call("refs.read", {"ref_id": rid, "loc": "Data Log!B2:D3"})["text"].startswith("id\tlabel\tvalue\n1\titem 1 ")
    assert error_code(lambda: env.call("refs.table", {"ref_id": rid, "loc": "Nope!A1:B2"})) == 2000
    assert error_code(lambda: env.call("refs.table", {"ref_id": rid, "loc": "name:Nope"})) == 2000


def test_csv(env):
    result = env.ingest("units.csv")
    assert result["tables"] == [{"loc": "r1-41", "rows": 41, "cols": 4}]  # the ';' dialect was sniffed
    assert next(c["loc"] for c in result["chunks"]).startswith("r1-") and len(result["chunks"]) >= 2
    rid = result["ref_id"]
    table = env.call("refs.table", {"ref_id": rid, "loc": "r1-41"})
    assert table["columns"] == ["month", "units", "price", "note"] and len(table["rows"]) == 40
    assert isinstance(table["rows"][0][1], int) and isinstance(table["rows"][0][2], float) and table["rows"][0][3] == "batch 1; checked"
    part = env.call("refs.table", {"ref_id": rid, "loc": "r2-4:c1-2"})
    assert part["columns"] == ["month", "units"] and len(part["rows"]) == 3 and part["rows"][0][0] == "2026-01"
    assert env.call("refs.read", {"ref_id": rid, "loc": "r1-1"})["text"] == "month\tunits\tprice\tnote"


def test_markdown_and_text(env):
    md = env.ingest("handbook.md")
    assert md["title"] == "Logistics Handbook"
    assert headings(md["outline"]) == ["Logistics Handbook", "Receiving", "Dock schedule", "Shipping"]  # the fenced '#' line is not a heading
    assert ["Logistics Handbook", "Receiving", "Dock schedule"] in [c["heading_path"] for c in md["chunks"]]
    table = env.call("refs.table", {"ref_id": md["ref_id"], "loc": md["tables"][0]["loc"]})
    assert table == {"columns": ["Dock", "Open", "Close"], "rows": [["A", "06:00", "14:00"], ["B", "08:00", "18:00"]], "loc": md["tables"][0]["loc"]}
    text = make_fixtures.markdown_text()
    assert env.call("refs.read", {"ref_id": md["ref_id"], "loc": f"c0-{len(text)}"})["text"] == text
    txt = env.ingest("notes.txt")
    assert txt["title"] == "Meeting notes 4 November" and txt["outline"] == [] and len(txt["chunks"]) >= 3
    assert all(c["heading_path"] == [] for c in txt["chunks"])


def test_image(env):
    result = env.ingest("chart.png")
    assert result["chunks"] == [] and result["tables"] == []
    assert [(i["loc"], i["width"], i["height"]) for i in result["images"]] == [("full", 320, 200)]
    assert env.call("refs.read", {"ref_id": result["ref_id"], "loc": "full"}) == {"text": "", "loc": "full", "truncated": False}


def test_html_and_mhtml(env):
    html, mhtml = env.ingest("brief.html"), env.ingest("brief.mhtml")
    for result in (html, mhtml):
        assert result["title"] == "Freight Market Brief"
        assert headings(result["outline"]) == ["Freight Market Brief", "Rates by region", "Outlook"]
        text = env.call("refs.read", {"ref_id": result["ref_id"], "loc": ""})["text"]
        assert "SENTINEL" not in text and "color:red" not in text
        assert "Spot rates & contract rates." in text and "First point" in text
        table = env.call("refs.table", {"ref_id": result["ref_id"], "loc": result["tables"][0]["loc"]})
        assert table["columns"] == REGION_COLUMNS and table["rows"][0] == NORTH
        assert len(result["chunks"]) >= 3 and ["Freight Market Brief", "Outlook"] in [c["heading_path"] for c in result["chunks"]]
    assert html["chunks"] == mhtml["chunks"]


def check_mail_children(env, result, names):
    assert len(result["children"]) == len(names) and [c["name"] for c in result["child_refs"]] == names
    assert [c["ref_id"] for c in result["child_refs"]] == result["children"]
    first = result["child_refs"][0]
    assert first["kind"] == "csv" and (env.project / first["file"]).is_file()
    assert first["file"] == f"references/{first['version']}.csv" and first["ref_id"] == f"r-{first['version'][:8]}"
    assert (env.project / first["file"]).read_bytes() == make_fixtures.csv_bytes()
    n = first["loc"]
    assert env.call("refs.read", {"ref_id": result["ref_id"], "loc": f"{n}/r1-1"}) == {"text": "month\tunits\tprice\tnote", "loc": f"{n}/r1-1", "truncated": False}
    table = env.call("refs.table", {"ref_id": result["ref_id"], "loc": f"{n}/r1-41"})
    assert table["columns"][0] == "month" and len(table["rows"]) == 40 and table["loc"] == f"{n}/r1-41"
    # The child is a reference of its own: ingesting its file gives the same id, and it reads directly.
    child = env.call("refs.ingest", {"path": first["file"]})
    assert child["ref_id"] == first["ref_id"] and child["kind"] == "csv" and len(child["chunks"]) >= 2
    assert env.call("refs.read", {"ref_id": first["ref_id"], "loc": "r1-1"})["text"] == "month\tunits\tprice\tnote"
    for node in result["outline"]:
        assert re.fullmatch(r"att\d+/.+", node["loc"]) and env.call("refs.read", {"ref_id": result["ref_id"], "loc": node["loc"], "max_chars": 50})["text"]


def test_eml_with_attachments(env):
    result = env.ingest("mail.eml")
    assert result["title"] == "Q3 shipment numbers"
    text = env.call("refs.read", {"ref_id": result["ref_id"], "loc": ""})["text"]
    assert text.startswith("Subject: Q3 shipment numbers\nFrom: Ana Diaz <ana@example.invalid>\nTo: Ben Okoro <ben@example.invalid>\nDate: 2026-11-04T11:32:00+00:00\n\nHi Ben,")
    check_mail_children(env, result, ["units.csv", "notes.txt"])
    assert [c["loc"] for c in result["child_refs"]] == ["att1", "att3"]  # att2 is the unsupported .bin
    assert [w["rule"] for w in result["warnings"]] == ["refs.attachment-skipped"]
    assert error_code(lambda: env.call("refs.read", {"ref_id": result["ref_id"], "loc": "att2/c0-5"})) == 2000


def test_msg_with_attachments(env):
    result = env.ingest("mail.msg")
    assert result["title"] == "Units for October" and result["warnings"] == []
    text = env.call("refs.read", {"ref_id": result["ref_id"], "loc": ""})["text"]
    assert text.startswith("Subject: Units for October\nFrom: Carla Rossi <carla@example.invalid>\n"
                           "To: Ben Okoro <ben@example.invalid>; Dana Lee <dana@example.invalid>\nDate: 2026-11-04T11:32:00Z\n\nHello team,")  # fmt: skip
    assert "\r" not in text and len(result["chunks"]) >= 2
    check_mail_children(env, result, ["units.csv", "big.txt"])
    assert result["child_refs"][1]["kind"] == "text"  # a stream above the mini-stream cutoff


def test_msg_rtf_only_body(env):
    result = env.ingest("mail-rtf.msg")
    text = env.call("refs.read", {"ref_id": result["ref_id"], "loc": ""})["text"]
    assert text.endswith("Budget approved for café refit.\nSecond line with bold text.") and "To: Ben Okoro" in text
    assert [(w["rule"], w["severity"]) for w in result["warnings"]] == [("refs.msg-rtf-body", "info")]


def test_msg_reader_details(tmp_path):
    import olefile
    from cfb_writer import build_msg

    from present_kernel.refs.extract import msg

    # The published example of MS-OXRTFCP (compressed RTF).
    example = bytes.fromhex("2d0000002b0000004c5a4675f1c5c7a703000a007263706731323542320af32068656c090020627705b06c647d0a800fa0")
    assert msg.rtf_decompress(example) == b"{\\rtf1\\ansi\\ansicpg1252\\pard hello world}\r\n"
    assert len(msg._RTF_PREBUF) == 207
    wrapped = (rb"{\rtf1\ansi\fromhtml1 \deff0{\fonttbl{\f0 Arial;}}{\*\htmltag19 <html>}{\*\htmltag64 <p>}\htmlrtf {\htmlrtf0 Hello "
               rb"{\*\htmltag84 <b>}\htmlrtf {\b \htmlrtf0 world{\*\htmltag92 </b>}\htmlrtf }\htmlrtf0 \htmlrtf\par}\htmlrtf0"
               rb"{\*\htmltag72 </p>}{\*\htmltag27 </html>}}")  # fmt: skip
    assert msg.rtf_to_text(wrapped) == ("<html><p>Hello <b>world</b></p></html>", True)
    # ANSI (001E) string properties, display-to without recipient storages, no attachments.
    path = tmp_path / "ansi.msg"
    path.write_bytes(build_msg(subject="Größe", sender_name="Eli", sender_email="eli@example.invalid", to="Ben; Dana",
                               body="Plain ANSI body", submit_filetime=make_fixtures.FILETIME_2026_11_04, unicode=False))  # fmt: skip
    ole = olefile.OleFileIO(str(path))
    try:
        data = msg.read_msg(ole)
    finally:
        ole.close()
    assert data["subject"] == "Größe" and data["from"] == "Eli <eli@example.invalid>" and data["to"] == "Ben; Dana"
    assert data["date"] == "2026-11-04T11:32:00Z" and data["body"] == "Plain ANSI body" and data["attachments"] == []


# ---- errors -------------------------------------------------------------------------------------
def test_ingest_errors(env):
    (env.project / "references" / "archive.zip").write_bytes(b"PK\x03\x04")
    (env.project / "references" / "broken.pdf").write_bytes(b"this is not a pdf")
    (env.project / "references" / "broken.msg").write_bytes(b"not ole")
    (env.project / "elsewhere.txt").write_text("x")
    assert error_code(lambda: env.call("refs.ingest", {"path": "references/archive.zip"})) == 2009
    assert error_code(lambda: env.call("refs.ingest", {"path": "references/broken.pdf"})) == 2009
    assert error_code(lambda: env.call("refs.ingest", {"path": "references/broken.msg"})) == 2009
    assert error_code(lambda: env.call("refs.ingest", {"path": "../outside.pdf"})) == 2001
    assert error_code(lambda: env.call("refs.ingest", {"path": "references/../../outside.pdf"})) == 2001
    assert error_code(lambda: env.call("refs.ingest", {"path": "references/missing.pdf"})) == 2000
    assert error_code(lambda: env.call("refs.ingest", {"path": "elsewhere.txt"})) == -32602
    assert error_code(lambda: env.call("refs.ingest", {"path": env.paths["notes.txt"], "ref_id": "bad id"})) == -32602
    assert error_code(lambda: env.call("refs.ingest", {})) == -32602
    assert error_code(lambda: env.call("refs.read", {"ref_id": "r-unknown1", "loc": "c0-5"})) == 2000
    assert error_code(lambda: env.call("refs.table", {"ref_id": env.ingest("notes.txt")["ref_id"], "loc": "t1"})) == 2000
    assert error_code(lambda: env.call("refs.read", {"cite": "nonsense"})) == -32602


def test_ref_id_param_and_cite_param(env):
    result = env.call("refs.ingest", {"path": env.paths["notes.txt"], "ref_id": "r-notes"})
    assert result["ref_id"] == "r-notes" and result["images"] == []
    loc = result["chunks"][0]["loc"]
    cite = f"ref:r-notes@{result['version']}#{loc}"
    assert env.call("refs.read", {"cite": cite})["text"] == result["chunks"][0]["text"]
    assert env.call("refs.read", {"cite": "r-notes#c0-7"})["text"] == "Meeting"
    # Files without a title of their own take the display name the core passes, else a generic one.
    named = env.call("refs.ingest", {"path": env.paths["units.csv"], "ref_id": "r-units2", "name": "Units 2026.csv"})
    assert named["title"] == "Units 2026.csv" and env.ingest("units.csv")["title"] == "month, units, price, note"
    assert env.ingest("chart.png")["title"] == "Image 320×200" and env.ingest("figures.xlsx")["title"] == "Q3, Data Log"


# ---- the reader and the manifest ------------------------------------------------------------------
def test_cite_parsing():
    assert locs.parse_cite("ref:r-q3rep@3f9a1c2b7e01#p11:c120-188") == locs.Cite("r-q3rep", "3f9a1c2b7e01", "p11:c120-188")
    assert locs.parse_cite("ref:r-q3rep@3f9a1c2b7e01#Q3 #2!B2:F14").loc == "Q3 #2!B2:F14"
    assert locs.parse_cite(" r-q3rep ") == locs.Cite("r-q3rep", None, "")
    assert str(locs.parse_cite("ref:r-q3rep#att2/r1-3")) == "ref:r-q3rep#att2/r1-3"
    for bad in ("", "ref:", "q3rep#p1", "ref:r-Q3#p1", "ref:r-ab#p1"):
        with pytest.raises(RpcError):
            locs.parse_cite(bad)
    assert locs.parse_sheet_range("Q3!B2:F14") == ("Q3", (2, 2, 14, 6)) and locs.parse_sheet_range("'Data Log'!$A$1") == ("Data Log", (1, 1, 1, 1))
    assert locs.a1_range(2, 2, 14, 28) == "B2:AB14" and locs.split_table("p12/t1") == ("p12", 1) and locs.split_attachment("att2/r1-3") == (2, "r1-3")


def test_reader_resolves_versions_through_the_manifest(tmp_path):
    project = tmp_path / "project"
    old, new = tmp_path / "old.txt", tmp_path / "new.txt"
    old.write_text("First version of the memo.\n")
    new.write_text("Second version of the memo, revised.\n")
    old_rel, new_rel = add_reference(project, old), add_reference(project, new)
    table_rel = add_reference(project, FIXTURES / "units.csv")
    (project / "references" / "manifest.toml").write_text(
        f'[[refs]]\nid = "r-memo"\nfile = "{new_rel}"\nkind = "text"\ntitle = "Memo"\n'
        f'original_path = "C:/Users/x/Memo final.txt"\nversion = "{Path(new_rel).stem}"\n'
        f'previous_versions = ["{Path(old_rel).stem}"]\nparent = ""\nsource = "file"\n\n'
        f'[[refs]]\nid = "r-units"\nfile = "{table_rel}"\nkind = "csv"\ntitle = "Units"\nversion = "{Path(table_rel).stem}"\n'
        "previous_versions = []\n", encoding="utf-8")  # fmt: skip
    refs = ProjectRefs(project)  # nothing ingested, no cache: the reader extracts on demand
    assert refs.text("ref:r-memo#c0-6") == "Second"
    assert refs.text(f"ref:r-memo@{Path(new_rel).stem}#c0-6") == "Second"
    assert refs.text(f"ref:r-memo@{Path(old_rel).stem}#c0-5") == "First"  # old citations still resolve
    assert refs.read("r-memo", "c0-100", max_chars=6) == {"text": "Second", "loc": "c0-6", "truncated": True}
    table = refs.table("ref:r-units#r1-41")
    assert set(table) == {"columns", "rows", "loc"} and table["columns"][0] == "month"
    with pytest.raises(RpcError) as info:
        refs.text("ref:r-memo@000000000000#c0-5")
    assert info.value.code == 2000
    with pytest.raises(RpcError) as info:
        refs.text("ref:r-nothere#c0-5")
    assert info.value.code == 2000
    # The cache is disposable.
    shutil.rmtree(project / ".present")
    assert ProjectRefs(project).text("ref:r-memo#c0-6") == "Second"


def test_table_header_rule():
    from present_kernel.refs.model import grid_to_table

    assert grid_to_table([["a", "b"], ["1", "x"]], "t")["columns"] == ["a", "b"]
    assert grid_to_table([["a", "b"], ["1", "x"]], "t")["rows"] == [[1, "x"]]
    assert grid_to_table([["a", 2025], [1, 2]], "t") == {"columns": ["col1", "col2"], "rows": [["a", 2025], [1, 2]], "loc": "t"}
    assert grid_to_table([["1", "2"], ["3", "4"]], "t")["columns"] == ["col1", "col2"]
    assert grid_to_table([["a", ""], ["x", "007"]], "t") == {"columns": ["col1", "col2"], "rows": [["a", None], ["x", "007"]], "loc": "t"}
    assert grid_to_table([], "t") == {"columns": [], "rows": [], "loc": "t"}


# ---- chunker --------------------------------------------------------------------------------------
def test_chunker_prefers_paragraph_then_sentence_boundaries():
    paragraphs = [("Word " * 60).strip() + "." for _ in range(8)]  # 300 characters each
    text = "\n\n".join(paragraphs)
    breaks = chunker.paragraph_breaks(text)
    spans = chunker.chunk_spans(text, 0, len(text), breaks)
    assert all(chunker.MIN_CHARS <= b - a <= chunker.MAX_CHARS for a, b in spans[:-1]) and spans[-1][1] == len(text)
    assert all(text[b - 1] == "." for a, b in spans)  # cut at paragraph ends
    assert all(0 < prev[1] - nxt[0] <= chunker.OVERLAP for prev, nxt in itertools.pairwise(spans))
    assert all(nxt[0] == 0 or text[nxt[0] - 1].isspace() for nxt in spans)  # chunks start at a word
    long = "Sentence number one is here. " * 100  # one paragraph of 2900 characters
    spans = chunker.chunk_spans(long, 0, len(long), [])
    assert len(spans) >= 3 and all(long[a:b].endswith(".") for a, b in spans)
    blob = "x" * 5000
    spans = chunker.chunk_spans(blob, 0, len(blob), [])
    assert spans[0] == (0, chunker.MAX_CHARS) and all(b - a <= chunker.MAX_CHARS for a, b in spans) and spans[-1][1] == 5000
    assert chunker.chunk_spans("   ", 0, 3, []) == [] and chunker.chunk_spans("short", 0, 5, []) == [(0, 5)]
    sections = chunker.sections_from_headings(100, [(10, 1, "A"), (40, 2, "B"), (70, 1, "C")])
    assert [(s.start, s.end, s.heading_path) for s in sections] == [(0, 10, []), (10, 40, ["A"]), (40, 70, ["A", "B"]), (70, 100, ["C"])]
    assert chunker.row_groups(["a" * 700, "b" * 700, "c" * 100, "d" * 1500]) == [(0, 0), (1, 2), (3, 3)]


# ---- performance ------------------------------------------------------------------------------------
def test_thirty_page_pdf_ingests_in_seconds(tmp_path):
    source = tmp_path / "long.pdf"
    make_fixtures.build_pdf(source, pages_of_text=30, image_only_page=False, seed=9)
    project = tmp_path / "project"
    rel = add_reference(project, source)
    kernel = Kernel(project=project, home=tmp_path / "home")
    try:
        started = time.perf_counter()
        result = kernel.dispatcher.call("refs.ingest", {"path": rel})
        elapsed = time.perf_counter() - started
    finally:
        kernel.close()
    assert result["pages"] >= 30 and len(result["chunks"]) >= 90 and len(headings(result["outline"])) >= 60
    assert elapsed < 5.0, f"ingest took {elapsed:.2f}s"


def test_methods_are_registered(env):
    methods = env.kernel.dispatcher.methods()
    for name in ("refs.ingest", "refs.read", "refs.table", "refs.page_image", "embed.texts", "analyst.scan", "analyst.import"):
        assert name in methods
