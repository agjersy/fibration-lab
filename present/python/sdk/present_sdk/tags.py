"""``present.*`` identity tags and custom document properties (PROTOCOL.md section 2.7).

Tags live in ``/ppt/tags/tagN.xml`` parts referenced from ``p:custDataLst``; PowerPoint keeps them
through edits, which is what makes re-import possible later. PowerPoint upper-cases tag names
on save, so names are compared case-insensitively when read back.
"""

from __future__ import annotations

from lxml import etree
from pptx.opc.package import Part
from pptx.opc.packuri import PackURI

P = "http://schemas.openxmlformats.org/presentationml/2006/main"
R = "http://schemas.openxmlformats.org/officeDocument/2006/relationships"
TAGS_CT = "application/vnd.openxmlformats-officedocument.presentationml.tags+xml"
TAGS_RT = "http://schemas.openxmlformats.org/officeDocument/2006/relationships/tags"
CUSTOM_CT = "application/vnd.openxmlformats-officedocument.custom-properties+xml"
CUSTOM_RT = "http://schemas.openxmlformats.org/officeDocument/2006/relationships/custom-properties"
CUSTOM_NS = "http://schemas.openxmlformats.org/officeDocument/2006/custom-properties"
VT_NS = "http://schemas.openxmlformats.org/officeDocument/2006/docPropsVTypes"
_FMTID = "{D5CDD505-2E9C-101B-9397-08002B2CF9AE}"


def _tags_blob(tags: dict[str, str]) -> bytes:
    root = etree.Element(f"{{{P}}}tagLst", nsmap={"p": P, "r": R})
    for name, val in tags.items():
        etree.SubElement(root, f"{{{P}}}tag", name=name, val=val)
    return etree.tostring(root, xml_declaration=True, encoding="UTF-8", standalone=True)


def nv_pr(shape_el: etree._Element) -> etree._Element | None:
    """The ``p:nvPr`` of any shape element (sp, pic, graphicFrame, grpSp, cxnSp)."""
    for child in shape_el:
        if etree.QName(child).localname.startswith("nv"):
            return child.find(f"{{{P}}}nvPr")
    return None


def _attach(slide_part, host: etree._Element, tags: dict[str, str], before: tuple[str, ...]) -> None:
    for old in host.findall(f"{{{P}}}custDataLst"):
        host.remove(old)
    package = slide_part.package
    partname = PackURI(package.next_partname("/ppt/tags/tag%d.xml"))
    part = Part(partname, TAGS_CT, package, _tags_blob(tags))
    rid = slide_part.relate_to(part, TAGS_RT)
    lst = etree.Element(f"{{{P}}}custDataLst")
    etree.SubElement(lst, f"{{{P}}}tags").set(f"{{{R}}}id", rid)
    for name in before:
        anchor = host.find(f"{{{P}}}{name}")
        if anchor is not None:
            anchor.addprevious(lst)
            return
    host.append(lst)


def set_shape_tags(slide_part, shape_el: etree._Element, tags: dict[str, str]) -> None:
    host = nv_pr(shape_el)
    if host is not None and tags:
        _attach(slide_part, host, tags, ("extLst",))


def set_slide_tags(slide_part, tags: dict[str, str]) -> None:
    csld = slide_part._element.find(f"{{{P}}}cSld")
    if csld is not None and tags:
        _attach(slide_part, csld, tags, ("controls", "extLst"))


def _read(slide_part, host: etree._Element | None) -> dict[str, str]:
    out: dict[str, str] = {}
    if host is None:
        return out
    for ref in host.iterfind(f"{{{P}}}custDataLst/{{{P}}}tags"):
        rid = ref.get(f"{{{R}}}id")
        try:
            part = slide_part.related_part(rid)
        except KeyError:
            continue
        root = etree.fromstring(part.blob)
        for tag in root.iterfind(f"{{{P}}}tag"):
            out[tag.get("name", "").lower()] = tag.get("val", "")
    return out


def read_shape_tags(slide_part, shape_el: etree._Element) -> dict[str, str]:
    return _read(slide_part, nv_pr(shape_el))


def read_slide_tags(slide_part) -> dict[str, str]:
    return _read(slide_part, slide_part._element.find(f"{{{P}}}cSld"))


def strip_present_tags(prs) -> None:
    """Remove every ``present.*`` tag reference (export option ``strip_tags``)."""
    for slide in prs.slides:
        for lst in slide._element.iter(f"{{{P}}}custDataLst"):
            for ref in list(lst):
                rid = ref.get(f"{{{R}}}id")
                try:
                    part = slide.part.related_part(rid)
                except KeyError:
                    continue
                names = [t.get("name", "").lower() for t in etree.fromstring(part.blob).iter(f"{{{P}}}tag")]
                if names and all(n.startswith("present.") for n in names):
                    lst.remove(ref)
                    slide.part.drop_rel(rid)
        for lst in list(slide._element.iter(f"{{{P}}}custDataLst")):
            if len(lst) == 0:
                lst.getparent().remove(lst)


def set_custom_properties(prs, props: dict[str, str]) -> None:
    """Write (merge into) ``docProps/custom.xml``."""
    package = prs.part.package
    existing = None
    for rel in package._rels.values():
        if rel.reltype == CUSTOM_RT:
            existing = rel.target_part
            break
    if existing is not None:
        root = etree.fromstring(existing.blob)
    else:
        root = etree.Element(f"{{{CUSTOM_NS}}}Properties", nsmap={None: CUSTOM_NS, "vt": VT_NS})
    for el in list(root):
        if el.get("name") in props:
            root.remove(el)
    pid = max([int(el.get("pid", "1")) for el in root] + [1])
    for name, val in props.items():
        pid += 1
        prop = etree.SubElement(root, f"{{{CUSTOM_NS}}}property", fmtid=_FMTID, pid=str(pid), name=name)
        etree.SubElement(prop, f"{{{VT_NS}}}lpwstr").text = val
    blob = etree.tostring(root, xml_declaration=True, encoding="UTF-8", standalone=True)
    if existing is not None:
        existing._blob = blob
    else:
        part = Part(PackURI("/docProps/custom.xml"), CUSTOM_CT, package, blob)
        package.relate_to(part, CUSTOM_RT)


def read_custom_properties(prs) -> dict[str, str]:
    package = prs.part.package
    for rel in package._rels.values():
        if rel.reltype == CUSTOM_RT:
            root = etree.fromstring(rel.target_part.blob)
            return {el.get("name", ""): (el[0].text or "") for el in root if len(el)}
    return {}
