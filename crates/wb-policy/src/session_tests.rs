// Moved from Personal Analyst `crates/core/src/policy.rs` (its test module as it stood after
// the security review): the regression tests for REVIEW G2, G4, G5, G6 and G7 and the
// "runs never get network" test, now run against the shared engine.

use crate::config::{analyst_class as class, AnalystEdits, AnalystPolicyConfig};
use crate::paths::PathRules;
use crate::session::{ScopeChange, Session, Unsandboxed};
use crate::{Action, Decision, Level, ModeRules, Reason, CLASS_EDIT, CLASS_READ};
use std::path::Path;

/// The analysis workbench's modes (plan, build, run) as data.
fn mode(name: &str) -> ModeRules {
    match name {
        "build" => ModeRules::unrestricted("build"),
        "plan" => ModeRules::with_classes("plan", [CLASS_READ, class::META]),
        "run" => ModeRules::with_classes("run", [CLASS_READ, class::META])
            .allow_tool("run.script")
            .allow_tool("exec.snippet"),
        other => panic!("no mode {other}"),
    }
}

/// The analysis workbench's tool-to-action mapping, reduced to what these tests use.
fn action(tool: &str, path: Option<&str>) -> Action {
    let a = match tool {
        "fs.read" => Action::read(tool),
        "fs.propose_edit" | "fs.propose_delete" => Action::new(tool, CLASS_EDIT),
        "fs.propose_create" => Action::new(tool, CLASS_EDIT).creates_new(true),
        "run.script" | "exec.snippet" => Action::new(tool, class::RUN).needs_sandbox(true),
        "shell.run" => Action::new(tool, class::SHELL).needs_sandbox(true),
        "pkg.add" => Action::new(tool, class::PACKAGE),
        "git.commit" => Action::new(tool, class::GIT),
        other => Action::new(other, "unknown"),
    };
    match path {
        Some(p) => a.targets([p]),
        None => a,
    }
}

fn session_with(cfg: AnalystPolicyConfig, mode_name: &str, repo: &Path) -> Session {
    Session::new(cfg.to_policy().unwrap(), mode(mode_name))
        .with_paths(PathRules::new(repo, [".analyst"]))
        .with_unsandboxed(cfg.unsandboxed_runs)
}

fn session(edits: AnalystEdits, mode_name: &str, repo: &Path) -> Session {
    let cfg = AnalystPolicyConfig {
        edits,
        protected: vec!["data/**".into()],
        ..Default::default()
    };
    let mut s = session_with(cfg, mode_name, repo);
    s.set_sandbox("bwrap");
    s
}

fn decide(s: &Session, tool: &str, path: Option<&str>) -> Decision {
    s.evaluate(&action(tool, path)).decision
}

#[test]
fn read_tools_allowed_everywhere_edits_follow_table() {
    let dir = tempfile::tempdir().unwrap();
    let s = session(AnalystEdits::Review, "build", dir.path());
    assert!(decide(&s, "fs.read", None).is_allow());
    assert!(decide(&s, "fs.propose_edit", Some("scripts/a.py")).is_ask());
    assert_eq!(
        decide(&s, "fs.propose_create", Some("data/x.csv")),
        Decision::Deny {
            reason: Reason::Locked
        }
    );
    assert!(decide(&s, "pkg.add", None).is_ask());
    assert!(decide(&s, "run.script", None).is_allow());
    assert!(s
        .evaluate(&action("run.script", None).requests_network(true))
        .decision
        .is_deny());
    // A tool the app cannot classify asks under build mode's wildcard and has no level.
    assert!(!decide(&s, "nope", None).is_allow());
}

/// No policy setting, sandbox state, or argument gives a run network access.
#[test]
fn a_run_cannot_be_given_network() {
    let dir = tempfile::tempdir().unwrap();
    // The most permissive table there is, plus a leftover `network = "allow"`.
    let cfg: AnalystPolicyConfig = toml::from_str(
        "edits = \"auto\"\nruns = \"auto\"\npackages = \"auto\"\nshell = \"auto\"\ngit = \"auto\"\nnetwork = \"allow\"\nunsandboxed_runs = \"auto\"\n",
    )
    .unwrap();
    assert_eq!(cfg.unsandboxed_runs, Unsandboxed::Auto);
    let mut s = session_with(cfg, "build", dir.path());
    for sandbox in ["bwrap", "sandbox-exec", "limits", "none"] {
        s.set_sandbox(sandbox);
        let ev = s.evaluate(&action("run.script", None).requests_network(true));
        assert_eq!(
            ev.decision,
            Decision::Deny {
                reason: Reason::Network
            },
            "{sandbox}"
        );
        assert!(ev.message.contains("never have network"), "{}", ev.message);
        assert!(decide(&s, "run.script", None).is_allow());
    }
    assert!(s.summary().contains("Network for runs: never"));
    assert!(!s.policy.classes.contains_key("network"));
}

