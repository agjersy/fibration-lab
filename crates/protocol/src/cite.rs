//! Citations: `ref:<ref_id>@<version>#<loc>` (PROTOCOL 2.3).
//!
//! [`Cite`] is a validated string newtype: it can only hold text that parses under the
//! grammar, and it serializes as that exact text. [`Loc`] is the typed location. Parsing is
//! independent of the reference kind (the grammar rows of PROTOCOL 2.3 do not overlap);
//! [`Loc::fits_kind`] says whether a location form is one the given kind uses.
//!
//! Canonical form: every integer is decimal without a sign or leading zeros, so
//! `Display` of a parsed value is byte-identical to the input.

use crate::ids::{IdError, RefId};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;
use thiserror::Error;
use ts_rs::TS;

/// Kind of a reference file (PROTOCOL 1.3 `RefKind`, 4.3 `kind`).
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    PartialOrd,
    Ord,
    Serialize,
    Deserialize,
    TS,
    schemars::JsonSchema,
)]
#[serde(rename_all = "kebab-case")]
pub enum RefKind {
    Pdf,
    Docx,
    Pptx,
    Xlsx,
    Csv,
    Image,
    Markdown,
    Text,
    Eml,
    Msg,
    Html,
    AnalystFigure,
    AnalystTable,
}

