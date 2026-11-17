#!/usr/bin/env python3
"""Generate the evaluation fixtures under evals/tasks/*/fixture (BUILD-SPEC W14).

Run with the kernel environment:  cd python/kernel && uv run python ../../evals/make_fixtures.py

Everything is synthetic and deterministic: no file here comes from outside the repository.
"""

from __future__ import annotations

import json
import random
from email.message import EmailMessage
from pathlib import Path

import openpyxl
from PIL import Image, ImageDraw
from reportlab.lib import colors
from reportlab.lib.pagesizes import A4
from reportlab.lib.styles import getSampleStyleSheet
from reportlab.platypus import PageBreak, Paragraph, SimpleDocTemplate, Spacer, Table, TableStyle

ROOT = Path(__file__).resolve().parent / "tasks"
REGIONS = ["EMEA", "Americas", "APAC", "LATAM"]
PRODUCTS = ["Atlas", "Borealis", "Cirrus", "Delta", "Echo", "Fjord", "Glacier", "Harbor"]
QUARTERS = ["Q1", "Q2", "Q3", "Q4"]


def fixture(task: str, template: str) -> Path:
    path = ROOT / task / "fixture"
    (path / "sources").mkdir(parents=True, exist_ok=True)
    (path / "TEMPLATE").write_text(template + "\n", encoding="utf-8")
    return path


# ---- sources -----------------------------------------------------------------------------------


def annual_report(path: Path) -> None:
    rng = random.Random(7)
    styles = getSampleStyleSheet()
    story = [Paragraph("Northwind Industrial: Annual Report 2025", styles["Title"]), Spacer(1, 12),
             Paragraph("This report is a synthetic test fixture. All companies and figures are invented.", styles["Italic"]), PageBreak()]
    facts = [
        ("Group performance", "Group revenue rose 8.4% to EUR 2,418 million in 2025, from EUR 2,231 million in 2024. "
         "Operating profit reached EUR 302 million, an operating margin of 12.5% compared with 11.1% a year earlier. "
         "Free cash flow was EUR 214 million. The board proposes a dividend of EUR 1.20 per share."),
        ("Regional review", "EMEA remained the largest region with revenue of EUR 1,015 million, up 5.2%. "
         "The Americas grew 12.9% to EUR 846 million on strong demand for automation systems. "
         "APAC revenue was EUR 421 million, an increase of 9.6%, while LATAM contributed EUR 136 million."),
        ("Segments", "Automation Systems generated EUR 1,102 million of revenue with a segment margin of 15.8%. "
         "Process Equipment reported revenue of EUR 874 million and a margin of 10.4%. "
         "Services grew fastest, by 14.3% to EUR 442 million, and now represents 18.3% of group revenue."),
        ("People and safety", "The group employed 11,640 people at year end, 380 more than in 2024. "
         "The lost-time injury rate fell to 1.8 per million hours worked from 2.4. "
         "Employee engagement in the annual survey was 78%."),
        ("Sustainability", "Scope 1 and 2 emissions fell 11% to 96,400 tonnes of CO2 equivalent. "
         "Renewable electricity covered 64% of consumption, up from 52%. "
         "The group targets a 50% reduction in operational emissions by 2030 against a 2020 baseline."),
        ("Outlook", "For 2026 the group expects revenue growth of 5% to 7% and an operating margin between 12.5% and 13.5%. "
         "Capital expenditure is planned at about EUR 130 million, mainly for capacity in the Americas."),
    ]
    filler = ("Management continued to simplify the portfolio and to invest in digital tools for field service. "
              "Customer demand was resilient across most end markets, although order intake slowed in the fourth quarter. ")
    for i, (heading, text) in enumerate(facts):
        story.append(Paragraph(heading, styles["Heading1"]))
        story.append(Paragraph(text, styles["BodyText"]))
        for _ in range(5):
            story.append(Paragraph(filler * rng.randint(2, 4), styles["BodyText"]))
            story.append(Spacer(1, 6))
        if i == 1:
            rows = [["Region", "Revenue 2025", "Revenue 2024", "Change"], ["EMEA", "1,015", "965", "5.2%"],
                    ["Americas", "846", "749", "12.9%"], ["APAC", "421", "384", "9.6%"], ["LATAM", "136", "133", "2.3%"]]
            table = Table(rows)
            table.setStyle(TableStyle([("GRID", (0, 0), (-1, -1), 0.5, colors.grey), ("BACKGROUND", (0, 0), (-1, 0), colors.lightgrey)]))
            story += [Spacer(1, 8), Paragraph("Revenue by region, EUR million", styles["Heading3"]), table]
        story.append(PageBreak())
        for _ in range(3):  # pad to about thirty pages
            for _ in range(7):
                story.append(Paragraph(filler * 4, styles["BodyText"]))
                story.append(Spacer(1, 6))
            story.append(PageBreak())
    SimpleDocTemplate(str(path), pagesize=A4, title="Northwind Industrial Annual Report 2025", invariant=1).build(story)


