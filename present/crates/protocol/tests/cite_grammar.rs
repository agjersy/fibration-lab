//! The citation grammar of PROTOCOL 2.3: every row of the location table, the composite
//! forms, and malformed inputs.

use fibration_deck_protocol::cite::{Cite, CiteError, Fraction, Loc, RefKind, Span};
use fibration_deck_protocol::ids::RefId;

fn loc(s: &str) -> Loc {
    let parsed: Loc = s.parse().unwrap_or_else(|e| panic!("{s}: {e}"));
    assert_eq!(parsed.to_string(), s, "Display must reproduce the input");
    parsed
}

fn frac(s: &str) -> Fraction {
    Fraction::parse(s).unwrap()
}

fn span(start: u32, end: u32) -> Span {
    Span { start, end }
}

const ALL_KINDS: [RefKind; 13] = [
    RefKind::Pdf,
    RefKind::Docx,
    RefKind::Pptx,
    RefKind::Xlsx,
    RefKind::Csv,
    RefKind::Image,
    RefKind::Markdown,
    RefKind::Text,
    RefKind::Eml,
    RefKind::Msg,
    RefKind::Html,
    RefKind::AnalystFigure,
    RefKind::AnalystTable,
];

fn assert_kinds(l: &Loc, expected: &[RefKind]) {
    for k in ALL_KINDS {
        assert_eq!(l.fits_kind(k), expected.contains(&k), "{l} vs {k:?}");
    }
}

// ---- one test per row of the table ----

#[test]
fn row_pdf() {
    let l = loc("p11:c120-188");
    assert_eq!(
        l,
        Loc::Page {
            page: 11,
            chars: Some(span(120, 188))
        }
    );
    assert_kinds(&l, &[RefKind::Pdf]);
    assert_eq!(
        loc("p12"),
        Loc::Page {
            page: 12,
            chars: None
        }
    );
    assert_eq!(
        loc("p1:c0-0"),
        Loc::Page {
            page: 1,
            chars: Some(span(0, 0))
        }
    );
}

#[test]
fn row_plain_text_kinds_and_attachments() {
    let l = loc("c3400-3520");
    assert_eq!(l, Loc::Chars(span(3400, 3520)));
    assert_kinds(
        &l,
        &[
            RefKind::Docx,
            RefKind::Markdown,
            RefKind::Text,
            RefKind::Html,
            RefKind::Eml,
            RefKind::Msg,
        ],
    );

    // `att<n>/...` addresses an attachment's own loc, whatever kind the attachment is.
    let a = loc("att2/p3:c10-20");
    assert_eq!(
        a,
        Loc::Attachment {
            index: 2,
            loc: Box::new(Loc::Page {
                page: 3,
                chars: Some(span(10, 20))
            })
        }
    );
    assert_kinds(&a, &[RefKind::Eml, RefKind::Msg]);
    assert_eq!(
        loc("att1/c0-99"),
        Loc::Attachment {
            index: 1,
            loc: Box::new(Loc::Chars(span(0, 99)))
        }
    );
    // An attachment of an attached email.
    assert_eq!(
        loc("att1/att3/Sheet1!A1:B2"),
        Loc::Attachment {
            index: 1,
            loc: Box::new(Loc::Attachment {
                index: 3,
                loc: Box::new(Loc::SheetRange {
                    sheet: "Sheet1".into(),
                    range: "A1:B2".into()
                })
            })
        }
    );
    assert_eq!(loc("att1/p2/t1").table_index(), Some(1));
}

#[test]
fn row_pptx() {
    let l = loc("s4/sh12");
    assert_eq!(
        l,
        Loc::Slide {
            slide: 4,
            shape: Some(12)
        }
    );
    assert_kinds(&l, &[RefKind::Pptx]);
    assert_eq!(
        loc("s4"),
        Loc::Slide {
            slide: 4,
            shape: None
        }
    );
}

#[test]
fn row_xlsx() {
    let l = loc("Q3!B2:F14");
    assert_eq!(
        l,
        Loc::SheetRange {
            sheet: "Q3".into(),
            range: "B2:F14".into()
        }
    );
    assert_kinds(&l, &[RefKind::Xlsx]);
    let n = loc("name:PipelineTable");
    assert_eq!(
        n,
        Loc::NamedRange {
            name: "PipelineTable".into()
        }
    );
    assert_kinds(&n, &[RefKind::Xlsx]);

    // Sheet names that look like other forms are still sheets: `!` decides.
    for (text, sheet, range) in [
        ("p11!A1", "p11", "A1"),
        ("c1-2!A:C", "c1-2", "A:C"),
        ("s4!1:5", "s4", "1:5"),
        ("r1-13!B2", "r1-13", "B2"),
        ("full!A1", "full", "A1"),
        ("Q3 Data (final)!B2:F14", "Q3 Data (final)", "B2:F14"),
        ("Why!Not!A1", "Why!Not", "A1"),
        ("attic!A1", "attic", "A1"),
        ("Été 2026!A1:B2", "Été 2026", "A1:B2"),
    ] {
        assert_eq!(
            loc(text),
            Loc::SheetRange {
                sheet: sheet.into(),
                range: range.into()
            },
            "{text}"
        );
    }
}

