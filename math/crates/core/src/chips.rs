//! Chips (ARCHITECTURE 11, UX note §3): what can be done next, read off the scene.
//!
//! Chips are a pure function of the scene state and the engine's manifest for that state. No
//! model is involved, so they work offline and with no provider configured, and a chip is
//! offered only when the operation behind it is available in the current scene (the engine's
//! masking is the one source of truth).
//!
//! Three kinds, as in the UX note: *direct* chips carry the code of the cell they create;
//! *parameter* chips open a slider; *seed* chips pre-fill the chat for the user to finish.

use std::collections::HashSet;

use serde::Serialize;
use serde_json::Value;

/// At most this many chips are offered flat; grouping beyond that is a UI matter.
pub const MAX_CHIPS: usize = 8;
/// Object-specific chips are offered for the most recent curves only.
const MAX_CURVES: usize = 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ChipKind {
    Direct,
    Param,
    Seed,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Slider {
    pub name: String,
    pub value: f64,
    pub min: f64,
    pub max: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub step: Option<f64>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Chip {
    /// Stable for a given scene: `<action>:<target>`.
    pub id: String,
    pub label: String,
    pub kind: ChipKind,
    /// The engine operation the chip leads to; it is available in the current scene.
    pub op: String,
    /// Direct chips: the code of the cell the chip creates.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    /// Seed chips: the text placed in the chat input.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seed: Option<String>,
    /// Parameter chips: the slider to open.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub slider: Option<Slider>,
}

/// A Python string literal for `text` (a JSON string is one, for the text the engine prints).
fn py_str(text: &str) -> String {
    serde_json::to_string(text).unwrap_or_else(|_| "\"\"".to_string())
}

/// The code of a slider-commit cell.
pub fn param_set_code(name: &str, value: f64) -> String {
    format!("from fibration_math import param\nparam.set({name}={value})\n")
}

/// Whether `code` is a slider-commit cell for exactly this parameter.
pub fn is_param_commit(code: &str, name: &str) -> bool {
    let mut lines = code.lines().filter(|l| !l.trim().is_empty());
    let prefix = format!("param.set({name}=");
    lines.next() == Some("from fibration_math import param")
        && lines
            .next()
            .is_some_and(|l| l.starts_with(&prefix) && l.ends_with(')') && !l.contains(','))
        && lines.next().is_none()
}

fn available(api: &Value) -> HashSet<&str> {
    api["operations"]
        .as_array()
        .map(|ops| {
            ops.iter()
                .filter(|o| o["available"].as_bool().unwrap_or(true))
                .filter_map(|o| o["name"].as_str())
                .collect()
        })
        .unwrap_or_default()
}

/// The chips for a scene state and the engine manifest computed for that state.
pub fn for_scene(state: &Value, api: &Value) -> Vec<Chip> {
    let ops = available(api);
    let objects = state["objects"].as_array().cloned().unwrap_or_default();
    let mut chips: Vec<Chip> = Vec::new();

    if objects.is_empty() {
        if ops.contains("plot.explicit") {
            chips.push(Chip {
                id: "graph:new".into(),
                label: "Graph a function…".into(),
                kind: ChipKind::Seed,
                op: "plot.explicit".into(),
                code: None,
                seed: Some("graph y = ".into()),
                slider: None,
            });
        }
        if ops.contains("plot.implicit") {
            chips.push(Chip {
                id: "implicit:new".into(),
                label: "Plot an equation…".into(),
                kind: ChipKind::Seed,
                op: "plot.implicit".into(),
                code: None,
                seed: Some("plot the curve ".into()),
                slider: None,
            });
        }
    }

    // Parameters first: they are what the user most likely wants to move.
    if ops.contains("param.set") {
        if let Some(params) = state["params"].as_object() {
            let mut names: Vec<&String> = params.keys().collect();
            names.sort();
            for name in names {
                let p = &params[name];
                let (Some(value), Some(min), Some(max)) =
                    (p["value"].as_f64(), p["min"].as_f64(), p["max"].as_f64())
                else {
                    continue;
                };
                chips.push(Chip {
                    id: format!("adjust:{name}"),
                    label: format!("Adjust {name}"),
                    kind: ChipKind::Param,
                    op: "param.set".into(),
                    code: None,
                    seed: None,
                    slider: Some(Slider {
                        name: name.clone(),
                        value,
                        min,
                        max,
                        step: p["step"].as_f64(),
                    }),
                });
            }
        }
    }

    // The most recent explicit curves that are not themselves derived helper lines.
    let curves: Vec<&Value> = objects
        .iter()
        .rev()
        .filter(|o| o["kind"] == "explicit" && o["spec"].get("tangent_of").is_none())
        .take(MAX_CURVES)
        .collect();
    for curve in &curves {
        let (Some(id), Some(f), Some(var)) = (
            curve["id"].as_str(),
            curve["exprs"]["f"]["text"].as_str(),
            curve["exprs"]["var"]["text"].as_str(),
        ) else {
            continue;
        };
        let domain = &curve["spec"]["domain"];
        let (lo, hi) = (
            domain[0]["text"].as_str().unwrap_or("-10"),
            domain[1]["text"].as_str().unwrap_or("10"),
        );
        if ops.contains("plot.explicit") && ops.contains("diff") {
            chips.push(Chip {
                id: format!("derivative:{id}"),
                label: format!("Derivative of {id}"),
                kind: ChipKind::Direct,
                op: "diff".into(),
                code: Some(format!(
                    "from fibration_math import sym, plot\n\
                     plot.explicit(sym.define({f}).diff({var}), var={var}, domain=({lo}, {hi}), \
                     style={{\"dash\": \"dashed\"}})\n",
                    f = py_str(f),
                    var = py_str(var),
                    lo = py_str(lo),
                    hi = py_str(hi),
                )),
                seed: None,
                slider: None,
            });
        }
        if ops.contains("plot.tangent") {
            chips.push(Chip {
                id: format!("tangent:{id}"),
                label: format!("Tangent to {id}…"),
                kind: ChipKind::Seed,
                op: "plot.tangent".into(),
                code: None,
                seed: Some(format!("draw the tangent to {id} at {var} = ")),
                slider: None,
            });
        }
        if ops.contains("plot.area") {
            chips.push(Chip {
                id: format!("area:{id}"),
                label: format!("Area under {id}…"),
                kind: ChipKind::Seed,
                op: "plot.area".into(),
                code: None,
                seed: Some(format!("shade the area under {id} from {var} = ")),
                slider: None,
            });
        }
    }

    if !objects.is_empty() {
        if ops.contains("scene.highlight") && !curves.is_empty() {
            chips.push(Chip {
                id: "highlight:domain".into(),
                label: "Highlight domain…".into(),
                kind: ChipKind::Seed,
                op: "scene.highlight".into(),
                code: None,
                seed: Some("highlight the part of the curve where ".into()),
                slider: None,
            });
        }
        if ops.contains("cam.fit") {
            chips.push(Chip {
                id: "view:fit".into(),
                label: "Zoom to fit".into(),
                kind: ChipKind::Direct,
                op: "cam.fit".into(),
                code: Some("from fibration_math import cam\ncam.fit()\n".into()),
                seed: None,
                slider: None,
            });
        }
    }

    chips.truncate(MAX_CHIPS);
    chips
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn api(names: &[(&str, bool)]) -> Value {
        json!({"operations": names.iter().map(|(n, a)| json!({"name": n, "available": a})).collect::<Vec<_>>()})
    }

    #[test]
    fn an_empty_scene_offers_only_ways_to_start() {
        let chips = for_scene(
            &json!({"objects": [], "params": {}}),
            &api(&[
                ("plot.explicit", true),
                ("plot.implicit", true),
                ("param.set", false),
                ("plot.tangent", false),
                ("cam.fit", true),
            ]),
        );
        let ids: Vec<&str> = chips.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(ids, ["graph:new", "implicit:new"]);
        assert!(chips.iter().all(|c| c.kind == ChipKind::Seed));
    }

    #[test]
    fn chips_follow_masking_and_quote_expression_text() {
        let state = json!({
            "params": {"b": {"value": 1.0, "min": 0.0, "max": 2.0}, "a": {"value": 2.0, "min": -3.0, "max": 3.0, "step": 0.5}},
            "objects": [{
                "id": "curve", "kind": "explicit",
                "exprs": {"f": {"text": "a*sin(b*x)"}, "var": {"text": "x"}},
                "spec": {"domain": [{"text": "-10"}, {"text": "2*pi"}]}
            }]
        });
        let full = api(&[
            ("plot.explicit", true),
            ("diff", true),
            ("param.set", true),
            ("plot.tangent", true),
            ("plot.area", true),
            ("scene.highlight", true),
            ("cam.fit", true),
        ]);
        let chips = for_scene(&state, &full);
        let ids: Vec<&str> = chips.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "adjust:a",
                "adjust:b",
                "derivative:curve",
                "tangent:curve",
                "area:curve",
                "highlight:domain",
                "view:fit"
            ]
        );
        assert_eq!(chips[0].slider.as_ref().unwrap().step, Some(0.5));
        let code = chips[2].code.as_ref().unwrap();
        assert!(
            code.contains(r#"sym.define("a*sin(b*x)").diff("x")"#),
            "{code}"
        );
        assert!(code.contains(r#"domain=("-10", "2*pi")"#), "{code}");

        // With tangent masked off, its chip is gone; nothing else changes.
        let masked = api(&[
            ("plot.explicit", true),
            ("diff", true),
            ("param.set", true),
            ("plot.tangent", false),
            ("plot.area", true),
            ("scene.highlight", true),
            ("cam.fit", true),
        ]);
        assert!(!for_scene(&state, &masked)
            .iter()
            .any(|c| c.id == "tangent:curve"));
    }

    #[test]
    fn param_commit_cells_are_recognised_exactly() {
        let code = param_set_code("a", 2.5);
        assert_eq!(code, "from fibration_math import param\nparam.set(a=2.5)\n");
        assert!(is_param_commit(&code, "a"));
        assert!(!is_param_commit(&code, "b"));
        assert!(!is_param_commit(
            "from fibration_math import param\nparam.set(a=1, b=2)\n",
            "a"
        ));
        assert!(!is_param_commit(&format!("{code}print(1)\n"), "a"));
    }
}
