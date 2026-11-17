"""EML with the standard library: headers and body text; attachments become child references.

``mail_doc`` is shared with the MSG reader. The extracted text is a header block (Subject, From,
To, Cc, Date) followed by the body; attachment ``n`` (1-based, in file order) is addressed as
``att<n>/<loc in the attachment>``.
"""

from __future__ import annotations

import email
import email.policy
from email.utils import parsedate_to_datetime
from pathlib import Path
from typing import Any

from ..model import Ctx, clean_title, diag
from .common import text_doc


def mail_doc(kind: str, ctx: Ctx, subject: str, headers: list[tuple[str, str]], body: str,
             attachments: list[tuple[str, bytes]]) -> dict[str, Any]:  # fmt: skip
    head = "\n".join(f"{name}: {' '.join(value.split())}" for name, value in headers if value and value.strip())
    body = body.replace("\r\n", "\n").replace("\r", "\n").replace("\x00", "").strip()
    text = f"{head}\n\n{body}" if head and body else head or body
    doc = text_doc(kind, clean_title(subject) or "(no subject)", text, [], None)
    for n, (filename, data) in enumerate(attachments, start=1):
        child = ctx.attach(n, filename, data) if ctx.attach else None
        if child is None:
            doc["warnings"].append(diag("refs.attachment-skipped", f"attachment {n} ({filename or 'unnamed'}) is not a supported reference format and was not imported", "info"))
            continue
        doc["attachments"].append(child)
        doc["outline"].append({"title": child["name"], "loc": f"att{n}/{child['root_loc']}", "children": []})
    return doc


def _body(message) -> str:
    part = message.get_body(preferencelist=("plain", "html"))
    if part is None:
        return ""
    try:
        content = part.get_content()
    except Exception:
        payload = part.get_payload(decode=True) or b""
        content = payload.decode("utf-8", errors="replace")
    if isinstance(content, bytes):
        content = content.decode("utf-8", errors="replace")
    if part.get_content_type() == "text/html":
        from .html import html_to_text

        return html_to_text(content)
    return content


def iso_date(value: str) -> str:
    try:
        return parsedate_to_datetime(value).isoformat()
    except Exception:
        return value


def extract(path: Path, ctx: Ctx) -> dict[str, Any]:
    message = email.message_from_bytes(path.read_bytes(), policy=email.policy.default)

    def header(name: str) -> str:
        try:
            return str(message.get(name, "") or "")
        except Exception:
            return ""

    attachments: list[tuple[str, bytes]] = []
    for part in message.iter_attachments():
        filename = part.get_filename() or ""
        if part.get_content_type() == "message/rfc822":
            inner = part.get_payload(0) if part.is_multipart() else None
            data = inner.as_bytes() if inner is not None else (part.get_payload(decode=True) or b"")
            filename = filename or "message.eml"
            if not filename.lower().endswith(".eml"):
                filename += ".eml"
        else:
            data = part.get_payload(decode=True) or b""
        attachments.append((filename, data))
    subject = header("Subject")
    headers = [("Subject", subject), ("From", header("From")), ("To", header("To")), ("Cc", header("Cc")),
               ("Date", iso_date(header("Date")) if header("Date") else "")]  # fmt: skip
    return mail_doc("eml", ctx, subject, headers, _body(message), attachments)
