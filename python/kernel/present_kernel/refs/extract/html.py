"""HTML and MHTML: main-content text with headings and tables (BeautifulSoup, stdlib ``html.parser``)."""

from __future__ import annotations

import email
import email.policy
import re
from pathlib import Path
from typing import Any

from ...errors import UNSUPPORTED_REFERENCE, RpcError
from ..model import Ctx, clean_title
from .common import TextBuilder, built_doc

DROP = ("script", "style", "nav", "aside", "footer", "header", "form", "noscript", "template", "svg", "iframe",
        "button", "select", "dialog")  # fmt: skip
BLOCK = {"p", "div", "section", "article", "main", "body", "li", "ul", "ol", "dl", "dt", "dd", "blockquote", "pre",
         "figure", "figcaption", "tr", "td", "th", "address", "details", "summary", "hr", "html", "center"}  # fmt: skip
_HEADING = re.compile(r"^h([1-6])$")
_WS = re.compile(r"[ \t\r\n\f\v\xa0]+")


def _table_cells(table) -> list[list[str]]:
    rows: list[list[str]] = []
    for tr in table.find_all("tr"):
        if tr.find_parent("table") is not table:
            continue
        cells = [_WS.sub(" ", td.get_text(" ")).strip() for td in tr.find_all(["td", "th"], recursive=False)]
        if any(cells):
            rows.append(cells)
    return rows


def html_to_builder(markup: str | bytes) -> tuple[str, TextBuilder]:
    """Page title and the extracted blocks of an HTML document or fragment."""
    from bs4 import BeautifulSoup
    from bs4.element import Comment, NavigableString, Tag

    soup = BeautifulSoup(markup, "html.parser")
    title = clean_title(soup.title.get_text()) if soup.title else ""
    root = soup.find("main") or soup.find(attrs={"role": "main"}) or soup.find("article") or soup.body or soup
    # A <header> inside the main content usually holds the page title, so it is only dropped outside it.
    for tag in soup.find_all(DROP):
        if tag.name == "header" and root is not soup.body and root is not soup and root in tag.parents:
            continue
        tag.decompose()
    builder = TextBuilder()
    buf: list[str] = []

    def flush() -> None:
        text = _WS.sub(" ", "".join(buf)).strip()
        buf.clear()
        if text:
            builder.paragraph(text)

    def walk(node) -> None:
        for child in node.children:
            if isinstance(child, Comment):
                continue
            if isinstance(child, NavigableString):
                buf.append(str(child))
                continue
            if not isinstance(child, Tag):
                continue
            name = child.name.lower()
            m = _HEADING.match(name)
            if m:
                flush()
                builder.heading(int(m.group(1)), child.get_text(" "))
            elif name == "table":
                flush()
                cells = _table_cells(child)
                caption = child.find("caption")
                if cells and max(len(r) for r in cells) >= 2:
                    builder.table(cells, clean_title(caption.get_text(" ")) if caption else None)
                else:
                    for row in cells:
                        builder.paragraph(" ".join(row))
            elif name == "br":
                buf.append("\n")
            elif name == "pre":
                flush()
                builder.paragraph(child.get_text())
            elif name in BLOCK:
                flush()
                walk(child)
                flush()
            else:
                walk(child)

    walk(root)
    flush()
    if not title and builder.headings:
        title = builder.headings[0][2]
    return title, builder


def html_to_text(markup: str | bytes) -> str:
    return html_to_builder(markup)[1].text


def extract(path: Path, ctx: Ctx) -> dict[str, Any]:
    title, builder = html_to_builder(path.read_bytes())
    return built_doc("html", title or ctx.name or "Web page", builder)


def extract_mhtml(path: Path, ctx: Ctx) -> dict[str, Any]:
    message = email.message_from_bytes(path.read_bytes(), policy=email.policy.default)
    for part in message.walk():
        if part.get_content_type() == "text/html":
            payload = part.get_payload(decode=True) or b""
            charset = part.get_content_charset()
            markup: str | bytes = payload
            if charset:
                try:
                    markup = payload.decode(charset, errors="replace")
                except LookupError:
                    markup = payload
            title, builder = html_to_builder(markup)
            subject = clean_title(str(message.get("Subject", "") or ""))
            return built_doc("html", title or subject or ctx.name or "Web page", builder)
    raise RpcError(UNSUPPORTED_REFERENCE, f"{ctx.label}: no text/html part in this MHTML file")