#[test]
fn modes_restrict_tool_sets() {
    let dir = tempfile::tempdir().unwrap();
    let plan = session(AnalystEdits::Auto, "plan", dir.path());
    let mode_deny = Decision::Deny {
        reason: Reason::Mode,
    };
    assert_eq!(decide(&plan, "fs.propose_create", Some("a.py")), mode_deny);
    assert_eq!(decide(&plan, "run.script", None), mode_deny);
    let run = session(AnalystEdits::Auto, "run", dir.path());
    assert!(decide(&run, "run.script", None).is_allow());
    assert!(decide(&run, "exec.snippet", None).is_allow());
    assert_eq!(decide(&run, "pkg.add", None), mode_deny);
    assert_eq!(decide(&run, "fs.propose_edit", Some("a.py")), mode_deny);
    assert!(run.tool_available("run.script", class::RUN));
    assert!(!run.tool_available("shell.run", class::SHELL));
}

#[test]
fn auto_mode_respects_scope() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = session(AnalystEdits::Auto, "build", dir.path());
    assert!(decide(&s, "fs.propose_edit", Some("scripts/a.py")).is_ask());
    s.declare_scope(vec!["scripts/a.py".into()]);
    assert!(decide(&s, "fs.propose_edit", Some("scripts/a.py")).is_allow());
    // The same file under another spelling is the same file.
    assert!(decide(&s, "fs.propose_edit", Some("./scripts/a.py")).is_allow());
    let ev = s.evaluate(&action("fs.propose_edit", Some("scripts/b.py")));
    assert_eq!(
        ev.decision,
        Decision::Ask {
            reason: Reason::OutOfScope
        }
    );
    assert!(ev.message.contains("outside the declared scope"));
    let anf = session(AnalystEdits::AutoNewFiles, "build", dir.path());
    assert!(decide(&anf, "fs.propose_create", Some("scripts/b.py")).is_allow());
    assert!(decide(&anf, "fs.propose_edit", Some("scripts/b.py")).is_ask());
}

/// G6: protected content can no longer be rewritten by a sequence of edits, because the
/// engine never looks at the edits: what is protected is the path, whatever the proposal
/// contains. (Comment-marker locked regions inside a file, which this test used to cover,
/// are removed; protect the file instead.)
#[test]
fn no_edit_shape_gets_past_a_protected_path() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("analysis/core")).unwrap();
    std::fs::write(dir.path().join("analysis/core/k.py"), "SECRET = 1\n").unwrap();
    let cfg = AnalystPolicyConfig {
        edits: AnalystEdits::Auto,
        protected: vec!["analysis/core/**".into()],
        ..Default::default()
    };
    let mut s = session_with(cfg, "build", dir.path());
    s.set_sandbox("bwrap");
    s.declare_scope(vec!["analysis/core/k.py".into()]);
    for tool in ["fs.propose_edit", "fs.propose_delete", "fs.propose_create"] {
        for path in [
            "analysis/core/k.py",
            "./analysis/core/k.py",
            "analysis/./core/k.py",
        ] {
            let ev = s.evaluate(&action(tool, Some(path)));
            assert_eq!(
                ev.decision,
                Decision::Deny {
                    reason: Reason::Locked
                },
                "{tool} {path}"
            );
            assert!(ev.message.contains("protected"), "{}", ev.message);
        }
    }
    // An action naming several targets is denied when any one of them is protected.
    let multi = Action::new("fs.propose_edit", CLASS_EDIT).targets(["a.py", "analysis/core/k.py"]);
    assert!(s.evaluate(&multi).decision.is_deny());
    // A file that merely contains the old marker comments is an ordinary file now.
    std::fs::write(
        dir.path().join("plain.py"),
        "# analyst: lock\nX = 1\n# analyst: unlock\n",
    )
    .unwrap();
    s.declare_scope(vec!["plain.py".into()]);
    assert!(decide(&s, "fs.propose_edit", Some("plain.py")).is_allow());
}

/// G2: without a confining sandbox, run- and shell-class tools ask.
#[test]
fn unsandboxed_runs_ask() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = AnalystPolicyConfig {
        shell: Level::Auto,
        ..Default::default()
    };
    let mut s = session_with(cfg, "build", dir.path());
    for tool in ["run.script", "exec.snippet", "shell.run"] {
        let ev = s.evaluate(&action(tool, None));
        assert_eq!(
            ev.decision,
            Decision::Ask {
                reason: Reason::Unsandboxed
            },
            "{tool} before set_sandbox"
        );
        assert!(ev.message.contains("unsandboxed"), "{}", ev.message);
    }
    for kind in ["limits", "none", "something-new"] {
        s.set_sandbox(kind);
        let ev = s.evaluate(&action("exec.snippet", None));
        assert!(ev.decision.is_ask(), "{kind}");
        assert!(ev.message.contains("unsandboxed") && ev.message.contains(kind));
        assert!(s.summary().contains("unsandboxed"));
    }
    // A denial is not softened into a question.
    assert!(s
        .evaluate(&action("run.script", None).requests_network(true))
        .decision
        .is_deny());
    for kind in ["bwrap", "sandbox-exec"] {
        s.set_sandbox(kind);
        assert!(decide(&s, "run.script", None).is_allow());
        assert!(decide(&s, "shell.run", None).is_allow());
        assert!(!s.summary().contains("unsandboxed"));
    }
    s.set_sandbox("none");
    s.unsandboxed = Unsandboxed::Auto;
    assert!(decide(&s, "run.script", None).is_allow());
    // Other classes are unaffected.
    assert!(decide(&s, "pkg.add", None).is_ask());
    assert!(decide(&s, "fs.read", None).is_allow());
    // Under the read-only preset shell stays denied, sandbox or not.
    let ro = session_with(
        AnalystPolicyConfig::for_preset(crate::Preset::ReadOnly),
        "build",
        dir.path(),
    );
    assert!(decide(&ro, "shell.run", None).is_deny());
    assert!(decide(&ro, "run.script", None).is_ask(), "unsandboxed");
}

