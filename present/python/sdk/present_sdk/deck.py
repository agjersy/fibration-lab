"""``Deck`` and ``Slide``: the public surface of the deck SDK (PROTOCOL.md section 5).

The structured renderer drives these with deck-document dicts; code-mode build scripts and
generated slides call the same methods directly.
"""

from __future__ import annotations

import copy
import hashlib
import io
import json
import os
import uuid
from pathlib import Path
from typing import Any

from lxml import etree

from . import content as C
from . import tags as T
from .omml import alternate_parts
from .template import (
    FURNITURE_TYPES,
    describe_presentation,
    find_layout,
    layout_info,
    load_presentation,
    resolve_placeholder_info,
    sha256_file,
)
from .zipfix import normalize_zip

P = "http://schemas.openxmlformats.org/presentationml/2006/main"
A = "http://schemas.openxmlformats.org/drawingml/2006/main"
P14 = "http://schemas.microsoft.com/office/powerpoint/2010/main"
_SECTION_EXT = "{521415D9-36F7-43E2-AB2F-B90AF26B5E84}"
_SHAPE_TAGS = {f"{{{P}}}{n}" for n in ("sp", "pic", "graphicFrame", "grpSp", "cxnSp")}
_DECKS: list[Deck] = []


def _short_hash(*parts: Any) -> str:
    return hashlib.sha1("\x1f".join(str(p) for p in parts).encode()).hexdigest()[:8]


_MC_ALTERNATE = "{http://schemas.openxmlformats.org/markup-compatibility/2006}AlternateContent"


def _sync_fallback(choice: etree._Element, fallback: etree._Element) -> None:
    """The fallback of an equation carries the choice shape's id, name, alt text and tags."""
    nv_c, nv_f = choice.find(f"{{{P}}}nvSpPr"), fallback.find(f"{{{P}}}nvSpPr")
    if nv_c is None or nv_f is None:
        return
    cnv_c, cnv_f = nv_c.find(f"{{{P}}}cNvPr"), nv_f.find(f"{{{P}}}cNvPr")
    if cnv_c is not None and cnv_f is not None:
        for k, v in cnv_c.attrib.items():
            cnv_f.set(k, v)
    pr_c, pr_f = nv_c.find(f"{{{P}}}nvPr"), nv_f.find(f"{{{P}}}nvPr")
    if pr_c is not None and pr_f is not None:
        for old in pr_f.findall(f"{{{P}}}custDataLst"):
            pr_f.remove(old)
        tags = pr_c.find(f"{{{P}}}custDataLst")
        if tags is not None:
            ext = pr_f.find(f"{{{P}}}extLst")
            if ext is not None:
                ext.addprevious(copy.deepcopy(tags))
            else:
                pr_f.append(copy.deepcopy(tags))


def _kind_of(el: etree._Element) -> str:
    local = etree.QName(el).localname
    if local == "graphicFrame":
        uri = el.find(f".//{{{A}}}graphicData")
        u = uri.get("uri", "") if uri is not None else ""
        return "chart" if u.endswith("/chart") else "table" if u.endswith("/table") else "frame"
    return {"sp": "shape", "pic": "image", "grpSp": "group", "cxnSp": "connector"}.get(local, local)


