use super::config::*;
use super::*;

fn edit(tool: &str, targets: &[&str]) -> Action {
    Action::edit(tool, targets.iter().copied())
}

const ASK_POLICY: Decision = Decision::Ask {
    reason: Reason::Policy,
};
const ASK_SCOPE: Decision = Decision::Ask {
    reason: Reason::OutOfScope,
};
const DENY_POLICY: Decision = Decision::Deny {
    reason: Reason::Policy,
};
const DENY_LOCKED: Decision = Decision::Deny {
    reason: Reason::Locked,
};
const DENY_MODE: Decision = Decision::Deny {
    reason: Reason::Mode,
};

#[test]
fn read_class_is_always_allowed() {
    let p = Policy::new()
        .with_class("read", Level::Ask)
        .with_locked(["s1"]);
    let a = Action::read("deck.read_slide")
        .targets(["s1"])
        .always_ask(true);
    // Read wins over the level table, locks, and always_ask.
    assert_eq!(p.evaluate(&a, &Scope::undeclared()), Decision::Allow);
}

#[test]
fn class_levels_and_unknown_class() {
    let p = Policy::new()
        .with_class("export", Level::Auto)
        .with_class("shell", Level::Deny)
        .with_class("code", Level::Ask);
    let s = Scope::undeclared();
    assert_eq!(
        p.evaluate(&Action::new("export.run", "export"), &s),
        Decision::Allow
    );
    assert_eq!(
        p.evaluate(&Action::new("shell.run", "shell"), &s),
        DENY_POLICY
    );
    assert_eq!(
        p.evaluate(&Action::new("deck.build", "code"), &s),
        ASK_POLICY
    );
    // Unknown class asks.
    assert_eq!(p.evaluate(&Action::new("x.y", "teleport"), &s), ASK_POLICY);
    assert_eq!(p.level("teleport"), Level::Ask);
    assert_eq!(p.level("read"), Level::Auto);
    let ev = p.explain(&Action::new("shell.run", "shell"), &s);
    assert_eq!(ev.message, "shell policy is deny");
}

#[test]
fn deny_level_wins_over_lock_and_always_ask_and_applies_to_edits() {
    let p = Policy::new()
        .with_class("edit", Level::Deny)
        .with_edits(EditGate::Auto)
        .with_locked(["s1"]);
    let a = edit("slide.propose_update", &["s1"]).always_ask(true);
    assert_eq!(p.evaluate(&a, &Scope::declared(["s1"])), DENY_POLICY);
}

#[test]
fn locked_targets_are_denied_by_exact_id_and_glob() {
    let p = Policy::new()
        .with_edits(EditGate::Auto)
        .with_class("code", Level::Auto)
        .with_locked([
            "s-003",
            "s-007/title",
            "data/**",
            "analysis/core/*.py",
            "weird[id",
        ]);
    let scope = Scope::declared(["s-003", "s-004", "data/x.csv"]);
    assert_eq!(
        p.evaluate(&edit("slide.propose_update", &["s-003"]), &scope),
        DENY_LOCKED
    );
    assert_eq!(
        p.evaluate(
            &edit("slide.propose_update", &["s-004", "s-007/title"]),
            &scope
        ),
        DENY_LOCKED
    );
    assert_eq!(
        p.evaluate(
            &edit("fs.propose_create", &["data/x.csv"]).creates_new(true),
            &scope
        ),
        DENY_LOCKED
    );
    assert_eq!(
        p.evaluate(&edit("fs.propose_edit", &["analysis/core/fit.py"]), &scope),
        DENY_LOCKED
    );
    // An entry that is not a valid glob still locks the exact id.
    assert!(p.is_locked("weird[id"));
    assert!(!p.is_locked("s-0030"));
    assert_eq!(
        p.evaluate(&edit("slide.propose_update", &["s-004"]), &scope),
        Decision::Allow
    );
    // Locks apply to non-edit classes that name targets, and win over always_ask.
    let a = Action::new("deck.build", "code")
        .targets(["s-003"])
        .always_ask(true);
    assert_eq!(p.evaluate(&a, &scope), DENY_LOCKED);
    assert_eq!(p.explain(&a, &scope).message, "`s-003` is locked");
}

