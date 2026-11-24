"""LaTeX to Office Math Markup (OMML) for native, editable PowerPoint equations.

A small converter for the LaTeX subset that SymPy's ``latex()`` produces, which is what the
analysis kernel extracts from code and ``Math:`` docstrings: fractions, powers and
subscripts, Greek letters, sums, products and integrals with limits, square and n-th roots,
``exp``/``log``/trigonometric functions, ``\\left( \\right)`` delimiters, accents, and
operators. Anything outside the subset raises :class:`OmmlError`; the caller then falls back
to a rendered image and records the fallback. Standard library and lxml only; no network.
"""

from __future__ import annotations

import copy

from lxml import etree

M = "http://schemas.openxmlformats.org/officeDocument/2006/math"
A = "http://schemas.openxmlformats.org/drawingml/2006/main"
A14 = "http://schemas.microsoft.com/office/drawing/2010/main"
MC = "http://schemas.openxmlformats.org/markup-compatibility/2006"


class OmmlError(ValueError):
    """LaTeX outside the supported subset."""


GREEK = {
    "alpha": "α", "beta": "β", "gamma": "γ", "delta": "δ", "epsilon": "ϵ", "varepsilon": "ε",
    "zeta": "ζ", "eta": "η", "theta": "θ", "vartheta": "ϑ", "iota": "ι", "kappa": "κ",
    "lambda": "λ", "mu": "μ", "nu": "ν", "xi": "ξ", "omicron": "ο", "pi": "π", "varpi": "ϖ",
    "rho": "ρ", "varrho": "ϱ", "sigma": "σ", "varsigma": "ς", "tau": "τ", "upsilon": "υ",
    "phi": "ϕ", "varphi": "φ", "chi": "χ", "psi": "ψ", "omega": "ω",
    "Gamma": "Γ", "Delta": "Δ", "Theta": "Θ", "Lambda": "Λ", "Xi": "Ξ", "Pi": "Π",
    "Sigma": "Σ", "Upsilon": "Υ", "Phi": "Φ", "Psi": "Ψ", "Omega": "Ω",
}  # fmt: skip
SYMBOLS = {
    "cdot": "⋅", "times": "×", "div": "÷", "pm": "±", "mp": "∓", "leq": "≤", "le": "≤",
    "geq": "≥", "ge": "≥", "neq": "≠", "ne": "≠", "approx": "≈", "sim": "∼", "equiv": "≡",
    "propto": "∝", "infty": "∞", "partial": "∂", "nabla": "∇", "to": "→", "rightarrow": "→",
    "leftarrow": "←", "in": "∈", "cdots": "⋯", "ldots": "…", "dots": "…", "hbar": "ℏ",
    "ell": "ℓ", "circ": "∘", "lbrace": "{", "rbrace": "}", "langle": "⟨", "rangle": "⟩",
    "{": "{", "}": "}", "%": "%", "#": "#", "&": "&", "_": "_", "|": "‖",
}  # fmt: skip
SPACES = {",": " ", ";": " ", ":": " ", "!": "", " ": " ", "quad": " ", "qquad": "  "}
FUNCTIONS = {
    "sin", "cos", "tan", "cot", "sec", "csc", "sinh", "cosh", "tanh", "coth", "arcsin",
    "arccos", "arctan", "exp", "log", "ln", "lg", "max", "min", "sup", "inf", "lim", "det",
    "arg", "deg", "gcd", "Re", "Im", "sign", "erf",
}  # fmt: skip
NARY = {"sum": "∑", "prod": "∏", "coprod": "∐", "int": "∫", "iint": "∬", "iiint": "∭", "oint": "∮",
        "bigcup": "⋃", "bigcap": "⋂"}  # fmt: skip
ACCENTS = {"hat": "̂", "widehat": "̂", "tilde": "̃", "widetilde": "̃", "dot": "̇",
           "ddot": "̈", "vec": "⃗", "check": "̌", "acute": "́", "grave": "̀",
           "breve": "̆"}  # fmt: skip
DELIMS = {"(": "(", ")": ")", "[": "[", "]": "]", "|": "|", ".": "", "\\{": "{", "\\}": "}",
          "\\lbrace": "{", "\\rbrace": "}", "\\langle": "⟨", "\\rangle": "⟩", "\\|": "‖",
          "\\lfloor": "⌊", "\\rfloor": "⌋", "\\lceil": "⌈", "\\rceil": "⌉"}  # fmt: skip