class Placeholder:
    """A placeholder of a slide, addressed by its template key."""

    def __init__(self, slide: Slide, key: str, info: dict[str, Any]):
        self.slide = slide
        self.key = key
        self.info = info

    @property
    def box(self) -> tuple[int, int, int, int]:
        return self.info["x"], self.info["y"], self.info["cx"], self.info["cy"]

    @property
    def shape(self):
        """The python-pptx placeholder shape, or None once it has been replaced by a graphic."""
        for ph in self.slide.raw.placeholders:
            if ph.placeholder_format.idx == self.info["idx"]:
                return ph
        return None

    def set_text(self, text: Any, element_id: str | None = None):
        shape = self.shape
        if shape is None:
            raise C.ContentError(f"placeholder {self.key!r} was already replaced", element_id)
        model = C.as_text(text)
        C.apply_text(shape.text_frame, model)
        self.slide._register(shape._element, element_id, C.text_cites(model))
        return shape

    def paragraph(self, text: str, level: int = 0, **run):
        """Append one paragraph; returns the python-pptx paragraph for further work (and ``cite``)."""
        shape = self.shape
        if shape is None:
            raise C.ContentError(f"placeholder {self.key!r} was already replaced")
        tf = shape.text_frame
        first = len(tf.paragraphs) == 1 and not tf.paragraphs[0].runs and not self.slide._touched.get(self.key)
        p = tf.paragraphs[0] if first else tf.add_paragraph()
        self.slide._touched[self.key] = True
        C.apply_paragraph(p, {"level": level, "runs": [{"t": text, **run}]})
        self.slide._register(shape._element, None, [])
        return p

    def _take_over(self) -> tuple[etree._Element | None, tuple[int, int, int, int]]:
        """Remove the placeholder shape, returning its ``p:ph`` so a graphic can stand in for it."""
        shape = self.shape
        ph_el = None
        if shape is not None:
            ph = shape._element.find(f".//{{{P}}}nvPr/{{{P}}}ph")
            ph_el = copy.deepcopy(ph) if ph is not None else None
            self.slide._forget(shape._element)
            shape._element.getparent().remove(shape._element)
        return ph_el, self.box

    def _adopt(self, new_el: etree._Element, ph_el: etree._Element | None) -> None:
        nv = T.nv_pr(new_el)
        if nv is not None and ph_el is not None and nv.find(f"{{{P}}}ph") is None:
            nv.insert(0, ph_el)