#[test]
fn lock_set_strict_insert_remove_and_serde() {
    assert!(matches!(
        LockSet::strict(["a/[b"]),
        Err(PolicyError::BadPattern { .. })
    ));
    let mut l = LockSet::strict(["data/**"]).unwrap();
    assert!(l.is_locked("data/a/b.csv"));
    assert!(l.insert("s1"));
    assert!(!l.insert("s1"));
    assert!(l.is_locked("s1"));
    assert!(l.remove("data/**"));
    assert!(!l.is_locked("data/a/b.csv"));
    assert_eq!(l.len(), 1);
    let json = serde_json::to_string(&l).unwrap();
    assert_eq!(json, r#"["s1"]"#);
    let back: LockSet = serde_json::from_str(r#"["s?"]"#).unwrap();
    assert!(back.is_locked("s1") && back.is_locked("s2") && !back.is_locked("s10"));
}

#[test]
fn always_ask_overrides_auto_levels_and_gates() {
    let p = Policy::new()
        .with_edits(EditGate::Auto)
        .with_class("export", Level::Auto);
    let ask = Decision::Ask {
        reason: Reason::AlwaysAsk,
    };
    let a = edit("deck.switch_mode", &["deck"]).always_ask(true);
    assert_eq!(p.evaluate(&a, &Scope::declared(["deck"])), ask);
    let e = Action::new("export.run", "export").always_ask(true);
    assert_eq!(p.evaluate(&e, &Scope::undeclared()), ask);
}

#[test]
fn edit_gate_review_always_asks() {
    let p = Policy::new().with_edits(EditGate::Review);
    let scope = Scope::declared(["s1"]).with_new(true);
    assert_eq!(
        p.evaluate(&edit("slide.propose_update", &["s1"]), &scope),
        ASK_POLICY
    );
    assert_eq!(
        p.evaluate(&edit("slide.propose_create", &[]).creates_new(true), &scope),
        ASK_POLICY
    );
}

#[test]
fn edit_gate_auto_new_allows_only_new_items() {
    let p = Policy::new().with_edits(EditGate::AutoNew);
    let s = Scope::undeclared();
    assert_eq!(
        p.evaluate(&edit("slide.propose_create", &[]).creates_new(true), &s),
        Decision::Allow
    );
    assert_eq!(
        p.evaluate(&edit("slide.propose_update", &["s1"]), &s),
        ASK_POLICY
    );
}

#[test]
fn edit_gate_auto_respects_scope() {
    let p = Policy::new().with_edits(EditGate::Auto);
    let a = edit("fs.propose_edit", &["scripts/a.py"]);
    // No scope declared.
    let ev = p.explain(&a, &Scope::undeclared());
    assert_eq!(ev.decision, ASK_SCOPE);
    assert!(ev.message.contains("no scope declared"));
    // In scope.
    let scope = Scope::declared(["scripts/a.py"]);
    assert_eq!(p.evaluate(&a, &scope), Decision::Allow);
    // Out of scope.
    let ev = p.explain(&edit("fs.propose_edit", &["scripts/b.py"]), &scope);
    assert_eq!(ev.decision, ASK_SCOPE);
    assert!(ev.message.contains("outside the declared scope"));
    // Every target must be in scope.
    assert_eq!(
        p.evaluate(&edit("slide.propose_move", &["scripts/a.py", "x"]), &scope),
        ASK_SCOPE
    );
    // An edit that names no target is not covered by any scope.
    assert_eq!(p.evaluate(&edit("outline.propose", &[]), &scope), ASK_SCOPE);
}

#[test]
fn edit_gate_auto_new_items_need_declaration() {
    let p = Policy::new().with_edits(EditGate::Auto);
    let create = edit("slide.propose_create", &[]).creates_new(true);
    // New items not declared.
    assert_eq!(p.evaluate(&create, &Scope::declared(["s1"])), ASK_SCOPE);
    // Declared: new items allowed.
    assert_eq!(
        p.evaluate(&create, &Scope::declared(["s1"]).with_new(true)),
        Decision::Allow
    );
    // allow_new without any declaration does not count as a declared scope.
    let undeclared = Scope {
        declared: None,
        allow_new: true,
    };
    assert_eq!(p.evaluate(&create, &undeclared), ASK_SCOPE);
    // A new item whose id (path) was declared is in scope (Analyst new files).
    let create_file = edit("fs.propose_create", &["scripts/new.py"]).creates_new(true);
    assert_eq!(
        p.evaluate(&create_file, &Scope::declared(["scripts/new.py"])),
        Decision::Allow
    );
    assert_eq!(
        p.evaluate(&create_file, &Scope::declared(["other.py"])),
        ASK_SCOPE
    );
    // allow_new does not cover changes to existing items.
    assert_eq!(
        p.evaluate(
            &edit("slide.propose_update", &["s9"]),
            &Scope::declared(["s1"]).with_new(true)
        ),
        ASK_SCOPE
    );
}

#[test]
fn scope_helpers() {
    let mut s = Scope::undeclared();
    assert!(!s.is_declared() && !s.contains("a"));
    s.declare(["a", "b"], true);
    assert!(s.is_declared() && s.contains("a") && s.allow_new);
    s.add("c");
    assert!(s.contains("c"));
    s.clear();
    assert_eq!(s, Scope::undeclared());
    s.add("x");
    assert!(s.is_declared() && s.contains("x"));
}

#[test]
fn denied_tools_are_denied_even_when_read() {
    let mut p = Policy::new();
    p.denied_tools.insert("refs.read".into());
    assert_eq!(
        p.evaluate(&Action::read("refs.read"), &Scope::undeclared()),
        DENY_POLICY
    );
}

fn analyst_modes() -> (ModeRules, ModeRules, ModeRules) {
    let plan = ModeRules::with_classes("plan", ["read", "meta"]);
    let build = ModeRules::unrestricted("build");
    let run = ModeRules::with_classes("run", ["read", "meta"])
        .allow_tool("run.script")
        .allow_tool("exec.snippet");
    (plan, build, run)
}

#[test]
fn mode_rules_restrict_tool_sets_like_analyst() {
    let (plan, build, run) = analyst_modes();
    let p = Policy::new()
        .with_edits(EditGate::Auto)
        .with_class("run", Level::Auto)
        .with_class("meta", Level::Auto);
    let s = Scope::declared(["a.py"]);
    let create = edit("fs.propose_create", &["a.py"]).creates_new(true);
    let run_script = Action::new("run.script", "run");
    let pkg = Action::new("pkg.add", "package");
    assert_eq!(p.evaluate_in_mode(&plan, &create, &s), DENY_MODE);
    assert_eq!(p.evaluate_in_mode(&plan, &run_script, &s), DENY_MODE);
    assert_eq!(
        p.evaluate_in_mode(&plan, &Action::read("fs.read"), &s),
        Decision::Allow
    );
    assert_eq!(
        p.evaluate_in_mode(&plan, &Action::new("pin_fact", "meta"), &s),
        Decision::Allow
    );
    assert_eq!(p.evaluate_in_mode(&run, &run_script, &s), Decision::Allow);
    assert_eq!(
        p.evaluate_in_mode(&run, &Action::new("exec.snippet", "run"), &s),
        Decision::Allow
    );
    assert_eq!(p.evaluate_in_mode(&run, &pkg, &s), DENY_MODE);
    assert_eq!(p.evaluate_in_mode(&run, &create, &s), DENY_MODE);
    assert_eq!(p.evaluate_in_mode(&build, &create, &s), Decision::Allow);
    assert_eq!(p.evaluate_in_mode(&build, &pkg, &s), ASK_POLICY);
    let ev = p.explain_in_mode(&plan, &create, &s);
    assert_eq!(
        ev.message,
        "tool `fs.propose_create` is not available in plan mode"
    );
    assert!(run.tool_available("run.script", "run"));
    assert!(!run.tool_available("pkg.add", "package"));
}

#[test]
fn mode_restrictions_deny_flags_like_present_polish() {
    let polish = ModeRules::with_classes("polish", ["read", "edit"])
        .deny_tool("slide.propose_delete")
        .restrict(FLAG_CREATES_NEW)
        .restrict("new-citation");
    let p = Policy::new().with_edits(EditGate::Auto);
    let s = Scope::declared(["s1"]).with_new(true);
    // A new slide.
    let create = edit("slide.propose_create", &[]).creates_new(true);
    let ev = p.explain_in_mode(&polish, &create, &s);
    assert_eq!(ev.decision, DENY_MODE);
    assert!(ev
        .message
        .contains("polish mode does not allow `creates_new`"));
    // A new citation on an existing slide.
    let cite = edit("slide.propose_update", &["s1"]).flag("new-citation");
    assert_eq!(p.evaluate_in_mode(&polish, &cite, &s), DENY_MODE);
    // Wording change: decided by the policy.
    let reword = edit("slide.propose_update", &["s1"]);
    assert_eq!(p.evaluate_in_mode(&polish, &reword, &s), Decision::Allow);
    // Denied tool although its class is offered; class not offered.
    assert_eq!(
        p.evaluate_in_mode(&polish, &edit("slide.propose_delete", &["s1"]), &s),
        DENY_MODE
    );
    assert_eq!(
        p.evaluate_in_mode(&polish, &Action::new("export.run", "export"), &s),
        DENY_MODE
    );
    // Mode is evaluated before the policy: a locked target still reports Mode here.
    let locked = p.clone().with_locked(["s1"]);
    assert_eq!(locked.evaluate_in_mode(&polish, &cite, &s), DENY_MODE);
    assert_eq!(locked.evaluate_in_mode(&polish, &reword, &s), DENY_LOCKED);
    // Without mode rules the same actions follow the policy only.
    assert_eq!(p.evaluate(&create, &s), Decision::Allow);
}

#[test]
fn session_overrides_layer_over_base() {
    let base = PresentPolicyConfig {
        locked_slides: vec!["s1".into(), "s2".into()],
        ..Default::default()
    }
    .to_policy();
    let patch = PolicyPatch {
        edits: Some(EditGate::Auto),
        max_steps: Some(10),
        classes: BTreeMap::from([("export".to_string(), Level::Ask)]),
        lock: vec!["s3".into()],
        unlock: vec!["s1".into()],
        deny_tools: vec!["export.run".into()],
        ..Default::default()
    };
    let p = base.with_overrides(&patch);
    assert_eq!(p.edits, EditGate::Auto);
    assert_eq!(p.max_steps, 10);
    assert_eq!(p.level("export"), Level::Ask);
    assert_eq!(p.level("code"), Level::Ask);
    assert!(!p.is_locked("s1") && p.is_locked("s2") && p.is_locked("s3"));
    assert!(p.denied_tools.contains("export.run"));
    // The base is untouched and an empty patch changes nothing.
    assert_eq!(base.edits, EditGate::Review);
    assert!(base.is_locked("s1"));
    assert!(PolicyPatch::default().is_empty());
    assert_eq!(base.with_overrides(&PolicyPatch::default()), base);
    // Replacing the locked set, then re-allowing a tool.
    let p2 = p.with_overrides(&PolicyPatch {
        locked: Some(vec!["only".into()]),
        allow_tools: vec!["export.run".into()],
        ..Default::default()
    });
    assert!(p2.is_locked("only") && !p2.is_locked("s2"));
    assert!(p2.denied_tools.is_empty());
}

#[test]
fn patches_merge_and_round_trip() {
    let a = PolicyPatch {
        edits: Some(EditGate::AutoNew),
        lock: vec!["s1".into()],
        deny_tools: vec!["t".into()],
        ..Default::default()
    };
    let b = PolicyPatch {
        edits: Some(EditGate::Auto),
        unlock: vec!["s1".into()],
        lock: vec!["s2".into()],
        allow_tools: vec!["t".into()],
        ..Default::default()
    };
    let m = a.merged(&b);
    assert_eq!(m.edits, Some(EditGate::Auto));
    assert_eq!(m.lock, vec!["s2"]);
    assert_eq!(m.unlock, vec!["s1"]);
    assert!(m.deny_tools.is_empty());
    let base = Policy::new().with_locked(["s1"]);
    assert_eq!(
        base.with_overrides(&a).with_overrides(&b),
        base.with_overrides(&m)
    );
    let json = serde_json::to_value(&m).unwrap();
    assert_eq!(json["edits"], "auto");
    assert_eq!(serde_json::from_value::<PolicyPatch>(json).unwrap(), m);
    // From the UI: a partial JSON object.
    let ui: PolicyPatch =
        serde_json::from_str(r#"{"edits":"auto-new-slides","classes":{"code":"auto"}}"#).unwrap();
    assert_eq!(ui.edits, Some(EditGate::AutoNew));
    assert_eq!(ui.classes["code"], Level::Auto);
}

#[test]
fn policy_and_decision_serde() {
    let p = Policy::new()
        .with_edits(EditGate::AutoNew)
        .with_class("export", Level::Auto)
        .with_locked(["s1", "data/**"]);
    let json = serde_json::to_string(&p).unwrap();
    assert_eq!(serde_json::from_str::<Policy>(&json).unwrap(), p);
    let toml_text = toml::to_string(&p).unwrap();
    let back: Policy = toml::from_str(&toml_text).unwrap();
    assert_eq!(back, p);
    assert!(back.is_locked("data/x"));
    // Both app spellings of the gate are accepted.
    for s in ["auto-new", "auto-new-slides", "auto-new-files"] {
        let g: EditGate = serde_json::from_value(serde_json::json!(s)).unwrap();
        assert_eq!(g, EditGate::AutoNew);
    }
    assert_eq!(serde_json::to_value(EditGate::AutoNew).unwrap(), "auto-new");
    assert_eq!(
        serde_json::to_value(Decision::Ask {
            reason: Reason::OutOfScope
        })
        .unwrap(),
        serde_json::json!({"decision": "ask", "reason": "out_of_scope"})
    );
    assert_eq!(
        serde_json::to_value(Decision::Allow).unwrap(),
        serde_json::json!({"decision": "allow"})
    );
    let a: Action = serde_json::from_str(r#"{"tool":"x","class":"edit"}"#).unwrap();
    assert_eq!(a, Action::new("x", "edit"));
    assert_eq!(Decision::Allow.reason(), None);
    assert!(DENY_MODE.is_deny() && ASK_POLICY.is_ask() && Decision::Allow.is_allow());
}

#[test]
fn present_policy_table_converts() {
    // The [policy] table of Present PROTOCOL 4.2, verbatim.
    let text = r#"
[project]
name = "Q3 review"
created = "2026-11-15T10:00:00Z"

[policy]
edits = "review"                      # review | auto-new-slides | auto
code = "ask"                          # builds and generated slides: ask | auto | deny
skills_scripts = "ask"                # ask | auto | deny
code_mode = "allow"                   # allow | deny (switching to code generation mode)
mcp = "ask"
export = "auto"
max_steps = 60
build_timeout_s = 60
"#;
    #[derive(serde::Deserialize)]
    struct Project {
        policy: PresentPolicyConfig,
    }
    let cfg = toml::from_str::<Project>(text).unwrap().policy;
    assert_eq!(cfg, PresentPolicyConfig::default());
    let p: Policy = (&cfg).into();
    let s = Scope::undeclared();
    assert_eq!(p.edits, EditGate::Review);
    assert_eq!(p.max_steps, 60);
    assert_eq!(
        p.evaluate(&Action::new("deck.build", "code"), &s),
        ASK_POLICY
    );
    assert_eq!(
        p.evaluate(&Action::new("skill.run_script", "skills_scripts"), &s),
        ASK_POLICY
    );
    assert_eq!(
        p.evaluate(&Action::new("export.run", "export"), &s),
        Decision::Allow
    );
    let switch = edit("deck.switch_mode", &[]).always_ask(true);
    assert_eq!(
        p.evaluate(&switch, &s),
        Decision::Ask {
            reason: Reason::AlwaysAsk
        }
    );
    assert_eq!(
        p.evaluate(&edit("slide.propose_update", &["s1"]), &s),
        ASK_POLICY
    );

    // Non-default values, the ARCHITECTURE 13 `locked_slides`, and a leftover `[mcp]` table
    // from an older file, which is ignored.
    let text = r#"
edits = "auto-new-slides"
code = "auto"
skills_scripts = "deny"
code_mode = "deny"
export = "ask"
locked_slides = ["s-002"]
max_steps = 12

[mcp]
default = "deny"
wolfram = "auto"
"#;
    let cfg: PresentPolicyConfig = toml::from_str(text).unwrap();
    assert_eq!(cfg.build_timeout_s, 60);
    let p = cfg.to_policy();
    assert_eq!(p.edits, EditGate::AutoNew);
    assert_eq!(p.max_steps, 12);
    assert_eq!(
        p.evaluate(&Action::new("deck.build", "code"), &s),
        Decision::Allow
    );
    assert_eq!(
        p.evaluate(&Action::new("skill.run_script", "skills_scripts"), &s),
        DENY_POLICY
    );
    assert_eq!(
        p.evaluate(&Action::new("export.run", "export"), &s),
        ASK_POLICY
    );
    assert_eq!(p.evaluate(&switch, &s), DENY_POLICY);
    assert_eq!(p.level("code_mode"), Level::Deny);
    assert_eq!(
        p.evaluate(&edit("slide.propose_update", &["s-002"]), &s),
        DENY_LOCKED
    );
    assert!(!p.classes.contains_key("mcp"));
    assert!(!serde_json::to_string(&p).unwrap().contains("mcp"));
    // The table round-trips.
    let back: PresentPolicyConfig = toml::from_str(&toml::to_string(&cfg).unwrap()).unwrap();
    assert_eq!(back, cfg);
    // An empty table is all defaults; a bad value is an error.
    assert_eq!(
        toml::from_str::<PresentPolicyConfig>("").unwrap(),
        PresentPolicyConfig::default()
    );
    assert!(toml::from_str::<PresentPolicyConfig>("edits = \"yolo\"").is_err());
    // The `mcp` key of older files (above) is ignored: there is no such class any more.
    assert_eq!(p.level("mcp"), Level::Ask);
    assert!(!toml::to_string(&cfg).unwrap().contains("mcp"));
}

#[test]
fn analyst_policy_table_converts() {
    // The [policy] table of Analyst ARCHITECTURE 5.8, verbatim.
    let text = r#"
[policy]
edits    = "review"        # review | auto-new-files | auto
runs     = "auto"          # auto | ask
packages = "ask"           # ask | auto | deny
shell    = "ask"           # ask | auto | deny
network  = "deny"          # deny | ask | allow
git      = "ask"           # commits by the agent
protected = ["analysis/core/**", "data/**"]   # never editable by the agent
max_steps = 40
max_run_seconds = 600

[policy.mcp]
wolfram = "ask"
"#;
    #[derive(serde::Deserialize)]
    struct Project {
        policy: AnalystPolicyConfig,
    }
    let cfg = toml::from_str::<Project>(text).unwrap().policy;
    // `network`, `max_run_seconds` and `[policy.mcp]` are keys of older files: ignored.
    assert_eq!(cfg.preset, None);
    let p = cfg.to_policy().unwrap();
    let s = Scope::undeclared();
    assert_eq!(p.max_steps, 40);
    // The table of Analyst's `read_tools_allowed_everywhere_edits_follow_table` test.
    assert_eq!(p.evaluate(&Action::read("fs.read"), &s), Decision::Allow);
    assert_eq!(
        p.evaluate(&edit("fs.propose_edit", &["scripts/a.py"]), &s),
        ASK_POLICY
    );
    assert_eq!(
        p.evaluate(
            &edit("fs.propose_create", &["data/x.csv"]).creates_new(true),
            &s
        ),
        DENY_LOCKED
    );
    assert_eq!(
        p.evaluate(&Action::new("pkg.add", "package"), &s),
        ASK_POLICY
    );
    assert_eq!(
        p.evaluate(&Action::new("shell.run", "shell"), &s),
        ASK_POLICY
    );
    assert_eq!(
        p.evaluate(&Action::new("git.commit", "git"), &s),
        ASK_POLICY
    );
    assert_eq!(
        p.evaluate(&Action::new("run.script", "run"), &s),
        Decision::Allow
    );
    assert_eq!(
        p.evaluate(&Action::new("declare_scope", "meta"), &s),
        Decision::Allow
    );
    assert!(!p.classes.contains_key("network") && !p.classes.contains_key("mcp"));

    let cfg: AnalystPolicyConfig = toml::from_str(
        "edits = \"auto-new-files\"\nruns = \"ask\"\nnetwork = \"allow\"\nshell = \"deny\"\n",
    )
    .unwrap();
    let p = Policy::try_from(&cfg).unwrap();
    assert_eq!(p.edits, EditGate::AutoNew);
    assert_eq!(
        p.evaluate(&Action::new("run.script", "run"), &s),
        ASK_POLICY
    );
    assert!(
        !p.classes.contains_key("network"),
        "a leftover `network = \"allow\"` grants nothing"
    );
    assert_eq!(
        p.evaluate(&Action::new("shell.run", "shell"), &s),
        DENY_POLICY
    );
    assert_eq!(
        p.evaluate(&edit("fs.propose_create", &["b.py"]).creates_new(true), &s),
        Decision::Allow
    );
    // Defaults match Analyst's PolicyConfig::default().
    let d = AnalystPolicyConfig::default();
    assert_eq!(
        (d.max_steps, d.packages, d.git),
        (40, Level::Ask, Level::Ask)
    );
    // A bad protected glob is an error, as in Analyst.
    let bad = AnalystPolicyConfig {
        protected: vec!["a/[".into()],
        ..Default::default()
    };
    assert!(bad.to_policy().is_err());
}

#[test]
fn summary_mentions_what_needs_approval() {
    let p = PresentPolicyConfig {
        locked_slides: vec!["s-002".into()],
        ..Default::default()
    }
    .to_policy();
    let build = ModeRules::unrestricted("build");
    let s = p.summary(Some(&build), &Scope::undeclared());
    assert!(s.starts_with("Mode: build. Edits: every edit pauses for review."));
    assert!(s.contains("export: auto."));
    assert!(s.contains("skills_scripts: ask."));
    assert!(s.contains("Max steps per turn: 60."));
    assert!(s.contains("Locked (never editable): s-002."));
    assert!(s.contains("No scope declared yet"));
    let s = p.summary(Some(&build), &Scope::declared(["s-001"]).with_new(true));
    assert!(s.contains("Declared scope: s-001, new items."));
    let outline = ModeRules::with_classes("outline", ["read"]).restrict("creates_new");
    let s = p.summary(Some(&outline), &Scope::undeclared());
    assert!(!s.contains("No scope declared"));
    assert!(s.contains("Not allowed in this mode: creates_new."));
    assert!(p.summary(None, &Scope::undeclared()).starts_with("Edits:"));
}

#[test]
fn normalize_paths() {
    assert_eq!(normalize_rel_path("./a/b.py").unwrap(), "a/b.py");
    assert!(normalize_rel_path("../x").is_err());
    assert!(normalize_rel_path("/etc/passwd").is_err());
    assert!(normalize_rel_path("a/../../b").is_err());
    assert!(normalize_rel_path("~/x").is_err());
    assert!(normalize_rel_path("").is_err());
    assert_eq!(normalize_rel_path("a/./b").unwrap(), "a/b");
}

#[test]
fn presets_expand_and_detailed_keys_override() {
    use crate::preset::{Preset, PresetKind};
    // The three presets, kind by kind.
    let f = Preset::Review.fields();
    assert_eq!(
        (f.edits, f.runs, f.packages, f.shell, f.git),
        (
            EditGate::Review,
            Level::Auto,
            Level::Ask,
            Level::Ask,
            Level::Ask
        )
    );
    let f = Preset::Auto.fields();
    assert_eq!(
        (f.edits, f.runs, f.packages, f.shell, f.git),
        (
            EditGate::Auto,
            Level::Auto,
            Level::Ask,
            Level::Ask,
            Level::Ask
        )
    );
    let f = Preset::ReadOnly.fields();
    assert_eq!(
        (f.edits, f.runs, f.packages, f.shell, f.git),
        (
            EditGate::Deny,
            Level::Auto,
            Level::Deny,
            Level::Deny,
            Level::Deny
        )
    );
    for p in Preset::ALL {
        assert_eq!(Preset::matching(&p.fields()), Some(p));
        assert_eq!(
            serde_json::from_value::<Preset>(serde_json::json!(p.as_str())).unwrap(),
            p
        );
    }
    assert_eq!(Preset::ReadOnly.level(PresetKind::Runs), Level::Auto);

    // Analysis table: the preset expands, a detailed key in the same table overrides it.
    let s = Scope::undeclared();
    let ro: AnalystPolicyConfig = toml::from_str("preset = \"read-only\"").unwrap();
    assert_eq!(ro, AnalystPolicyConfig::for_preset(Preset::ReadOnly));
    assert_eq!(ro.effective_preset(), Some(Preset::ReadOnly));
    let p = ro.to_policy().unwrap();
    assert_eq!(
        p.evaluate(&edit("fs.propose_edit", &["a.py"]), &s),
        DENY_POLICY
    );
    assert_eq!(
        p.evaluate(&edit("fs.propose_create", &["b.py"]).creates_new(true), &s),
        DENY_POLICY
    );
    // Deny is not softened by scope or by always-ask.
    assert_eq!(
        p.evaluate(
            &edit("fs.propose_edit", &["a.py"]).always_ask(true),
            &Scope::declared(["a.py"])
        ),
        DENY_POLICY
    );
    for (tool, class) in [
        ("pkg.add", "package"),
        ("shell.run", "shell"),
        ("git.commit", "git"),
    ] {
        assert_eq!(p.evaluate(&Action::new(tool, class), &s), DENY_POLICY);
    }
    assert_eq!(
        p.evaluate(&Action::new("run.script", "run"), &s),
        Decision::Allow
    );
    assert_eq!(p.evaluate(&Action::read("fs.read"), &s), Decision::Allow);
    assert!(p.summary(None, &s).contains("denied (read-only)"));
    assert!(!p.summary(None, &s).contains("No scope declared"));

    let over: AnalystPolicyConfig =
        toml::from_str("preset = \"read-only\"\ngit = \"ask\"\nprotected = [\"data/**\"]\n")
            .unwrap();
    assert_eq!(over.preset, Some(Preset::ReadOnly));
    assert_eq!(over.git, Level::Ask);
    assert_eq!(over.shell, Level::Deny);
    assert_eq!(over.edits, AnalystEdits::Deny);
    assert_eq!(over.effective_preset(), None, "no longer one of the three");
    // The written table reads back the same.
    let back: AnalystPolicyConfig = toml::from_str(&toml::to_string(&over).unwrap()).unwrap();
    assert_eq!(back, over);

    let auto: AnalystPolicyConfig = toml::from_str("preset = \"auto\"").unwrap();
    let p = auto.to_policy().unwrap();
    assert_eq!(
        p.evaluate(
            &edit("fs.propose_edit", &["a.py"]),
            &Scope::declared(["a.py"])
        ),
        Decision::Allow
    );
    assert_eq!(
        p.evaluate(
            &edit("fs.propose_edit", &["b.py"]),
            &Scope::declared(["a.py"])
        ),
        ASK_SCOPE
    );
    // No preset: the table defaults, which are the review preset.
    let d: AnalystPolicyConfig = toml::from_str("").unwrap();
    assert_eq!(d.preset, None);
    assert_eq!(d.effective_preset(), Some(Preset::Review));
    let mut c = AnalystPolicyConfig {
        protected: vec!["data/**".into()],
        max_steps: 7,
        ..Default::default()
    };
    c.apply_preset(Preset::ReadOnly);
    assert_eq!(
        (c.edits, c.max_steps, c.protected.len()),
        (AnalystEdits::Deny, 7, 1)
    );
    assert!(toml::from_str::<AnalystPolicyConfig>("preset = \"yolo\"").is_err());
}

/// The same presets gate deck actions: slide and element edits are edits, deck builds and
/// skill scripts are runs.
#[test]
fn presets_gate_deck_actions() {
    use crate::preset::Preset;
    let s = Scope::undeclared();
    let build = Action::new("deck.build", "code");
    let script = Action::new("skill.run_script", "skills_scripts");
    let slide = edit("slide.propose_update", &["s-001"]);
    let element = edit("element.propose_update", &["s-001/e-2"]);

    let review: PresentPolicyConfig = toml::from_str("preset = \"review\"").unwrap();
    let p = review.to_policy();
    assert_eq!(p.evaluate(&slide, &s), ASK_POLICY);
    assert_eq!(p.evaluate(&build, &s), Decision::Allow);
    assert_eq!(p.evaluate(&script, &s), Decision::Allow);

    let auto: PresentPolicyConfig = toml::from_str("preset = \"auto\"").unwrap();
    let p = auto.to_policy();
    assert_eq!(
        p.evaluate(&slide, &Scope::declared(["s-001"])),
        Decision::Allow
    );
    assert_eq!(p.evaluate(&element, &Scope::declared(["s-001"])), ASK_SCOPE);
    assert_eq!(p.evaluate(&build, &s), Decision::Allow);

    let ro: PresentPolicyConfig =
        toml::from_str("preset = \"read-only\"\nlocked_slides = [\"s-009\"]").unwrap();
    let p = ro.to_policy();
    assert_eq!(p.evaluate(&slide, &s), DENY_POLICY);
    assert_eq!(p.evaluate(&element, &s), DENY_POLICY);
    assert_eq!(
        p.evaluate(&edit("slide.propose_create", &[]).creates_new(true), &s),
        DENY_POLICY
    );
    assert_eq!(p.evaluate(&build, &s), Decision::Allow);
    assert_eq!(p.evaluate(&script, &s), Decision::Allow);
    assert_eq!(
        p.evaluate(&edit("deck.switch_mode", &[]).always_ask(true), &s),
        DENY_POLICY
    );
    assert_eq!(
        p.evaluate(&Action::new("export.run", "export"), &s),
        Decision::Allow
    );
    assert!(p.is_locked("s-009"));

    // Detailed keys still override; without a preset the deck defaults are unchanged.
    let over: PresentPolicyConfig = toml::from_str("preset = \"review\"\ncode = \"ask\"").unwrap();
    assert_eq!((over.code, over.skills_scripts), (Level::Ask, Level::Auto));
    assert_eq!(
        toml::from_str::<PresentPolicyConfig>("").unwrap().code,
        Level::Ask
    );
    let back: PresentPolicyConfig = toml::from_str(&toml::to_string(&over).unwrap()).unwrap();
    assert_eq!(back, over);
    // The generic form, for any class table.
    let p = Policy::new().with_preset(Preset::ReadOnly, present_class::PRESET_CLASSES);
    assert_eq!(p.edits, EditGate::Deny);
    assert_eq!(p.level("code"), Level::Auto);
}