OPERATORS = set("+-=<>,;:!/*'|()[]")


def _m(tag: str) -> str:
    return f"{{{M}}}{tag}"


# ---- tokens -----------------------------------------------------------------------------


def tokenize(src: str) -> list[str]:
    out: list[str] = []
    i = 0
    while i < len(src):
        c = src[i]
        if c == "\\":
            j = i + 1
            if j < len(src) and src[j].isalpha():
                while j < len(src) and src[j].isalpha():
                    j += 1
                out.append(src[i:j])
                i = j
            elif j < len(src):
                out.append(src[i : j + 1])
                i = j + 1
            else:
                raise OmmlError("trailing backslash")
        elif c.isspace():
            i += 1
        elif c.isdigit() or (c == "." and i + 1 < len(src) and src[i + 1].isdigit()):
            j = i
            while j < len(src) and (src[j].isdigit() or src[j] == "."):
                j += 1
            out.append(src[i:j])
            i = j
        else:
            out.append(c)
            i += 1
    return out


# ---- OMML builders ----------------------------------------------------------------------


def _run(text: str, plain: bool = False, bold: bool = False) -> etree._Element:
    r = etree.Element(_m("r"))
    if plain or bold:
        rpr = etree.SubElement(r, _m("rPr"))
        etree.SubElement(rpr, _m("sty")).set(_m("val"), "b" if bold else "p")
    # PowerPoint renders math runs in Cambria Math; it writes this on every run.
    arpr = etree.SubElement(r, f"{{{A}}}rPr", lang="en-US", i="0" if plain else "1")
    etree.SubElement(arpr, f"{{{A}}}latin", typeface="Cambria Math", panose="02040503050406030204", pitchFamily="18", charset="0")
    etree.SubElement(r, _m("t")).text = text
    return r


def _wrap(tag: str, children: list[etree._Element]) -> etree._Element:
    el = etree.Element(_m(tag))
    el.extend(children)
    return el


def _prop(parent: etree._Element, tag: str, **vals: str) -> etree._Element:
    el = etree.SubElement(parent, _m(tag))
    for k, v in vals.items():
        el.set(_m(k), v)
    return el


# ---- parser -----------------------------------------------------------------------------