def sales_workbook(path: Path) -> None:
    rng = random.Random(11)
    wb = openpyxl.Workbook()
    ws = wb.active
    ws.title = "Sales"
    ws.append(["Region", "Product", "Quarter", "Revenue", "Units"])
    for region in REGIONS:
        for product in PRODUCTS:
            base = rng.randint(40, 400)
            for qi, quarter in enumerate(QUARTERS):
                revenue = round(base * (1 + 0.06 * qi) * rng.uniform(0.9, 1.1), 1)
                ws.append([region, product, quarter, revenue, int(revenue * rng.uniform(2, 4))])
    notes = wb.create_sheet("Notes")
    notes.append(["Synthetic sales data for evaluation tasks. Revenue in EUR thousand."])
    import datetime

    wb.properties.created = wb.properties.modified = datetime.datetime(2026, 1, 1)
    wb.save(path)


def email_thread(path: Path, attachment: bytes) -> None:
    msg = EmailMessage()
    msg["Subject"] = "RE: Project Lighthouse status before steering committee"
    msg["From"] = "Dana Whitfield <dana.whitfield@example.com>"
    msg["To"] = "Steering committee <lighthouse-steerco@example.com>"
    msg["Date"] = "Mon, 02 Nov 2026 09:14:00 +0000"
    msg.set_content(
        "All,\n\nStatus ahead of Thursday's steering committee:\n\n"
        "1. Migration: 14 of 22 plants are live on the new platform; the remaining 8 follow by the end of February.\n"
        "2. Budget: we have spent EUR 3.4 million of the EUR 5.0 million approved, which is 68% with 64% of scope delivered.\n"
        "3. Risk: the Lyon cut-over slipped by three weeks because of a vendor delay. No impact on the end date so far.\n"
        "4. Decision needed: approve EUR 250,000 from contingency for parallel run support in Q1.\n\n"
        "The attached deck has last month's detail.\n\nDana\n\n"
        "-----Original Message-----\nFrom: Priya Raman\nSent: Friday, 30 October 2026\n\n"
        "Dana, can you confirm the plant count and the spend to date before Thursday? Thanks, Priya\n")
    msg.add_attachment(attachment, maintype="application", subtype="vnd.openxmlformats-officedocument.presentationml.presentation",
                       filename="lighthouse-october.pptx")
    path.write_bytes(msg.as_bytes())


def october_deck() -> bytes:
    import io

    from pptx import Presentation

    prs = Presentation()
    for title, lines in (("Project Lighthouse: October", ["Monthly status"]),
                         ("Migration progress", ["11 of 22 plants live at the end of October", "Lyon and Turin scheduled for November"]),
                         ("Budget", ["EUR 3.1 million spent of EUR 5.0 million", "Contingency of EUR 400,000 untouched"])):
        slide = prs.slides.add_slide(prs.slide_layouts[1])
        slide.shapes.title.text = title
        slide.placeholders[1].text = "\n".join(lines)
    buf = io.BytesIO()
    prs.save(buf)
    return buf.getvalue()


PLAN = """# Programme Meridian: delivery plan

Synthetic test fixture.

## Phases

The programme runs in four phases. Discovery runs from 2026-01 to 2026-03 and produces the target operating model.
Design runs from 2026-03 to 2026-06. Build runs from 2026-06 to 2026-11, in two releases.
Rollout runs from 2026-11 to 2027-03, region by region.

## Milestones

- 2026-03-31: operating model approved by the board
- 2026-06-15: design freeze
- 2026-09-01: release 1 in pilot
- 2026-11-15: release 2 complete
- 2027-03-31: all regions live

## Change request process

1. A requester submits a change request.
2. The product owner assesses impact within five working days.
3. If the impact is above EUR 50,000 or two weeks, the steering committee decides; otherwise the product owner decides.
4. Approved changes are scheduled into the next release; rejected changes are returned with a reason.
"""


# ---- starting decks ----------------------------------------------------------------------------