#[test]
fn row_csv() {
    let l = loc("r1-13");
    assert_eq!(
        l,
        Loc::Rows {
            rows: span(1, 13),
            cols: None
        }
    );
    assert_kinds(&l, &[RefKind::Csv]);
    assert_eq!(
        loc("r2-3:c1-3"),
        Loc::Rows {
            rows: span(2, 3),
            cols: Some(span(1, 3))
        }
    );
    assert_eq!(
        loc("r5-5"),
        Loc::Rows {
            rows: span(5, 5),
            cols: None
        }
    );
}

#[test]
fn row_image() {
    let l = loc("full");
    assert_eq!(l, Loc::ImageFull);
    assert_kinds(&l, &[RefKind::Image]);
    let b = loc("box:0.1,0.2,0.5,0.5");
    assert_eq!(
        b,
        Loc::ImageBox {
            x: frac("0.1"),
            y: frac("0.2"),
            w: frac("0.5"),
            h: frac("0.5")
        }
    );
    assert_kinds(&b, &[RefKind::Image]);
    // Fractions keep their spelling.
    loc("box:0,0,1,1");
    loc("box:0.10,0.250,1.0,0.5");
    match loc("box:0.10,0.250,1.0,0.5") {
        Loc::ImageBox { x, w, .. } => {
            assert_eq!(x.value(), 0.1);
            assert_eq!(w.value(), 1.0);
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn row_analyst() {
    let l = loc("run:20261104-1132-ab12/residuals");
    assert_eq!(
        l,
        Loc::AnalystRun {
            run_id: "20261104-1132-ab12".into(),
            name: "residuals".into()
        }
    );
    assert_kinds(&l, &[RefKind::AnalystFigure, RefKind::AnalystTable]);
    // An item that happens to be called `t1` is the item, not a table suffix.
    assert_eq!(
        loc("run:20261104-1132-ab12/t1"),
        Loc::AnalystRun {
            run_id: "20261104-1132-ab12".into(),
            name: "t1".into()
        }
    );
    assert_eq!(loc("run:20261104-1132-ab12/t1/t2").table_index(), Some(2));
}

#[test]
fn row_extracted_table() {
    let l = loc("p12/t1");
    assert_eq!(
        l,
        Loc::Table {
            base: Box::new(Loc::Page {
                page: 12,
                chars: None
            }),
            index: 1
        }
    );
    assert_eq!(l.table_index(), Some(1));
    assert_kinds(&l, &[RefKind::Pdf]);
    // The suffix composes with every base form.
    for (text, kinds) in [
        ("p12:c0-40/t2", &[RefKind::Pdf][..]),
        (
            "c3400-3520/t1",
            &[
                RefKind::Docx,
                RefKind::Markdown,
                RefKind::Text,
                RefKind::Html,
                RefKind::Eml,
                RefKind::Msg,
            ][..],
        ),
        ("s4/t1", &[RefKind::Pptx][..]),
        ("s4/sh12/t1", &[RefKind::Pptx][..]),
        ("Q3!B2:F14/t1", &[RefKind::Xlsx][..]),
        ("name:Pipeline/t3", &[RefKind::Xlsx][..]),
        ("r1-13/t1", &[RefKind::Csv][..]),
        ("full/t1", &[RefKind::Image][..]),
        (
            "run:20261104-1132-ab12/params/t1",
            &[RefKind::AnalystFigure, RefKind::AnalystTable][..],
        ),
    ] {
        let l = loc(text);
        assert!(matches!(l, Loc::Table { .. }), "{text}");
        assert_kinds(&l, kinds);
    }
    assert_eq!(loc("p12").table_index(), None);
}

// ---- full citations ----

#[test]
fn cite_examples_from_protocol_parse_and_round_trip() {
    for text in [
        "ref:r-q3rep@3f9a1c2b7e01#p11:c120-188",
        "ref:r-pipe@aa01b2c3d4e5#Pipeline!A1:E9",
        "ref:r-q3rep@3f9a1c2b7e01#c3400-3520",
        "ref:r-q3rep@3f9a1c2b7e01#s4/sh12",
        "ref:r-q3rep@3f9a1c2b7e01#Q3!B2:F14",
        "ref:r-q3rep@3f9a1c2b7e01#name:Totals",
        "ref:r-q3rep@3f9a1c2b7e01#r1-13",
        "ref:r-q3rep@3f9a1c2b7e01#full",
        "ref:r-q3rep@3f9a1c2b7e01#box:0.1,0.2,0.5,0.5",
        "ref:r-q3rep@3f9a1c2b7e01#run:20261104-1132-ab12/residuals",
        "ref:r-q3rep@3f9a1c2b7e01#p12/t1",
        "ref:r-q3rep@3f9a1c2b7e01#att1/p2",
        "ref:r-0123456789ab@000000000000#Sheet #1!A1",
        "ref:r-q3-report@3f9a1c2b7e01#p11",
        "ref:r-a@3f9a1c2b7e01#full",
    ] {
        let c: Cite = text.parse().unwrap_or_else(|e| panic!("{text}: {e}"));
        assert_eq!(c.to_string(), text);
        assert_eq!(c.as_str(), text);
        let rebuilt = Cite::new(&c.ref_id(), c.version(), &c.loc()).unwrap();
        assert_eq!(rebuilt, c);
        let json = serde_json::to_string(&c).unwrap();
        assert_eq!(json, serde_json::to_string(text).unwrap());
        assert_eq!(serde_json::from_str::<Cite>(&json).unwrap(), c);
    }
}

#[test]
fn cite_accessors() {
    let c = Cite::parse("ref:r-q3rep@3f9a1c2b7e01#p11:c120-188").unwrap();
    assert_eq!(c.ref_id(), RefId::new("r-q3rep").unwrap());
    assert_eq!(c.version(), "3f9a1c2b7e01");
    assert_eq!(c.loc_str(), "p11:c120-188");
    assert_eq!(
        c.loc(),
        Loc::Page {
            page: 11,
            chars: Some(span(120, 188))
        }
    );
    let newer = c.with_version("0123456789ab").unwrap();
    assert_eq!(newer.as_str(), "ref:r-q3rep@0123456789ab#p11:c120-188");
    assert!(c.with_version("nope").is_err());
}

#[test]
fn cite_new_rejects_parts_the_grammar_cannot_express() {
    let id = RefId::new("r-q3rep").unwrap();
    assert!(Cite::new(&id, "3f9a1c2b7e01", &Loc::ImageFull).is_ok());
    assert!(Cite::new(&id, "3F9A1C2B7E01", &Loc::ImageFull).is_err());
    let bad_sheet = Loc::SheetRange {
        sheet: "a/b".into(),
        range: "A1".into(),
    };
    assert!(Cite::new(&id, "3f9a1c2b7e01", &bad_sheet).is_err());
    let reversed = Loc::Chars(span(10, 5));
    assert!(Cite::new(&id, "3f9a1c2b7e01", &reversed).is_err());
}

// ---- malformed inputs ----

#[test]
fn malformed_citation_envelopes() {
    type Expect = fn(&CiteError) -> bool;
    let cases: &[(&str, Expect)] = &[
        ("", |e| matches!(e, CiteError::MissingScheme)),
        ("r-q3rep@3f9a1c2b7e01#p11", |e| {
            matches!(e, CiteError::MissingScheme)
        }),
        ("REF:r-q3rep@3f9a1c2b7e01#p11", |e| {
            matches!(e, CiteError::MissingScheme)
        }),
        (" ref:r-q3rep@3f9a1c2b7e01#p11", |e| {
            matches!(e, CiteError::MissingScheme)
        }),
        // The unversioned form used in ARCHITECTURE.md section 5 is not a valid citation.
        ("ref:q3-report#p11:c120-188", |e| {
            matches!(e, CiteError::MissingVersion)
        }),
        ("ref:r-q3rep#p11", |e| {
            matches!(e, CiteError::MissingVersion)
        }),
        ("ref:r-q3rep@3f9a1c2b7e01", |e| {
            matches!(e, CiteError::MissingLoc)
        }),
        ("ref:r-q3rep@3f9a1c2b7e01p11", |e| {
            matches!(e, CiteError::MissingLoc)
        }),
        ("ref:r-@3f9a1c2b7e01#p11", |e| {
            matches!(e, CiteError::RefId(_))
        }),
        ("ref:q3rep@3f9a1c2b7e01#p11", |e| {
            matches!(e, CiteError::RefId(_))
        }),
        ("ref:r-Q3REP@3f9a1c2b7e01#p11", |e| {
            matches!(e, CiteError::RefId(_))
        }),
        ("ref:r--q3@3f9a1c2b7e01#p11", |e| {
            matches!(e, CiteError::RefId(_))
        }),
        ("ref:r-0123456789abcdefghijklmno@3f9a1c2b7e01#p11", |e| {
            matches!(e, CiteError::RefId(_))
        }),
        ("ref:r-q3rep@#p11", |e| matches!(e, CiteError::Version(_))),
        ("ref:r-q3rep@3f9a1c2b7e0#p11", |e| {
            matches!(e, CiteError::Version(_))
        }),
        ("ref:r-q3rep@3f9a1c2b7e012#p11", |e| {
            matches!(e, CiteError::Version(_))
        }),
        ("ref:r-q3rep@3F9A1C2B7E01#p11", |e| {
            matches!(e, CiteError::Version(_))
        }),
        ("ref:r-q3rep@3f9a1c2b7e0g#p11", |e| {
            matches!(e, CiteError::Version(_))
        }),
        ("ref:r-q3rep@latest#p11", |e| {
            matches!(e, CiteError::Version(_))
        }),
        ("ref:r-q3rep@3f9a1c2b7e01#", |e| {
            matches!(e, CiteError::Loc(_))
        }),
        ("ref:r-q3rep@3f9a1c2b7e01#page 11", |e| {
            matches!(e, CiteError::Loc(_))
        }),
        ("ref:r-q3rep@3f9a1c2b7e01#p11\n", |e| {
            matches!(e, CiteError::Loc(_))
        }),
    ];
    for (text, expected) in cases {
        match Cite::parse(text) {
            Ok(c) => panic!("{text:?} parsed as {c}"),
            Err(e) => assert!(expected(&e), "{text:?}: unexpected error {e:?}"),
        }
        assert!(serde_json::from_value::<Cite>(serde_json::json!(text)).is_err());
    }
    assert!(serde_json::from_str::<Cite>("12").is_err());
}

#[test]
fn malformed_locations() {
    for text in [
        "",
        " ",
        "x",
        "P11",
        "page11",
        // pdf
        "p",
        "p-1",
        "p011",
        "p+1",
        "p1.5",
        "p11:",
        "p11:120-188",
        "p11:c120",
        "p11:c120-",
        "p11:c-188",
        "p11:c188-120",
        "p11:c120-188-200",
        "p11:c120–188",
        "p11:c0120-188",
        "p11 :c1-2",
        "p99999999999",
        // text
        "c",
        "c10",
        "c10-",
        "c-10",
        "c20-10",
        "c1-2-3",
        "c01-2",
        "c1_2",
        // attachments
        "att",
        "att1",
        "att1/",
        "att/p1",
        "attx/p1",
        "att01/p1",
        "att1/att2",
        "att1//p1",
        // pptx
        "s",
        "s04",
        "s4/",
        "s4/sh",
        "s4/12",
        "s4/shx",
        "s4/sh012",
        "s4/sh12/sh13",
        "s4/sh12/",
        // xlsx
        "!A1",
        "Q3!",
        "Q3!b2",
        "Q3!B0",
        "Q3!B2:",
        "Q3!B2:F",
        "Q3!$B$2",
        "Q3!B2;F14",
        "Sales/2026!A1",
        "name:",
        "name:a/b",
        // csv
        "r",
        "r1",
        "r1-",
        "r0-5",
        "r13-1",
        "r1-13:",
        "r1-13:c",
        "r1-13:c0-2",
        "r1-13:c3-1",
        "r1-13:1-2",
        "r01-13",
        // image
        "Full",
        "full ",
        "box",
        "box:",
        "box:0.1,0.2,0.5",
        "box:0.1,0.2,0.5,0.5,0.1",
        "box:0.1,0.2,0.5,1.5",
        "box:-0.1,0.2,0.5,0.5",
        "box:.1,0.2,0.5,0.5",
        "box:0.1, 0.2,0.5,0.5",
        "box:a,b,c,d",
        "box:1e-1,0,1,1",
        // fibration
        "run:",
        "run:/residuals",
        "run:20261104-1132-ab12",
        "run:20261104-1132-ab12/",
        "run:20261104 1132/residuals",
        "run:20261104-1132-ab12/a/b",
        "run:20261104-1132-ab12/a/t1/t2",
        // tables
        "/t1",
        "t1",
        "p12/t",
        "p12/tx",
        "p12/t01",
        "p12/t1/t2",
        "p12/T1",
        "p12/t1/",
    ] {
        match text.parse::<Loc>() {
            Ok(l) => panic!("{text:?} parsed as {l:?}"),
            Err(e) => {
                assert_eq!(e.loc, text);
                assert!(e.to_string().contains("invalid location"), "{e}");
            }
        }
    }
}