class _Parser:
    def __init__(self, tokens: list[str]):
        self.t = tokens
        self.i = 0

    def peek(self) -> str | None:
        return self.t[self.i] if self.i < len(self.t) else None

    def next(self) -> str:
        if self.i >= len(self.t):
            raise OmmlError("unexpected end of input")
        tok = self.t[self.i]
        self.i += 1
        return tok

    def expect(self, tok: str) -> None:
        got = self.next()
        if got != tok:
            raise OmmlError(f"expected {tok!r}, found {got!r}")

    def group(self) -> list[etree._Element]:
        """A braced group, or a single token as an argument (``x^2``, ``\\frac12``)."""
        if self.peek() == "{":
            self.next()
            items = self.expr(stop={"}"})
            self.expect("}")
            return items
        tok = self.peek()
        if tok is not None and tok.isdigit() and len(tok) > 1:
            # `x^23` is x^2 followed by 3 in TeX.
            self.t[self.i] = tok[1:]
            return [_run(tok[0])]
        return self.atom()

    def raw_group(self) -> str:
        """The text of a braced group as written (for ``\\text`` and ``\\operatorname``)."""
        self.expect("{")
        depth, parts = 1, []
        while True:
            tok = self.next()
            if tok == "{":
                depth += 1
            elif tok == "}":
                depth -= 1
                if depth == 0:
                    break
            parts.append(tok[1:] if tok.startswith("\\") and len(tok) == 2 else tok)
        return "".join(parts)

    def expr(self, stop: set[str]) -> list[etree._Element]:
        out: list[etree._Element] = []
        while self.peek() is not None and self.peek() not in stop:
            tok = self.peek()
            if tok in ("^", "_"):
                base = out.pop() if out else _run("​")
                out.append(self.scripts([base]))
                continue
            if tok == "\\right":
                break
            out.extend(self.atom(stop))
        return out

    def scripts(self, base: list[etree._Element]) -> etree._Element:
        sub = sup = None
        while self.peek() in ("^", "_"):
            tok = self.next()
            if tok == "^":
                if sup is not None:
                    raise OmmlError("double superscript")
                sup = self.group()
            else:
                if sub is not None:
                    raise OmmlError("double subscript")
                sub = self.group()
        if sub is not None and sup is not None:
            el = etree.Element(_m("sSubSup"))
            el.append(_wrap("e", base))
            el.append(_wrap("sub", sub))
            el.append(_wrap("sup", sup))
        elif sup is not None:
            el = etree.Element(_m("sSup"))
            el.append(_wrap("e", base))
            el.append(_wrap("sup", sup))
        else:
            el = etree.Element(_m("sSub"))
            el.append(_wrap("e", base))
            el.append(_wrap("sub", sub or []))
        return el

    def delimiter(self) -> str:
        tok = self.next()
        if tok in DELIMS:
            return DELIMS[tok]
        raise OmmlError(f"unsupported delimiter {tok!r}")

    def atom(self, stop: set[str] | None = None) -> list[etree._Element]:
        tok = self.next()
        if tok == "{":
            items = self.expr(stop={"}"})
            self.expect("}")
            return items
        if tok in ("}", "&") or tok == "\\\\":
            raise OmmlError(f"unsupported token {tok!r}")
        if not tok.startswith("\\"):
            if tok[0].isdigit() or tok[0] == ".":
                return [_run(tok)]
            if tok == "-":
                return [_run("−")]
            if tok in OPERATORS or tok.isalpha() or not tok.isascii():
                return [_run(tok)]
            raise OmmlError(f"unsupported character {tok!r}")
        name = tok[1:]
        if name in ("frac", "dfrac", "tfrac", "cfrac"):
            num, den = self.group(), self.group()
            f = etree.Element(_m("f"))
            f.append(_wrap("num", num))
            f.append(_wrap("den", den))
            return [f]
        if name == "binom":
            top, bottom = self.group(), self.group()
            f = etree.Element(_m("f"))
            _prop(etree.SubElement(f, _m("fPr")), "type", val="noBar")
            f.append(_wrap("num", top))
            f.append(_wrap("den", bottom))
            return [self._delimited("(", ")", [f])]
        if name == "sqrt":
            rad = etree.Element(_m("rad"))
            deg: list[etree._Element] = []
            if self.peek() == "[":
                self.next()
                deg = self.expr(stop={"]"})
                self.expect("]")
            if not deg:
                _prop(etree.SubElement(rad, _m("radPr")), "degHide", val="1")
            rad.append(_wrap("deg", deg))
            rad.append(_wrap("e", self.group()))
            return [rad]
        if name == "left":
            open_ = self.delimiter()
            inner = self.expr(stop=set())
            if self.peek() != "\\right":
                raise OmmlError("\\left without \\right")
            self.next()
            close = self.delimiter()
            return [self._delimited(open_, close, inner)]
        if name in NARY:
            return [self.nary(NARY[name], stop or set())]
        if name in FUNCTIONS or name == "operatorname":
            fname = [_run(self.raw_group() if name == "operatorname" else name, plain=True)]
            if self.peek() in ("^", "_"):
                fname = [self.scripts(fname)]
            arg = self.group() if self.peek() is not None and self.peek() not in (stop or set()) else []
            func = etree.Element(_m("func"))
            func.append(_wrap("fName", fname))
            func.append(_wrap("e", arg))
            return [func]
        if name in ("mathrm", "text", "textrm", "mbox", "mathsf", "mathtt"):
            text = self.raw_group()
            return [_run(text, plain=True)] if text else []
        if name in ("mathbf", "boldsymbol", "bm"):
            return [_run(self.raw_group(), bold=True)]
        if name in ("mathit", "mathnormal"):
            return [_run(self.raw_group())]
        if name in ("overline", "bar"):
            bar = etree.Element(_m("bar"))
            _prop(etree.SubElement(bar, _m("barPr")), "pos", val="top")
            bar.append(_wrap("e", self.group()))
            return [bar]
        if name in ACCENTS:
            acc = etree.Element(_m("acc"))
            _prop(etree.SubElement(acc, _m("accPr")), "chr", val=ACCENTS[name])
            acc.append(_wrap("e", self.group()))
            return [acc]
        if name in GREEK:
            return [_run(GREEK[name])]
        if name in SYMBOLS:
            return [_run(SYMBOLS[name])]
        if name in SPACES:
            return [_run(SPACES[name])] if SPACES[name] else []
        if name in ("limits", "nolimits", "displaystyle", "textstyle"):
            return []
        raise OmmlError(f"unsupported command {tok!r}")

    def _delimited(self, open_: str, close: str, inner: list[etree._Element]) -> etree._Element:
        d = etree.Element(_m("d"))
        pr = etree.SubElement(d, _m("dPr"))
        _prop(pr, "begChr", val=open_)
        _prop(pr, "endChr", val=close)
        d.append(_wrap("e", inner))
        return d

    def nary(self, chr_: str, stop: set[str]) -> etree._Element:
        under = False
        sub = sup = None
        while self.peek() in ("^", "_", "\\limits", "\\nolimits"):
            tok = self.next()
            if tok == "\\limits":
                under = True
            elif tok == "^":
                sup = self.group()
            elif tok == "_":
                sub = self.group()
        # The operand runs to the next top-level `+`, `-`, or `=` (SymPy brackets sums).
        body: list[etree._Element] = []
        while self.peek() is not None and self.peek() not in stop and self.peek() not in ("+", "-", "=", "\\right"):
            if self.peek() in ("^", "_"):
                base = body.pop() if body else _run("​")
                body.append(self.scripts([base]))
                continue
            body.extend(self.atom(stop))
        n = etree.Element(_m("nary"))
        pr = etree.SubElement(n, _m("naryPr"))
        _prop(pr, "chr", val=chr_)
        _prop(pr, "limLoc", val="undOvr" if (under or chr_ in "∑∏∐⋃⋂") else "subSup")
        if sub is None:
            _prop(pr, "subHide", val="1")
        if sup is None:
            _prop(pr, "supHide", val="1")
        n.append(_wrap("sub", sub or []))
        n.append(_wrap("sup", sup or []))
        n.append(_wrap("e", body))
        return n


