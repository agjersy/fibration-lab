"""Generate the reference fixtures for the W9 tests (one file per reference kind).

Run from ``python/kernel``::

    uv run python tests/fixtures/refs/make_fixtures.py

Everything is synthetic and produced by this script; nothing is copied from elsewhere. The
tests import :func:`build_all` and the individual builders (the 30-page PDF is built at test time).
"""

from __future__ import annotations

import csv
import io
import random
import sys
from email.message import EmailMessage
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))

from cfb_writer import build_msg, rtf_compress_literal  # noqa: E402

WORDS = ["revenue", "margin", "quarter", "growth", "region", "customer", "pipeline", "forecast", "inventory", "supplier", "contract", "renewal", "pricing", "volume", "channel", "partner", "backlog", "churn", "retention", "capacity", "utilization", "shipment", "order", "invoice", "budget", "variance", "headcount", "productivity", "logistics", "warehouse", "freight", "demand", "supply", "outlook", "guidance"]

TABLE = [
    ["Region", "Q1", "Q2", "Q3"],
    ["North", "120", "135", "150"],
    ["South", "80", "82", "91"],
    ["East", "64", "70", "77"],
    ["West", "101", "99", "113"],
    ["Total", "365", "386", "431"],
]


def sentence(rng: random.Random) -> str:
    n = rng.randint(8, 16)
    words = [rng.choice(WORDS) for _ in range(n)]
    return words[0].capitalize() + " " + " ".join(words[1:]) + "."


def paragraph(rng: random.Random, sentences: int = 5) -> str:
    return " ".join(sentence(rng) for _ in range(sentences))