/// G4: reserved directories are denied under every edit policy, in any case.
#[test]
fn reserved_paths_are_never_editable() {
    let dir = tempfile::tempdir().unwrap();
    for edits in [
        AnalystEdits::Auto,
        AnalystEdits::AutoNewFiles,
        AnalystEdits::Review,
    ] {
        let mut s = session(edits, "build", dir.path());
        s.declare_scope(vec![
            ".git/config".into(),
            ".analyst/project.toml".into(),
            ".GIT/hooks/pre-commit".into(),
        ]);
        for (tool, path) in [
            ("fs.propose_edit", ".git/config"),
            ("fs.propose_create", ".GIT/hooks/pre-commit"),
            ("fs.propose_create", ".git/hooks/post-checkout"),
            ("fs.propose_edit", ".analyst/project.toml"),
            ("fs.propose_delete", ".Analyst/project.toml"),
            ("fs.propose_create", "sub/.git/config"),
            ("fs.propose_create", ".git./config"),
            ("fs.propose_create", "GIT~1/config"),
        ] {
            let ev = s.evaluate(&action(tool, Some(path)));
            assert!(
                ev.decision.is_deny(),
                "{tool} {path} under {edits:?}: {}",
                ev.message
            );
        }
        // Paths that leave the root are refused as well.
        for path in ["../x.py", "/etc/passwd", "~/x", ""] {
            assert_eq!(
                decide(&s, "fs.propose_create", Some(path)),
                Decision::Deny {
                    reason: Reason::Path
                },
                "{path}"
            );
        }
    }
    assert!(session(AnalystEdits::Review, "build", dir.path())
        .summary()
        .contains(".git/ and .analyst/ are never editable"));
}

/// G5: scope bookkeeping.
#[test]
fn scope_changes_are_classified_and_reset_per_turn() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = session(AnalystEdits::Auto, "build", dir.path());
    assert_eq!(s.scope_change(&["a.py".into()]), ScopeChange::First);
    s.declare_scope(vec!["a.py".into(), "./b.py".into()]);
    assert_eq!(
        s.scope(),
        Some(&["a.py".to_string(), "b.py".to_string()][..])
    );
    assert_eq!(s.scope_change(&["b.py".into()]), ScopeChange::Narrows);
    assert_eq!(
        s.scope_change(&["a.py".into(), "c.py".into()]),
        ScopeChange::Widens(vec!["c.py".into()])
    );
    assert!(s.in_scope("a.py") && !s.in_scope("c.py"));
    s.begin_turn();
    assert!(s.scope().is_none());
    assert_eq!(s.scope_change(&["z.py".into()]), ScopeChange::First);
    assert!(decide(&s, "fs.propose_edit", Some("a.py")).is_ask());
}

/// G7: path confinement follows symlinks.
#[cfg(unix)]
#[test]
fn edits_through_symlinks_are_denied() {
    let outside = tempfile::tempdir().unwrap();
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("data")).unwrap();
    std::fs::write(dir.path().join("data/x.csv"), "1").unwrap();
    std::os::unix::fs::symlink(outside.path(), dir.path().join("out_link")).unwrap();
    std::os::unix::fs::symlink(dir.path().join("data"), dir.path().join("alias")).unwrap();
    std::os::unix::fs::symlink(dir.path().join(".git"), dir.path().join("vcs")).unwrap();
    std::fs::create_dir_all(dir.path().join(".git")).unwrap();
    let mut s = session(AnalystEdits::Auto, "build", dir.path());
    s.declare_scope(vec![
        "out_link/evil.py".into(),
        "alias/x.csv".into(),
        "vcs/config".into(),
    ]);
    let ev = s.evaluate(&action("fs.propose_create", Some("out_link/evil.py")));
    assert_eq!(
        ev.decision,
        Decision::Deny {
            reason: Reason::Path
        }
    );
    assert!(ev.message.contains("outside the repository"));
    // `alias/x.csv` is really `data/x.csv`, which is protected.
    let ev = s.evaluate(&action("fs.propose_edit", Some("alias/x.csv")));
    assert_eq!(
        ev.decision,
        Decision::Deny {
            reason: Reason::Locked
        },
        "{}",
        ev.message
    );
    // `vcs/config` is really `.git/config`, which is reserved.
    assert!(decide(&s, "fs.propose_create", Some("vcs/config")).is_deny());
}