def latex_to_omath(latex: str) -> etree._Element:
    """``<m:oMath>`` for a LaTeX expression; :class:`OmmlError` outside the subset."""
    src = latex.strip()
    for wrapper in ("$$", "$"):
        if src.startswith(wrapper) and src.endswith(wrapper) and len(src) > 2 * len(wrapper):
            src = src[len(wrapper) : -len(wrapper)]
            break
    if not src:
        raise OmmlError("empty equation")
    parser = _Parser(tokenize(src))
    items = parser.expr(stop=set())
    if parser.peek() is not None:
        raise OmmlError(f"unexpected {parser.peek()!r}")
    omath = etree.Element(_m("oMath"), nsmap={"m": M})
    omath.extend(items)
    return omath


R = "http://schemas.openxmlformats.org/officeDocument/2006/relationships"
P = "http://schemas.openxmlformats.org/presentationml/2006/main"


def equation_paragraph(latex: str) -> etree._Element:
    """An ``<a:p>`` holding the equation as PowerPoint writes one: ``a14:m`` around an
    ``m:oMathPara``. It belongs in a shape inside the ``a14`` choice of :func:`wrap_equation_shape`,
    which declares the ``a14`` namespace and holds the fallback for other consumers."""
    omath = latex_to_omath(latex)
    p = etree.Element(f"{{{A}}}p", nsmap={"a": A, "a14": A14})
    ppr = etree.SubElement(p, f"{{{A}}}pPr", algn="ctr")
    etree.SubElement(ppr, f"{{{A}}}buNone")
    m14 = etree.SubElement(p, f"{{{A14}}}m")
    para = etree.SubElement(m14, _m("oMathPara"), nsmap={"m": M})
    _prop(etree.SubElement(para, _m("oMathParaPr")), "jc", val="centerGroup")
    para.append(omath)
    return p