def png_bytes(width: int = 320, height: int = 200, seed: int = 1) -> bytes:
    from PIL import Image, ImageDraw

    img = Image.new("RGB", (width, height), (245, 245, 250))
    draw = ImageDraw.Draw(img)
    rng = random.Random(seed)
    for i in range(6):
        x = 20 + i * (width - 40) // 6
        h = rng.randint(5, max(6, height - 40))
        draw.rectangle([x, height - 20 - h, x + (width - 40) // 8, height - 20], fill=(40 + i * 30, 90, 160))
    out = io.BytesIO()
    img.save(out, format="PNG")
    return out.getvalue()


def csv_bytes() -> bytes:
    out = io.StringIO()
    w = csv.writer(out, delimiter=";", lineterminator="\n")
    w.writerow(["month", "units", "price", "note"])
    rng = random.Random(7)
    for i in range(1, 41):
        w.writerow([f"2026-{(i - 1) % 12 + 1:02d}", rng.randint(10, 900), f"{rng.uniform(1, 99):.2f}", f"batch {i}; checked"])
    return out.getvalue().encode("utf-8")


# ---------------------------------------------------------------------------------------- PDF
def build_pdf(path: Path, pages_of_text: int = 3, with_bookmarks: bool = False, image_only_page: bool = True,
              seed: int = 3) -> None:
    from reportlab.lib import colors
    from reportlab.lib.pagesizes import A4
    from reportlab.lib.styles import getSampleStyleSheet
    from reportlab.platypus import Image, PageBreak, Paragraph, SimpleDocTemplate, Spacer, Table, TableStyle

    rng = random.Random(seed)
    styles = getSampleStyleSheet()

    class Doc(SimpleDocTemplate):
        def afterFlowable(self, flowable):
            if with_bookmarks and isinstance(flowable, Paragraph) and flowable.style.name in ("Heading1", "Heading2"):
                key = f"bk{self.seq.nextf('bk')}"
                self.canv.bookmarkPage(key)
                self.canv.addOutlineEntry(flowable.getPlainText(), key, 0 if flowable.style.name == "Heading1" else 1)

    doc = Doc(str(path), pagesize=A4, title="Quarterly Operations Report", author="Fixture", invariant=1)
    story: list = []
    for page in range(1, pages_of_text + 1):
        story.append(Paragraph(f"Chapter {page}: {WORDS[page % len(WORDS)].capitalize()} review", styles["Heading1"]))
        for _ in range(3):
            story.append(Paragraph(paragraph(rng, 4), styles["BodyText"]))
        story.append(Paragraph(f"Section {page}.1 Details", styles["Heading2"]))
        for _ in range(3):
            story.append(Paragraph(paragraph(rng, 4), styles["BodyText"]))
        if page == 2:
            story.append(Paragraph("Section 2.2 Regional figures", styles["Heading2"]))
            table = Table(TABLE, colWidths=[110, 80, 80, 80])
            table.setStyle(TableStyle([("GRID", (0, 0), (-1, -1), 0.5, colors.grey), ("ALIGN", (1, 0), (-1, -1), "RIGHT")]))
            story += [table, Spacer(1, 12), Paragraph(paragraph(rng, 3), styles["BodyText"])]
        if page == 1:
            story += [Image(io.BytesIO(png_bytes()), width=240, height=150), Spacer(1, 8)]
        story.append(PageBreak())
    if image_only_page:
        story += [Image(io.BytesIO(png_bytes(400, 300, seed=5)), width=400, height=300), PageBreak()]
    doc.build(story)


# --------------------------------------------------------------------------------------- DOCX
def build_docx(path: Path) -> None:
    from docx import Document

    rng = random.Random(11)
    doc = Document()
    doc.core_properties.title = "Supplier Review"
    doc.add_heading("Supplier Review", level=0)
    doc.add_paragraph(paragraph(rng, 3))
    doc.add_heading("Overview", level=1)
    for _ in range(4):
        doc.add_paragraph(paragraph(rng, 5))
    doc.add_heading("Regional figures", level=2)
    doc.add_paragraph("The table below lists shipments by region.")
    table = doc.add_table(rows=len(TABLE), cols=4)
    for r, row in enumerate(TABLE):
        for c, value in enumerate(row):
            table.cell(r, c).text = value
    doc.add_heading("Risks", level=1)
    for _ in range(3):
        doc.add_paragraph(paragraph(rng, 5))
    doc.add_picture(io.BytesIO(png_bytes(160, 100, seed=2)))
    doc.save(str(path))


# --------------------------------------------------------------------------------------- PPTX
def build_pptx(path: Path) -> None:
    from pptx import Presentation
    from pptx.chart.data import CategoryChartData
    from pptx.enum.chart import XL_CHART_TYPE
    from pptx.util import Inches

    rng = random.Random(13)
    prs = Presentation()
    s1 = prs.slides.add_slide(prs.slide_layouts[0])
    s1.shapes.title.text = "Market Update"
    s1.placeholders[1].text = "Prepared for the steering group"
    s2 = prs.slides.add_slide(prs.slide_layouts[1])
    s2.shapes.title.text = "Key messages"
    s2.placeholders[1].text = "Demand is stable\nFreight costs fell 12%\nInventory turns improved"
    s2.notes_slide.notes_text_frame.text = "Mention the supplier contract renewal."
    s3 = prs.slides.add_slide(prs.slide_layouts[5])
    s3.shapes.title.text = "Units by region"
    data = CategoryChartData()
    data.categories = ["North", "South", "East"]
    data.add_series("2025", (120, 80, 64))
    data.add_series("2026", (150, 91, 77))
    s3.shapes.add_chart(XL_CHART_TYPE.COLUMN_CLUSTERED, Inches(1), Inches(1.5), Inches(6), Inches(4), data)
    s4 = prs.slides.add_slide(prs.slide_layouts[5])
    s4.shapes.title.text = "Shipments table"
    shape = s4.shapes.add_table(len(TABLE), 4, Inches(1), Inches(1.5), Inches(7), Inches(3))
    for r, row in enumerate(TABLE):
        for c, value in enumerate(row):
            shape.table.cell(r, c).text = value
    s4.shapes.add_picture(io.BytesIO(png_bytes(160, 100, seed=4)), Inches(1), Inches(5))
    s5 = prs.slides.add_slide(prs.slide_layouts[1])
    s5.shapes.title.text = "Long commentary"
    s5.placeholders[1].text = "\n".join(paragraph(rng, 4) for _ in range(5))
    box = s5.shapes.add_textbox(Inches(1), Inches(6), Inches(6), Inches(1))
    box.text_frame.text = paragraph(rng, 3)
    s5.notes_slide.notes_text_frame.text = "Long slide notes: " + paragraph(rng, 2)
    prs.save(str(path))


# --------------------------------------------------------------------------------------- XLSX
def build_xlsx(path: Path) -> None:
    from openpyxl import Workbook
    from openpyxl.workbook.defined_name import DefinedName

    wb = Workbook()
    ws = wb.active
    ws.title = "Q3"
    for row in TABLE:
        ws.append([row[0], *[int(v) for v in row[1:]]] if row[0] != "Region" else row)
    data = wb.create_sheet("Data Log")
    data["B2"], data["C2"], data["D2"] = "id", "label", "value"
    rng = random.Random(17)
    for i in range(60):
        data.cell(row=3 + i, column=2, value=i + 1)
        data.cell(row=3 + i, column=3, value=f"item {i + 1} {rng.choice(WORDS)} {rng.choice(WORDS)}")
        data.cell(row=3 + i, column=4, value=round(rng.uniform(0, 1000), 2))
    wb.defined_names["Totals"] = DefinedName("Totals", attr_text="'Q3'!$A$6:$D$6")
    wb.create_sheet("Empty")
    wb.save(str(path))


# ------------------------------------------------------------------------------ text formats
def markdown_text() -> str:
    rng = random.Random(19)
    parts = ["# Logistics Handbook", "", paragraph(rng, 3), "", "## Receiving", ""]
    for _ in range(4):
        parts += [paragraph(rng, 5), ""]
    parts += ["```", "# not a heading inside a code fence", "```", "", "### Dock schedule", ""]
    parts += ["| Dock | Open | Close |", "|---|---|---|", "| A | 06:00 | 14:00 |", "| B | 08:00 | 18:00 |", ""]
    parts += ["## Shipping", ""]
    for _ in range(3):
        parts += [paragraph(rng, 5), ""]
    return "\n".join(parts)


def plain_text() -> str:
    rng = random.Random(23)
    return "Meeting notes 4 November\n\n" + "\n\n".join(paragraph(rng, 6) for _ in range(5)) + "\n"


def html_text() -> str:
    rng = random.Random(29)
    rows = "".join("<tr>" + "".join(f"<t{'h' if i == 0 else 'd'}>{v}</t{'h' if i == 0 else 'd'}>" for v in row) + "</tr>"
                   for i, row in enumerate(TABLE))  # fmt: skip
    body = "".join(f"<p>{paragraph(rng, 5)}</p>" for _ in range(4))
    more = "".join(f"<p>{paragraph(rng, 4)}</p>" for _ in range(2))
    return (
        "<!doctype html><html><head><title>Freight Market Brief</title><style>p{color:red}</style>"
        "<script>var hidden = 'SCRIPT-SENTINEL';</script></head><body>"
        "<header><div>Site banner HEADER-SENTINEL</div></header><nav><a href='/'>Home NAV-SENTINEL</a></nav>"
        f"<main><h1>Freight Market Brief</h1>{body}<h2>Rates by region</h2><p>Spot rates &amp; contract rates.</p>"
        f"<table>{rows}</table><h2>Outlook</h2>{more}<ul><li>First point</li><li>Second point</li></ul></main>"
        "<aside>Related links ASIDE-SENTINEL</aside><footer>Copyright FOOTER-SENTINEL</footer></body></html>"
    )


def mhtml_bytes() -> bytes:
    msg = EmailMessage()
    msg["Subject"] = "Saved page"
    msg["MIME-Version"] = "1.0"
    msg.set_content("placeholder")
    msg.clear_content()
    msg.make_related()
    page = EmailMessage()
    page.set_content(html_text(), subtype="html", cte="quoted-printable")
    page["Content-Location"] = "https://example.invalid/brief.html"
    msg.attach(page)
    img = EmailMessage()
    img.set_content(png_bytes(40, 30), maintype="image", subtype="png")
    img["Content-Location"] = "https://example.invalid/chart.png"
    msg.attach(img)
    return msg.as_bytes()


def eml_bytes() -> bytes:
    rng = random.Random(31)
    msg = EmailMessage()
    msg["Subject"] = "Q3 shipment numbers"
    msg["From"] = "Ana Diaz <ana@example.invalid>"
    msg["To"] = "Ben Okoro <ben@example.invalid>"
    msg["Date"] = "Wed, 04 Nov 2026 11:32:00 +0000"
    msg.set_content("Hi Ben,\n\nAttached are the monthly units.\n\n" + "\n\n".join(paragraph(rng, 5) for _ in range(3)) + "\n\nAna\n")
    msg.add_attachment(csv_bytes(), maintype="text", subtype="csv", filename="units.csv")
    msg.add_attachment(b"\x00\x01binary", maintype="application", subtype="octet-stream", filename="blob.bin")
    msg.add_attachment(plain_text().encode(), maintype="text", subtype="plain", filename="notes.txt")
    return msg.as_bytes()


FILETIME_2026_11_04 = (1793791920 + 11644473600) * 10_000_000  # 2026-11-04T11:32:00Z


def msg_bytes() -> bytes:
    rng = random.Random(37)
    body = "Hello team,\r\n\r\nPlease find the units attached.\r\n\r\n" + "\r\n\r\n".join(paragraph(rng, 5) for _ in range(3))
    return build_msg(subject="Units for October", sender_name="Carla Rossi", sender_email="carla@example.invalid",
                     to="Ben Okoro; Dana Lee", body=body, submit_filetime=FILETIME_2026_11_04,
                     attachments=[("units.csv", csv_bytes()), ("big.txt", (plain_text() * 3).encode())],
                     recipients=[("Ben Okoro", "ben@example.invalid"), ("Dana Lee", "dana@example.invalid")])  # fmt: skip


def msg_rtf_bytes() -> bytes:
    rtf = (rb"{\rtf1\ansi\ansicpg1252\deff0{\fonttbl{\f0 Arial;}}{\colortbl;\red0\green0\blue0;}"
           rb"\pard Budget approved for caf\'e9 refit.\par Second line with {\b bold} text.\par}")
    return build_msg(subject="RTF only", sender_name="Eli Stone", sender_email="eli@example.invalid", to="Ben Okoro",
                     body=None, submit_filetime=FILETIME_2026_11_04, unicode=False,
                     rtf_compressed=rtf_compress_literal(rtf))  # fmt: skip


def build_all(out: Path) -> dict[str, Path]:
    out.mkdir(parents=True, exist_ok=True)
    files: dict[str, Path] = {}

    def write(name: str, data: bytes) -> None:
        (out / name).write_bytes(data)
        files[name] = out / name

    for name, builder in (("report.pdf", build_pdf), ("review.docx", build_docx), ("update.pptx", build_pptx),
                          ("figures.xlsx", build_xlsx)):  # fmt: skip
        builder(out / name)
        files[name] = out / name
    build_pdf(out / "bookmarks.pdf", pages_of_text=2, with_bookmarks=True, image_only_page=False, seed=5)
    files["bookmarks.pdf"] = out / "bookmarks.pdf"
    write("units.csv", csv_bytes())
    write("handbook.md", markdown_text().encode())
    write("notes.txt", plain_text().encode())
    write("chart.png", png_bytes())
    write("mail.eml", eml_bytes())
    write("mail.msg", msg_bytes())
    write("mail-rtf.msg", msg_rtf_bytes())
    write("brief.html", html_text().encode())
    write("brief.mhtml", mhtml_bytes())
    return files


if __name__ == "__main__":
    for name in sorted(build_all(HERE)):
        print(name)