def T(*paras) -> dict:
    out = []
    for p in paras:
        out.append({"runs": [{"t": p}]} if isinstance(p, str) else p)
    return {"paragraphs": out}


def slide(sid: str, layout: str, title: str, body=None, **extra) -> dict:
    doc = {"id": sid, "kind": "layout", "layout": layout,
           "placeholders": {"title": {"type": "text", "id": f"e-t{sid[2:]}", "text": T(title)}}, "elements": []}
    if body is not None:
        doc["placeholders"]["body"] = body if isinstance(body, dict) and "type" in body else {"type": "text", "id": f"e-b{sid[2:]}", "text": T(*body)}
    doc.update(extra)
    return doc


def write_deck(fix: Path, title: str, slides: list[dict], **deck_extra) -> None:
    (fix / "deck" / "slides").mkdir(parents=True, exist_ok=True)
    for doc in slides:
        (fix / "deck" / "slides" / f"{doc['id']}.json").write_text(json.dumps(doc, indent=2) + "\n", encoding="utf-8")
    deck = {"schema_version": 1, "mode": "structured", "title": title, "template": "template/placeholder.pptx",
            "sections": [{"title": "", "slides": [d["id"] for d in slides]}]}
    deck.update(deck_extra)
    (fix / "deck" / "deck.json").write_text(json.dumps(deck, indent=2) + "\n", encoding="utf-8")


def status_deck(n: int, locked: tuple[str, ...] = ()) -> list[dict]:
    topics = ["Summary", "Scope", "Timeline", "Budget", "Risks", "Team", "Vendors", "Quality", "Training", "Rollout",
              "Support", "Benefits", "Dependencies", "Decisions", "Next steps"]
    out = []
    for i in range(n):
        sid = f"s-st{i + 1:02d}"
        doc = slide(sid, "Title and Content", f"{topics[i]} is on track for the March go-live",
                    [f"{topics[i]} point one for the programme", f"{topics[i]} point two with supporting detail", "A third point"])
        if sid in locked:
            doc["locked"] = True
        out.append(doc)
    return out


def main() -> None:
    fix = fixture("report-to-deck", "corp/corp.potx")
    annual_report(fix / "sources" / "annual-report-2025.pdf")

    fix = fixture("spreadsheet-to-charts", "corp/corp.potx")
    sales_workbook(fix / "sources" / "sales-2026.xlsx")

    fix = fixture("email-thread-brief", "plain/plain.pptx")
    email_thread(fix / "sources" / "lighthouse-status.eml", october_deck())

    fix = fixture("restyle-template", "corp/corp.potx")
    write_deck(fix, "Programme status", status_deck(6, locked=("s-st01",)))

    fix = fixture("shorten-deck", "plain/plain.pptx")
    write_deck(fix, "Programme status (long)", status_deck(15))

    fix = fixture("fix-checker-failures", "wide-dense/wide-dense.pptx")
    (fix / "assets").mkdir(exist_ok=True)
    im = Image.new("RGB", (2400, 1350), (31, 56, 100))
    ImageDraw.Draw(im).rectangle([300, 225, 2100, 1125], outline=(255, 255, 255), width=10)
    im.save(fix / "assets" / "site.png")
    long_points = [f"Workstream {i} has completed its design review and is preparing the build plan for the next release" for i in range(1, 31)]
    write_deck(fix, "Deck with problems", [
        slide("s-fx01", "Title and Content", "The programme has too many workstreams to list on one slide", long_points),
        slide("s-fx02", "Title and Content", "The site photo has no description",
              {"type": "image", "id": "e-img02", "asset": "assets/site.png", "alt": ""}),
        slide("s-fx03", "Title and Content", "Pale emphasis is hard to read on a white slide",
              [{"runs": [{"t": "Costs are "}, {"t": "under control", "color": "lt2", "b": True}]}, "Schedule is unchanged"]),
        slide("s-fx04", "Title and Content", "A slide that is already fine", ["Nothing to fix here"]),
    ])

    fix = fixture("roadmap-diagram", "corp/corp.potx")
    (fix / "sources" / "meridian-plan.md").write_text(PLAN, encoding="utf-8")

    fix = fixture("code-mode-custom", "plain/plain.pptx")
    write_deck(fix, "Capability radar", [slide("s-cm01", "Title and Content", "Six capabilities define the operating model",
                                                ["Strategy", "Process", "Data", "Technology", "People", "Governance"])])
    print("fixtures written under", ROOT)


if __name__ == "__main__":
    main()