def fallback_shape(sp: etree._Element, image_rid: str | None, image_size: tuple[int, int] | None,
                   latex: str) -> etree._Element:
    """The ``mc:Fallback`` copy of an equation shape, as PowerPoint writes it: the same shape (id,
    name, position; not a placeholder) filled with a picture of the equation (``a:blipFill``; ``image_size`` is its
    natural size in EMU, kept, centred, and only scaled down to fit) and an empty paragraph.
    Without a picture the LaTeX is the text."""
    fb = copy.deepcopy(sp)
    # Not a placeholder: an empty placeholder is not drawn by some consumers (LibreOffice skips
    # it in print and PDF), and the shape's position is explicit anyway.
    for ph in fb.findall(f"{{{P}}}nvSpPr/{{{P}}}nvPr/{{{P}}}ph"):
        ph.getparent().remove(ph)
    body = fb.find(f"{{{P}}}txBody")
    if body is not None:
        body_pr = body.find(f"{{{A}}}bodyPr")
        if body_pr is not None:  # the picture fixes the size: no text autofit
            for fit in ("spAutoFit", "normAutofit", "noAutofit"):
                for old in body_pr.findall(f"{{{A}}}{fit}"):
                    body_pr.remove(old)
        for old in body.findall(f"{{{A}}}p"):
            body.remove(old)
        para = etree.SubElement(body, f"{{{A}}}p")
        if image_rid is None:
            ppr = etree.SubElement(para, f"{{{A}}}pPr", algn="ctr")
            etree.SubElement(ppr, f"{{{A}}}buNone")
            r = etree.SubElement(para, f"{{{A}}}r")
            etree.SubElement(r, f"{{{A}}}rPr", lang="en-US")
            etree.SubElement(r, f"{{{A}}}t").text = latex
        else:
            etree.SubElement(para, f"{{{A}}}endParaRPr", lang="en-US")
    if image_rid is None:
        return fb
    sppr = fb.find(f"{{{P}}}spPr")
    if sppr is None:
        sppr = etree.SubElement(fb, f"{{{P}}}spPr")
    for fill in ("noFill", "solidFill", "gradFill", "blipFill", "pattFill", "grpFill"):
        for old in sppr.findall(f"{{{A}}}{fill}"):
            sppr.remove(old)
    blip_fill = etree.Element(f"{{{A}}}blipFill", rotWithShape="1")
    etree.SubElement(blip_fill, f"{{{A}}}blip", {f"{{{R}}}embed": image_rid}, nsmap={"r": R})
    stretch = etree.SubElement(blip_fill, f"{{{A}}}stretch")
    rect = etree.SubElement(stretch, f"{{{A}}}fillRect")
    xfrm_ext = sppr.find(f"{{{A}}}xfrm/{{{A}}}ext")
    if image_size and xfrm_ext is not None:
        # The picture at its natural size (scaled down to fit), centred in the shape.
        cx, cy = int(xfrm_ext.get("cx", "0")), int(xfrm_ext.get("cy", "0"))
        w, h = image_size
        if cx > 0 and cy > 0 and w > 0 and h > 0:
            scale = min(1.0, cx / w, cy / h)
            for side_a, side_b, room, used in (("l", "r", cx, w * scale), ("t", "b", cy, h * scale)):
                inset = round((room - used) / 2 / room * 100000)
                if inset > 0:
                    rect.set(side_a, str(inset))
                    rect.set(side_b, str(inset))
    # Fill goes after the transform and the geometry (CT_ShapeProperties order).
    anchor = None
    for name in ("xfrm", "custGeom", "prstGeom"):
        found = sppr.find(f"{{{A}}}{name}")
        if found is not None:
            anchor = found
    if anchor is None:
        sppr.insert(0, blip_fill)
    else:
        anchor.addnext(blip_fill)
    return fb


def wrap_equation_shape(sp: etree._Element, fallback: etree._Element) -> etree._Element:
    """Puts ``sp`` (a shape whose text holds ``a14:m``) in place inside shape-level markup
    compatibility content, the way PowerPoint stores an equation: ``mc:AlternateContent`` with an
    ``mc:Choice Requires="a14"`` (declaring the ``a14`` namespace) holding the shape and an
    ``mc:Fallback`` holding ``fallback``. Returns the ``mc:AlternateContent`` element."""
    ac = etree.Element(f"{{{MC}}}AlternateContent", nsmap={"mc": MC})
    sp.addprevious(ac)
    choice = etree.SubElement(ac, f"{{{MC}}}Choice", nsmap={"a14": A14}, Requires="a14")
    choice.append(sp)
    etree.SubElement(ac, f"{{{MC}}}Fallback").append(fallback)
    return ac


def alternate_parts(ac: etree._Element) -> tuple[etree._Element | None, etree._Element | None]:
    """The choice shape and the fallback shape of an equation's ``mc:AlternateContent``."""
    choice = ac.find(f"{{{MC}}}Choice")
    fallback = ac.find(f"{{{MC}}}Fallback")
    first = lambda el: next(iter(el), None) if el is not None else None  # noqa: E731
    return first(choice), first(fallback)