class Slide:
    """One slide under construction."""

    def __init__(self, deck: Deck, raw, slide_id: str, layout: dict[str, Any]):
        self.deck = deck
        self.raw = raw  # the python-pptx slide: the escape hatch for code mode
        self.id = slide_id
        self.layout = layout
        self._elements: list[tuple[etree._Element, str, list[str]]] = []
        self._touched: dict[str, bool] = {}
        self._auto = 0

    # ---- registry -------------------------------------------------------------------------
    def _next_id(self) -> str:
        self._auto += 1
        return f"e-{_short_hash(self.id, self._auto)}"

    def _register(self, el: etree._Element, element_id: str | None, cites: list[str]) -> str:
        for i, (known, eid, old) in enumerate(self._elements):
            if known is el:
                merged = old + [c for c in cites if c not in old]
                self._elements[i] = (known, element_id or eid, merged)
                if element_id:
                    C.set_name(el, element_id)
                return element_id or eid
        eid = element_id or self._next_id()
        self._elements.append((el, eid, list(cites)))
        if not eid.startswith(("e-ftr", "e-sldNum", "e-dt")):
            C.set_name(el, eid)
        return eid

    def _forget(self, el: etree._Element) -> None:
        self._elements = [e for e in self._elements if e[0] is not el]

    def _owner(self, obj: Any) -> etree._Element | None:
        """The top-level shape element that contains a python-pptx object or lxml element."""
        el = obj
        for attr in ("_element", "_r", "_p", "_txBody", "_tc", "_tbl", "_chartSpace"):
            if hasattr(el, attr) and not isinstance(el, etree._Element):
                el = getattr(el, attr)
                break
        if hasattr(el, "part") and hasattr(el, "_chartSpace"):
            return None
        if not isinstance(el, etree._Element):
            return None
        tree = self.raw.shapes._spTree
        while el is not None:
            parent = el.getparent()
            if parent is tree and el.tag in _SHAPE_TAGS:
                return el
            el = parent
        return None

    # ---- placeholders -----------------------------------------------------------------------
    def placeholder(self, key: str) -> Placeholder:
        try:
            info = resolve_placeholder_info(self.layout, key)
        except KeyError:
            keys = [p["key"] for p in self.layout["placeholders"] if p["type"] not in FURNITURE_TYPES]
            raise C.ContentError(f"layout {self.layout['name']!r} has no placeholder {key!r} (has {keys})") from None
        return Placeholder(self, info["key"], info)

    def title(self, text: Any, element_id: str | None = None):
        return self.placeholder("title").set_text(text, element_id)

    def free_box(self, margin: int = 457200) -> tuple[int, int, int, int]:
        """The area below the title (or the whole slide minus margins) for free content."""
        w, h = self.deck.slide_size
        top = margin
        for ph in self.layout["placeholders"]:
            if ph["type"] in ("title", "ctrTitle"):
                top = max(top, ph["y"] + ph["cy"] + margin // 4)
        return margin, top, w - 2 * margin, h - top - margin

    def _target(self, placeholder: str | None, box):
        if placeholder is not None:
            ph = self.placeholder(placeholder)
            ph_el, geom = ph._take_over()
            return ph, ph_el, geom
        if box is None:
            box = self.free_box()
        return None, None, tuple(int(v) for v in box)

    # ---- content ----------------------------------------------------------------------------
    def text(self, text: Any, box, element_id: str | None = None):
        x, y, cx, cy = (int(v) for v in box)
        shape = self.raw.shapes.add_textbox(x, y, cx, cy)
        shape.text_frame.word_wrap = True
        model = C.as_text(text)
        C.apply_text(shape.text_frame, model)
        self._register(shape._element, element_id, C.text_cites(model))
        return shape

    def image(self, path: str | os.PathLike, alt: str, placeholder: str | None = None, box=None, fit: str = "fit",
              crop: dict[str, float] | None = None, cite: list[str] | None = None, element_id: str | None = None):
        full = self.deck.resolve(path)
        ph, ph_el, geom = self._target(placeholder, box)
        pic = C.add_picture(self.raw.shapes, full, geom, fit, crop)
        if ph is not None:
            ph._adopt(pic._element, ph_el)
        C.set_alt(pic._element, alt)
        self._register(pic._element, element_id, cite or [])
        return pic

    def chart(self, chart: dict[str, Any] | None = None, placeholder: str | None = None, box=None,
              data: dict[str, Any] | None = None, alt: str | None = None, element_id: str | None = None, **spec):
        """Add a native chart. ``chart`` is a Chart model; keyword arguments build one in scripts."""
        model = dict(chart or {})
        model.update(spec)
        table = data or model.get("computed")
        if table is None and model.get("source", {}).get("kind") == "inline":
            table = model["source"]["table"]
        if table is None:
            table = self.deck.compute_chart(model)
        elif model.get("steps") and data is not None:
            table = self.deck.apply_steps(table, model["steps"])
        ph, ph_el, geom = self._target(placeholder, box)
        frame = C.add_chart(self.raw.shapes, model, table, geom)
        if ph is not None:
            ph._adopt(frame._element, ph_el)
        cites = []
        src = model.get("source") or {}
        if src.get("kind") == "ref" and src.get("cite"):
            cites.append(src["cite"])
        cites += [c for c in src.get("cites", []) if c not in cites]
        C.set_alt(frame._element, alt or model.get("title") or "Chart")
        self._register(frame._element, element_id, cites)
        return frame

    def table(self, table: dict[str, Any], placeholder: str | None = None, box=None, element_id: str | None = None):
        ph, ph_el, geom = self._target(placeholder, box)
        frame = C.add_table(self.raw.shapes, table, geom, self.deck.default_table_style)
        if ph is not None:
            ph._adopt(frame._element, ph_el)
        cites: list[str] = []
        for row in table.get("rows", []):
            for cell in row.get("cells", []):
                cites += [c for c in C.text_cites(cell.get("text") if isinstance(cell.get("text"), dict) else None)
                          if c not in cites]  # fmt: skip
        src = table.get("source") or {}
        if src.get("cite") and src["cite"] not in cites:
            cites.append(src["cite"])
        self._register(frame._element, element_id, cites)
        return frame

    def diagram(self, family: str | dict[str, Any], placeholder: str | None = None, box=None,
                element_id: str | None = None, **spec):
        from .diagrams import add_diagram

        model = dict(family) if isinstance(family, dict) else {"family": family, **spec}
        _, _, geom = self._target(placeholder, box)
        group, overflow = add_diagram(self, model, geom)
        eid = self._register(group._element, element_id, _diagram_cites(model))
        if overflow:
            self.deck.warnings.append({
                "rule": "fit.overflow", "severity": "error", "slide_id": self.id, "element_id": eid,
                "message": f"diagram text does not fit at the minimum font size ({len(overflow)} label(s))",
            })  # fmt: skip
        return group

    def equation(self, latex: str, placeholder: str | None = None, box=None, alt: str | None = None,
                 element_id: str | None = None):
        """A native, editable PowerPoint equation (OMML) from LaTeX.

        LaTeX outside the converter's subset falls back to a rendered image with the LaTeX as
        alt text (or, without Matplotlib, to the LaTeX as text); the fallback is recorded in
        ``deck.warnings`` as ``eq.omml-fallback``."""
        from . import omml

        ph, ph_el, geom = self._target(placeholder, box)
        x, y, cx, cy = geom
        description = alt or latex
        try:
            para = omml.equation_paragraph(latex)
        except omml.OmmlError as exc:
            return self._equation_fallback(latex, ph, ph_el, geom, description, element_id, str(exc))
        shape = self.raw.shapes.add_textbox(x, y, cx, cy)
        tf = shape.text_frame
        tf.word_wrap = True
        body = tf._txBody
        for old in body.findall(f"{{{A}}}p"):
            body.remove(old)
        body.append(para)
        body_pr = body.find(f"{{{A}}}bodyPr")
        if body_pr is not None:
            body_pr.set("anchor", "ctr")
        if ph is not None:
            ph._adopt(shape._element, ph_el)
        C.set_alt(shape._element, description)
        self._register(shape._element, element_id, [])
        # As PowerPoint stores an equation: the shape inside `mc:AlternateContent`, an `a14`
        # choice holding the OMML and a fallback holding a picture of the equation (Matplotlib's
        # mathtext) for viewers without Office 2010 math; the LaTeX as text without Matplotlib.
        png = _render_latex_png(latex)
        rid, size = None, None
        if png is not None:
            from PIL import Image

            _, rid = self.raw.part.get_or_add_image_part(io.BytesIO(png))
            with Image.open(io.BytesIO(png)) as im:
                # Natural size in EMU at the resolution it was drawn at (_LATEX_DPI).
                size = (round(im.size[0] * 914400 / _LATEX_DPI), round(im.size[1] * 914400 / _LATEX_DPI))
        sp = shape._element
        omml.wrap_equation_shape(sp, omml.fallback_shape(sp, rid, size, latex))
        return shape

    def _equation_fallback(self, latex: str, ph, ph_el, geom, alt: str, element_id: str | None, why: str):
        import tempfile

        png = _render_latex_png(latex)
        eid = element_id
        if png is not None:
            with tempfile.NamedTemporaryFile(suffix=".png", delete=False) as fh:
                fh.write(png)
                tmp = Path(fh.name)
            try:
                shape = C.add_picture(self.raw.shapes, tmp, geom, "fit", None)
            finally:
                tmp.unlink(missing_ok=True)
            if ph is not None:
                ph._adopt(shape._element, ph_el)
            C.set_alt(shape._element, alt)
            how = "a rendered image"
        else:
            x, y, cx, cy = geom
            shape = self.raw.shapes.add_textbox(x, y, cx, cy)
            C.apply_text(shape.text_frame, {"paragraphs": [{"runs": [{"t": latex}], "align": "center"}]})
            C.set_alt(shape._element, alt)
            how = "its LaTeX as text"
        eid = self._register(shape._element, element_id, [])
        self.deck.warnings.append({
            "rule": "eq.omml-fallback", "severity": "warn", "slide_id": self.id, "element_id": eid,
            "message": f"the equation could not be converted to a native equation ({why}); exported as {how}",
        })  # fmt: skip
        return shape

    def shape(self, spec: dict[str, Any], box, element_id: str | None = None):
        shp = C.add_shape(self.raw.shapes, spec, tuple(int(v) for v in box))
        self._register(shp._element, element_id, C.text_cites(spec.get("text") if isinstance(spec.get("text"), dict) else None))
        return shp

    def notes(self, text: Any) -> None:
        C.apply_text(self.raw.notes_slide.notes_text_frame, text)

    def fill(self, key: str, item: dict[str, Any]) -> None:
        """Apply one PlaceholderContent from the deck document."""
        kind = item.get("type")
        eid = item.get("id")
        if kind in (None, "empty"):
            return
        if kind == "text":
            self.placeholder(key).set_text(item["text"], eid)
        elif kind == "image":
            self.image(item["asset"], item.get("alt", ""), placeholder=key, fit=item.get("fit", "fit"),
                       crop=item.get("crop"), cite=item.get("cite"), element_id=eid)  # fmt: skip
        elif kind == "chart":
            self.chart(item["chart"], placeholder=key, alt=item.get("alt"), element_id=eid)
        elif kind == "table":
            self.table(item["table"], placeholder=key, element_id=eid)
        elif kind == "diagram":
            self.diagram(item["diagram"], placeholder=key, element_id=eid)
        elif kind == "equation":
            self.equation(item["eq_cite"]["latex"], placeholder=key, element_id=eid,
                          alt=_equation_alt(item["eq_cite"]))  # fmt: skip
        else:
            raise C.ContentError(f"unknown content type {kind!r}", eid)

    def free(self, element: dict[str, Any]) -> None:
        """Apply one FreeElement from the deck document."""
        box = (element["x"], element["y"], element["cx"], element["cy"])
        item = element["content"]
        kind, eid = item.get("type"), element.get("id") or item.get("id")
        if kind == "text":
            self.text(item["text"], box, eid)
        elif kind == "image":
            self.image(item["asset"], item.get("alt", ""), box=box, fit=item.get("fit", "fit"),
                       crop=item.get("crop"), cite=item.get("cite"), element_id=eid)  # fmt: skip
        elif kind == "chart":
            self.chart(item["chart"], box=box, alt=item.get("alt"), element_id=eid)
        elif kind == "table":
            self.table(item["table"], box=box, element_id=eid)
        elif kind == "diagram":
            self.diagram(item["diagram"], box=box, element_id=eid)
        elif kind == "equation":
            self.equation(item["eq_cite"]["latex"], box=box, element_id=eid, alt=_equation_alt(item["eq_cite"]))
        elif kind == "shape":
            self.shape(item, box, eid)
        else:
            raise C.ContentError(f"unknown content type {kind!r}", eid)

    # ---- furniture --------------------------------------------------------------------------
    def furniture(self, footer_text: str | None = None, slide_numbers: bool = False, date: bool = False) -> None:
        """Instantiate the layout's footer, slide number, and date placeholders on this slide."""
        want = {"ftr": footer_text is not None and footer_text != "", "sldNum": slide_numbers, "dt": date}
        tree = self.raw.shapes._spTree
        for sp in self.raw.slide_layout._element.iterfind(f".//{{{P}}}sp"):
            ph = sp.find(f"{{{P}}}nvSpPr/{{{P}}}nvPr/{{{P}}}ph")
            if ph is None or not want.get(ph.get("type", ""), False):
                continue
            clone = copy.deepcopy(sp)
            tree.append(clone)
            self._register(clone, f"e-{ph.get('type')}", [])
            if ph.get("type") == "ftr":
                body = clone.find(f"{{{P}}}txBody")
                if body is not None:
                    for p in body.findall(f"{{{A}}}p"):
                        body.remove(p)
                    para = etree.SubElement(body, f"{{{A}}}p")
                    run = etree.SubElement(para, f"{{{A}}}r")
                    etree.SubElement(run, f"{{{A}}}rPr", lang="en-US")
                    etree.SubElement(run, f"{{{A}}}t").text = footer_text


def _equation_alt(cite: dict[str, Any]) -> str:
    return f"{cite.get('latex', '')} ({cite.get('qualname', '?')} in {cite.get('path', '?')})"


_LATEX_DPI = 200


def _render_latex_png(latex: str) -> bytes | None:
    """The equation drawn by Matplotlib's mathtext, when Matplotlib is installed and can draw it."""
    try:
        from matplotlib.backends.backend_agg import FigureCanvasAgg
        from matplotlib.figure import Figure
    except ImportError:
        return None
    try:
        fig = Figure(figsize=(0.1, 0.1))
        FigureCanvasAgg(fig)
        fig.text(0, 0, f"${latex}$", fontsize=28)
        buf = io.BytesIO()
        fig.savefig(buf, format="png", dpi=_LATEX_DPI, bbox_inches="tight", pad_inches=0.05, transparent=True)
        return buf.getvalue()
    except Exception:
        return None


def _diagram_cites(model: dict[str, Any]) -> list[str]:
    out: list[str] = []

    def walk(value: Any) -> None:
        if isinstance(value, dict):
            if "paragraphs" in value:
                out.extend(c for c in C.text_cites(value) if c not in out)
            else:
                for v in value.values():
                    walk(v)
        elif isinstance(value, list):
            for v in value:
                walk(v)

    walk(model)
    return out


class Deck:
    """A presentation being built on a template."""

    def __init__(self, template: str | os.PathLike, project: str | os.PathLike | None = None,
                 out: str | os.PathLike | None = None, snapshot: str = "", sidecar: dict | None = None):
        self.template_path = Path(template)
        self.project = Path(project) if project else self.template_path.parent
        self.out = Path(out) if out else None
        self.snapshot = snapshot
        self.prs = load_presentation(self.template_path)
        self._drop_existing_slides()
        self.description = describe_presentation(self.prs, str(template), sha256_file(self.template_path), sidecar)
        self.default_table_style: str | None = self.description.get("default_table_style")
        self.slides: list[Slide] = []
        self.sections: list[tuple[str, list[str]]] = []
        self.properties: dict[str, str] = {}
        self.warnings: list[dict[str, Any]] = []
        self.mode = "code"
        self.stamp = os.environ.get("PRESENT_SNAPSHOT_TIME", "2000-01-01T00:00:00Z")
        self._refs = None
        _DECKS.append(self)
        del _DECKS[:-4]  # scripts normally have exactly one deck open

    @classmethod
    def open_template(cls) -> Deck:
        """Open the project template named by the kernel-provided environment."""
        env = os.environ
        missing = [k for k in ("PRESENT_PROJECT", "PRESENT_TEMPLATE", "PRESENT_OUT") if k not in env]
        if missing:
            raise RuntimeError(f"Deck.open_template() needs {', '.join(missing)} (set by the Present kernel)")
        project = Path(env["PRESENT_PROJECT"])
        template = Path(env["PRESENT_TEMPLATE"])
        if not template.is_absolute():
            template = project / template
        sidecar = None
        side = template.parent / "template.toml"
        if side.is_file():
            import tomllib

            sidecar = tomllib.loads(side.read_text(encoding="utf-8"))
        return cls(template, project, env["PRESENT_OUT"], env.get("PRESENT_SNAPSHOT", ""), sidecar)

    # ---- helpers ----------------------------------------------------------------------------
    def _drop_existing_slides(self) -> None:
        lst = self.prs.slides._sldIdLst
        for sld in list(lst):
            self.prs.part.drop_rel(sld.rId)
            lst.remove(sld)

    @property
    def slide_size(self) -> tuple[int, int]:
        return int(self.prs.slide_width), int(self.prs.slide_height)

    def resolve(self, path: str | os.PathLike) -> Path:
        p = Path(path)
        return p if p.is_absolute() else self.project / p

    def asset(self, path: str) -> Path:
        return self.resolve(path)

    # References are read through the kernel's extractors when they are importable (always true
    # inside the bundled runtime); the SDK itself stays free of that dependency.
    def _ref_reader(self):
        if self._refs is None:
            try:
                from present_kernel.refs.reader import ProjectRefs
            except ImportError as exc:  # pragma: no cover - SDK used standalone
                raise RuntimeError("reading references needs the Present kernel runtime") from exc
            self._refs = ProjectRefs(self.project)
        return self._refs

    def ref_table(self, cite: str) -> dict[str, Any]:
        return self._ref_reader().table(cite)

    def ref_text(self, cite: str) -> str:
        return self._ref_reader().text(cite)

    def apply_steps(self, table: dict[str, Any], steps: list[dict[str, Any]]) -> dict[str, Any]:
        from present_kernel.transforms import apply_steps

        return apply_steps(table, steps)

    def compute_chart(self, chart: dict[str, Any]) -> dict[str, Any]:
        src = chart.get("source") or {}
        if src.get("kind") == "inline":
            table = src["table"]
        elif src.get("kind") == "ref":
            table = self.ref_table(src["cite"])
        else:
            raise C.ContentError("chart has no data: give `data=`, an inline source, or a ref source")
        return self.apply_steps(table, chart["steps"]) if chart.get("steps") else table

    # ---- building ---------------------------------------------------------------------------
    def add_slide(self, layout: str, slide_id: str | None = None, section: str | None = None) -> Slide:
        try:
            raw_layout = find_layout(self.prs, layout)
            info = layout_info(self.description, layout)
        except KeyError:
            names = [lay["name"] for m in self.description["masters"] for lay in m["layouts"]]
            raise C.ContentError(f"template has no layout {layout!r} (has {names})") from None
        raw = self.prs.slides.add_slide(raw_layout)
        sid = slide_id or f"s-{_short_hash(len(self.slides), layout)}"
        slide = Slide(self, raw, sid, info)
        self.slides.append(slide)
        if section is not None:
            if self.sections and self.sections[-1][0] == section:
                self.sections[-1][1].append(sid)
            else:
                self.sections.append((section, [sid]))
        return slide

    def set_sections(self, sections: list[dict[str, Any]]) -> None:
        self.sections = [(s.get("title", ""), list(s.get("slides", []))) for s in sections]

    def _write_sections(self) -> None:
        named = [s for s in self.sections if s[0]]
        if not named:
            return
        ids = {s.id: sld.get("id") for s, sld in zip(self.slides, self.prs.slides._sldIdLst)}
        pres = self.prs.part._element
        ext_lst = pres.find(f"{{{P}}}extLst")
        if ext_lst is None:
            ext_lst = etree.SubElement(pres, f"{{{P}}}extLst")
        for ext in ext_lst.findall(f"{{{P}}}ext"):
            if ext.get("uri") == _SECTION_EXT:
                ext_lst.remove(ext)
        ext = etree.SubElement(ext_lst, f"{{{P}}}ext", uri=_SECTION_EXT)
        lst = etree.SubElement(ext, f"{{{P14}}}sectionLst", nsmap={"p14": P14})
        for i, (title, slides) in enumerate(self.sections):
            guid = "{" + str(uuid.uuid5(uuid.NAMESPACE_URL, f"present-section:{i}:{title}")).upper() + "}"
            sec = etree.SubElement(lst, f"{{{P14}}}section", name=title or f"Section {i + 1}", id=guid)
            sld_lst = etree.SubElement(sec, f"{{{P14}}}sldIdLst")
            for sid in slides:
                if sid in ids:
                    etree.SubElement(sld_lst, f"{{{P14}}}sldId", id=ids[sid])

    def _finalize(self) -> list[dict[str, Any]]:
        """Assign ids to unregistered shapes, write tags, and return the element map."""
        emap: list[dict[str, Any]] = []
        for index, slide in enumerate(self.slides):
            known = {id(el): (eid, cites) for el, eid, cites in slide._elements}
            raw_n = 0
            T.set_slide_tags(slide.raw.part, {"present.slide": slide.id})
            for top in slide.raw.shapes._spTree:
                el, fallback = top, None
                if top.tag == _MC_ALTERNATE:  # an equation: the a14 choice is the element
                    el, fallback = alternate_parts(top)
                    if el is None:
                        continue
                if el.tag not in _SHAPE_TAGS:
                    continue
                ph = el.find(f".//{{{P}}}nvPr/{{{P}}}ph")
                if id(el) in known:
                    eid, cites = known[id(el)]
                elif ph is not None and not _has_content(el):
                    continue  # an untouched, empty placeholder
                else:
                    raw_n += 1
                    eid, cites = f"e-raw-{raw_n}", []
                    C.set_name(el, eid)
                tags = {"present.element": eid}
                for n, c in enumerate(cites):
                    tags[f"present.cite.{n}"] = c
                T.set_shape_tags(slide.raw.part, el, tags)
                if fallback is not None:
                    _sync_fallback(el, fallback)
                cnv = el.find(f".//{{{P}}}cNvPr")
                emap.append({
                    "slide_id": slide.id, "element_id": eid, "slide_index": index,
                    "shape_id": int(cnv.get("id", "0")) if cnv is not None else 0, "kind": _kind_of(el),
                })  # fmt: skip
        return emap

    def _apply_autofit(self) -> None:
        """Write the shrink scale into shrink-on-overflow placeholders whose text needs it."""
        from .fit import measure_shape, placeholder_info, write_autofit
        from .measure import Measurer

        measurer = Measurer()
        fonts = self.description["theme"]["fonts"]
        for slide in self.slides:
            for sp in slide.raw.shapes._spTree.iterfind(f"{{{P}}}sp"):
                info = placeholder_info(slide.layout, sp)
                if info is None or info["autofit"] != "shrink":
                    continue
                got = measure_shape(sp, info, fonts, measurer)
                if got is not None and got[1].scale < 1.0:
                    write_autofit(sp, got[1])

    def to_bytes(self) -> tuple[bytes, list[dict[str, Any]]]:
        self._apply_autofit()
        self._write_sections()
        emap = self._finalize()
        props = {"present.schema_version": "1", "present.mode": self.mode}
        if self.snapshot:
            props["present.deck_snapshot"] = self.snapshot
        props.update({k: v for k, v in self.properties.items() if k not in ("Title", "Author")})
        T.set_custom_properties(self.prs, props)
        core = self.prs.core_properties
        if "Title" in self.properties:
            core.title = self.properties["Title"]
        if "Author" in self.properties:
            core.author = self.properties["Author"]
        core.last_modified_by = "Personal Present"
        core.revision = 1
        buf = io.BytesIO()
        self.prs.save(buf)
        return normalize_zip(buf.getvalue(), self.stamp), emap

    def save(self, out: str | os.PathLike | None = None) -> list[dict[str, Any]]:
        target = Path(out) if out else self.out
        if target is None:
            raise RuntimeError("no output path: pass one to save() or run under the Present kernel")
        data, emap = self.to_bytes()
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_bytes(data)
        map_path = os.environ.get("PRESENT_MAP")
        if map_path:
            Path(map_path).write_text(json.dumps({"element_map": emap, "warnings": self.warnings}), encoding="utf-8")
        return emap


def _has_content(el: etree._Element) -> bool:
    return any((t.text or "").strip() for t in el.iter(f"{{{A}}}t")) or el.tag != f"{{{P}}}sp"


def cite(obj: Any, citation: str, deck: Deck | None = None) -> None:
    """Attach a citation to the shape that owns ``obj`` (a run, paragraph, shape, chart frame...)."""
    decks = [deck] if deck else list(reversed(_DECKS))
    for d in decks:
        for slide in d.slides:
            owner = slide._owner(obj)
            if owner is not None:
                slide._register(owner, None, [citation])
                return
    raise ValueError("cite(): the object does not belong to a slide of an open Deck")


def alt(obj: Any, text: str) -> None:
    """Set alt text on a shape (python-pptx shape or the object returned by the SDK)."""
    el = obj._element if hasattr(obj, "_element") else obj
    C.set_alt(el, text)