/// A citation string could not be parsed.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum CiteError {
    #[error("citation must start with `ref:`")]
    MissingScheme,
    #[error("citation has no `@<version>` after the reference id")]
    MissingVersion,
    #[error("citation has no `#<loc>` after the version")]
    MissingLoc,
    #[error(transparent)]
    RefId(#[from] IdError),
    #[error("invalid reference version `{0}` (expected 12 lowercase hex digits)")]
    Version(String),
    #[error(transparent)]
    Loc(#[from] LocError),
}

/// A location string could not be parsed.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("invalid location `{loc}`: {reason}")]
pub struct LocError {
    pub loc: String,
    pub reason: &'static str,
}

/// The first 12 hex digits of a reference file's SHA-256 (PROTOCOL 2.3).
pub fn is_version(s: &str) -> bool {
    s.len() == 12
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// A citation, `ref:<ref_id>@<version>#<loc>`, validated against the full grammar.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, TS)]
pub struct Cite(String);

impl Cite {
    /// Loose pattern used in the JSON Schema; [`Cite::parse`] additionally checks `loc`.
    pub const PATTERN: &'static str = "^ref:r-[0-9a-z][0-9a-z-]{0,23}@[0-9a-f]{12}#.+$";

    /// Parses and validates a citation string.
    pub fn parse(s: &str) -> Result<Cite, CiteError> {
        let (ref_id, version, loc) = split(s)?;
        RefId::new(ref_id)?;
        if !is_version(version) {
            return Err(CiteError::Version(version.to_string()));
        }
        loc.parse::<Loc>()?;
        Ok(Cite(s.to_string()))
    }

    /// Builds a citation from its parts. Fails if `version` is not 12 hex digits or `loc`
    /// was constructed with field values its own grammar cannot express.
    pub fn new(ref_id: &RefId, version: &str, loc: &Loc) -> Result<Cite, CiteError> {
        Cite::parse(&format!("ref:{ref_id}@{version}#{loc}"))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    fn parts(&self) -> (&str, &str, &str) {
        // Invariant: the inner string was validated by `parse`.
        split(&self.0).unwrap_or(("", "", ""))
    }

    /// The cited reference.
    pub fn ref_id(&self) -> RefId {
        RefId::new(self.parts().0).expect("validated at construction")
    }

    /// The cited reference version (12 hex digits).
    pub fn version(&self) -> &str {
        self.parts().1
    }

    /// The location text, as passed to the kernel's `refs.read` and `refs.table`.
    pub fn loc_str(&self) -> &str {
        self.parts().2
    }

    /// The typed location.
    pub fn loc(&self) -> Loc {
        self.loc_str().parse().expect("validated at construction")
    }

    /// The same reference and location at another version.
    pub fn with_version(&self, version: &str) -> Result<Cite, CiteError> {
        let (ref_id, _, loc) = self.parts();
        Cite::parse(&format!("ref:{ref_id}@{version}#{loc}"))
    }
}

fn split(s: &str) -> Result<(&str, &str, &str), CiteError> {
    let rest = s.strip_prefix("ref:").ok_or(CiteError::MissingScheme)?;
    let (ref_id, rest) = rest.split_once('@').ok_or(CiteError::MissingVersion)?;
    let (version, loc) = rest.split_once('#').ok_or(CiteError::MissingLoc)?;
    Ok((ref_id, version, loc))
}

impl fmt::Display for Cite {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for Cite {
    type Err = CiteError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Cite::parse(s)
    }
}

impl TryFrom<String> for Cite {
    type Error = CiteError;
    fn try_from(s: String) -> Result<Self, Self::Error> {
        Cite::parse(&s)
    }
}

impl From<Cite> for String {
    fn from(c: Cite) -> String {
        c.0
    }
}

impl AsRef<str> for Cite {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl Serialize for Cite {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for Cite {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Cite::parse(&s).map_err(serde::de::Error::custom)
    }
}

impl schemars::JsonSchema for Cite {
    fn schema_name() -> String {
        "Cite".to_string()
    }

    fn json_schema(_: &mut schemars::gen::SchemaGenerator) -> schemars::schema::Schema {
        crate::schema::string_with_pattern(Cite::PATTERN)
    }
}

/// A pair written `<start>-<end>` with `start <= end`. PROTOCOL 2.3 does not say whether
/// `end` is inclusive; this type only carries the two numbers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Span {
    pub start: u32,
    pub end: u32,
}

/// A fraction in `0..=1`, kept as written so formatting round-trips exactly
/// (`0.50` stays `0.50`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Fraction(String);

impl Fraction {
    /// Accepts `<digits>[.<digits>]` with a value in `0..=1`.
    pub fn parse(s: &str) -> Option<Fraction> {
        let (int, frac) = match s.split_once('.') {
            Some((i, f)) => (i, Some(f)),
            None => (s, None),
        };
        let digits = |t: &str| !t.is_empty() && t.bytes().all(|b| b.is_ascii_digit());
        if !digits(int) || !frac.map(digits).unwrap_or(true) {
            return None;
        }
        let v: f64 = s.parse().ok()?;
        (0.0..=1.0).contains(&v).then(|| Fraction(s.to_string()))
    }

    /// The shortest decimal text for `v`; `None` outside `0..=1`.
    pub fn from_f64(v: f64) -> Option<Fraction> {
        Fraction::parse(&format!("{v}"))
    }

    pub fn value(&self) -> f64 {
        self.0.parse().unwrap_or(0.0)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Fraction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A location inside a reference (the `loc` column of PROTOCOL 2.3).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Loc {
    /// pdf: `p<page>[:c<start>-<end>]`, character offsets within the page's extracted text.
    Page { page: u32, chars: Option<Span> },
    /// docx, markdown, text, html, eml, msg: `c<start>-<end>` over the extracted plain text.
    Chars(Span),
    /// eml, msg: `att<n>/<loc>`, a location inside the n-th attachment.
    Attachment { index: u32, loc: Box<Loc> },
    /// pptx: `s<slide>[/sh<shape_id>]`.
    Slide { slide: u32, shape: Option<u32> },
    /// xlsx: `<sheet>!<A1 range>`.
    SheetRange { sheet: String, range: String },
    /// xlsx: `name:<named range>`.
    NamedRange { name: String },
    /// csv: `r<row1>-<row2>[:c<col1>-<col2>]`, 1-based.
    Rows { rows: Span, cols: Option<Span> },
    /// image: `full`.
    ImageFull,
    /// image: `box:<x>,<y>,<w>,<h>` in fractions of the image.
    ImageBox {
        x: Fraction,
        y: Fraction,
        w: Fraction,
        h: Fraction,
    },
    /// analyst-figure, analyst-table: `run:<run_id>/<name>`.
    AnalystRun { run_id: String, name: String },
    /// Any extracted table: `<base loc>/t<n>`. `base` is never `Attachment` or `Table`.
    Table { base: Box<Loc>, index: u32 },
}

impl Loc {
    /// Whether references of `kind` use this form of location.
    pub fn fits_kind(&self, kind: RefKind) -> bool {
        use RefKind::*;
        match self {
            Loc::Page { .. } => kind == Pdf,
            Loc::Chars(_) => matches!(kind, Docx | Markdown | Text | Html | Eml | Msg),
            Loc::Attachment { .. } => matches!(kind, Eml | Msg),
            Loc::Slide { .. } => kind == Pptx,
            Loc::SheetRange { .. } | Loc::NamedRange { .. } => kind == Xlsx,
            Loc::Rows { .. } => kind == Csv,
            Loc::ImageFull | Loc::ImageBox { .. } => kind == Image,
            Loc::AnalystRun { .. } => matches!(kind, AnalystFigure | AnalystTable),
            Loc::Table { base, .. } => base.fits_kind(kind),
        }
    }

    /// The table index when this location addresses an extracted table.
    pub fn table_index(&self) -> Option<u32> {
        match self {
            Loc::Table { index, .. } => Some(*index),
            Loc::Attachment { loc, .. } => loc.table_index(),
            _ => None,
        }
    }
}

impl fmt::Display for Loc {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Loc::Page { page, chars: None } => write!(f, "p{page}"),
            Loc::Page {
                page,
                chars: Some(c),
            } => write!(f, "p{page}:c{}-{}", c.start, c.end),
            Loc::Chars(c) => write!(f, "c{}-{}", c.start, c.end),
            Loc::Attachment { index, loc } => write!(f, "att{index}/{loc}"),
            Loc::Slide { slide, shape: None } => write!(f, "s{slide}"),
            Loc::Slide {
                slide,
                shape: Some(sh),
            } => write!(f, "s{slide}/sh{sh}"),
            Loc::SheetRange { sheet, range } => write!(f, "{sheet}!{range}"),
            Loc::NamedRange { name } => write!(f, "name:{name}"),
            Loc::Rows { rows, cols: None } => write!(f, "r{}-{}", rows.start, rows.end),
            Loc::Rows {
                rows,
                cols: Some(c),
            } => write!(f, "r{}-{}:c{}-{}", rows.start, rows.end, c.start, c.end),
            Loc::ImageFull => f.write_str("full"),
            Loc::ImageBox { x, y, w, h } => write!(f, "box:{x},{y},{w},{h}"),
            Loc::AnalystRun { run_id, name } => write!(f, "run:{run_id}/{name}"),
            Loc::Table { base, index } => write!(f, "{base}/t{index}"),
        }
    }
}

impl FromStr for Loc {
    type Err = LocError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        parse_loc(s).map_err(|reason| LocError {
            loc: s.to_string(),
            reason,
        })
    }
}

type R<T> = Result<T, &'static str>;

/// A canonical unsigned decimal: digits only, no leading zeros.
fn num(s: &str) -> Option<u32> {
    let canonical = !s.is_empty()
        && s.bytes().all(|b| b.is_ascii_digit())
        && (s.len() == 1 || !s.starts_with('0'));
    if canonical {
        s.parse().ok()
    } else {
        None
    }
}

fn span(s: &str, min: u32, what: &'static str) -> R<Span> {
    let (a, b) = s.split_once('-').ok_or(what)?;
    let (start, end) = (num(a).ok_or(what)?, num(b).ok_or(what)?);
    if start < min || end < start {
        return Err(what);
    }
    Ok(Span { start, end })
}

fn table_suffix(s: &str) -> Option<u32> {
    num(s.strip_prefix('t')?)
}

fn parse_loc(s: &str) -> R<Loc> {
    if s.is_empty() {
        return Err("empty location");
    }
    // `att<n>/<loc>`: sheet names and the other forms cannot contain `/` before this point.
    if let Some((n, inner)) = s.strip_prefix("att").and_then(|r| r.split_once('/')) {
        if let Some(index) = num(n) {
            let loc = Box::new(parse_loc(inner)?);
            return Ok(Loc::Attachment { index, loc });
        }
    }
    // `run:<run_id>/<name>[/t<n>]`: the item name owns the first segment after the run id,
    // so an item called `t1` is not mistaken for a table suffix.
    if let Some(rest) = s.strip_prefix("run:") {
        let mut segs = rest.split('/');
        let (run_id, name, table, extra) = (segs.next(), segs.next(), segs.next(), segs.next());
        let run_id = run_id.filter(|r| is_run_id(r)).ok_or("bad run id")?;
        let name = name
            .filter(|n| is_name(n))
            .ok_or("expected run:<run_id>/<name>")?;
        let base = Loc::AnalystRun {
            run_id: run_id.to_string(),
            name: name.to_string(),
        };
        return match (table, extra) {
            (None, _) => Ok(base),
            (Some(t), None) => Ok(Loc::Table {
                base: Box::new(base),
                index: table_suffix(t).ok_or("expected /t<n> after the run item")?,
            }),
            _ => Err("too many `/` segments in run location"),
        };
    }
    if let Some((base, last)) = s.rsplit_once('/') {
        if let Some(index) = table_suffix(last) {
            let base = Box::new(parse_base(base)?);
            return Ok(Loc::Table { base, index });
        }
    }
    parse_base(s)
}

fn parse_base(s: &str) -> R<Loc> {
    if s == "full" {
        return Ok(Loc::ImageFull);
    }
    if let Some(rest) = s.strip_prefix("box:") {
        let what = "expected box:<x>,<y>,<w>,<h> with fractions in 0..1";
        let mut it = rest.split(',').map(Fraction::parse);
        let mut next = || it.next().flatten().ok_or(what);
        let (x, y, w, h) = (next()?, next()?, next()?, next()?);
        if it.next().is_some() {
            return Err(what);
        }
        return Ok(Loc::ImageBox { x, y, w, h });
    }
    if let Some(name) = s.strip_prefix("name:") {
        if !is_name(name) {
            return Err("expected name:<named range>");
        }
        return Ok(Loc::NamedRange {
            name: name.to_string(),
        });
    }
    // A sheet name may start with any letter, so `!` is tested before the letter forms.
    if let Some((sheet, range)) = s.rsplit_once('!') {
        if !is_name(sheet) {
            return Err("bad sheet name");
        }
        if !is_a1_range(range) {
            return Err("bad A1 range");
        }
        return Ok(Loc::SheetRange {
            sheet: sheet.to_string(),
            range: range.to_string(),
        });
    }
    if let Some(rest) = s.strip_prefix('p') {
        let what = "expected p<page>[:c<start>-<end>]";
        let (page, chars) = match rest.split_once(':') {
            Some((p, c)) => (p, Some(span(c.strip_prefix('c').ok_or(what)?, 0, what)?)),
            None => (rest, None),
        };
        let page = num(page).ok_or(what)?;
        return Ok(Loc::Page { page, chars });
    }
    if let Some(rest) = s.strip_prefix('c') {
        return Ok(Loc::Chars(span(rest, 0, "expected c<start>-<end>")?));
    }
    if let Some(rest) = s.strip_prefix('s') {
        let what = "expected s<slide>[/sh<shape_id>]";
        let (slide, shape) = match rest.split_once('/') {
            Some((sl, sh)) => (sl, Some(sh.strip_prefix("sh").and_then(num).ok_or(what)?)),
            None => (rest, None),
        };
        let slide = num(slide).ok_or(what)?;
        return Ok(Loc::Slide { slide, shape });
    }
    if let Some(rest) = s.strip_prefix('r') {
        let what = "expected r<row1>-<row2>[:c<col1>-<col2>], 1-based";
        let (rows, cols) = match rest.split_once(':') {
            Some((r, c)) => (r, Some(span(c.strip_prefix('c').ok_or(what)?, 1, what)?)),
            None => (rest, None),
        };
        let rows = span(rows, 1, what)?;
        return Ok(Loc::Rows { rows, cols });
    }
    Err("unrecognized location form")
}

/// Non-empty, no `/`, no control characters.
fn is_name(s: &str) -> bool {
    !s.is_empty() && !s.chars().any(|c| c == '/' || c.is_control())
}

fn is_run_id(s: &str) -> bool {
    !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}

/// `B2`, `B2:F14`, `A:C`, or `1:5`; uppercase columns (1 to 3 letters), no `$`.
fn is_a1_range(s: &str) -> bool {
    #[derive(PartialEq)]
    enum Part {
        Cell,
        Col,
        Row,
    }
    fn part(p: &str) -> Option<Part> {
        let letters = p.bytes().take_while(|b| b.is_ascii_uppercase()).count();
        let digits = &p[letters..];
        let row_ok = num(digits).map(|n| n >= 1).unwrap_or(false);
        match (letters, digits.is_empty()) {
            (1..=3, true) => Some(Part::Col),
            (1..=3, false) if row_ok => Some(Part::Cell),
            (0, false) if row_ok => Some(Part::Row),
            _ => None,
        }
    }
    match s.split_once(':') {
        None => part(s) == Some(Part::Cell),
        Some((a, b)) => matches!((part(a), part(b)), (Some(x), Some(y)) if x == y),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fraction_keeps_its_text() {
        assert_eq!(Fraction::parse("0.50").unwrap().to_string(), "0.50");
        assert_eq!(Fraction::parse("0.50").unwrap().value(), 0.5);
        assert_eq!(Fraction::from_f64(0.25).unwrap().as_str(), "0.25");
        assert_eq!(Fraction::from_f64(1.0).unwrap().as_str(), "1");
        for bad in ["", ".5", "1.", "1.5", "-0.1", "1e-1", "0,5", "+0.5", " 0.5"] {
            assert!(Fraction::parse(bad).is_none(), "{bad:?}");
        }
        assert!(Fraction::from_f64(1.5).is_none());
    }

    #[test]
    fn a1_ranges() {
        for ok in ["B2", "B2:F14", "A:C", "1:5", "XFD1048576", "AA10:AB20"] {
            assert!(is_a1_range(ok), "{ok}");
        }
        for bad in [
            "",
            "b2",
            "B0",
            "B02",
            "B2:",
            ":B2",
            "B2:C",
            "A:1",
            "$B$2",
            "ABCD1",
            "B2:F14:G1",
            "B 2",
        ] {
            assert!(!is_a1_range(bad), "{bad}");
        }
    }
}
