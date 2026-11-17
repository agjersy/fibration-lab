use super::*;
use std::fs;

fn write_skill(root: &Path, folder: &str, front: &str, body: &str) -> PathBuf {
    let dir = root.join(folder);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join(SKILL_FILE), format!("---\n{front}\n---\n{body}")).unwrap();
    dir
}

fn simple(root: &Path, name: &str, description: &str) -> PathBuf {
    write_skill(
        root,
        name,
        &format!("name: {name}\ndescription: {description}"),
        &format!("# {name}\n\nBody of {name}.\n"),
    )
}

fn kinds(diags: &[SkillDiagnostic]) -> Vec<DiagnosticKind> {
    diags.iter().map(|d| d.kind).collect()
}

#[test]
fn discovers_skills_with_scripts_and_resources() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("skills");
    let dir = write_skill(
        &root,
        "chart-selection",
        "name: chart-selection\ndescription: Pick the right chart for a message.\nlicense: Apache-2.0\nallowed-tools: [\"slide.render\"]",
        "# Chart selection\n\nUse a bar chart for comparisons.\n",
    );
    fs::create_dir_all(dir.join("scripts/lib")).unwrap();
    fs::create_dir_all(dir.join("examples")).unwrap();
    fs::create_dir_all(dir.join(".git")).unwrap();
    fs::write(dir.join("scripts/make_chart.py"), "print('x')\n").unwrap();
    fs::write(dir.join("scripts/a_first.py"), "").unwrap();
    fs::write(dir.join("scripts/run.sh"), "echo no\n").unwrap();
    fs::write(dir.join("scripts/lib/helper.py"), "").unwrap();
    fs::write(dir.join("examples/bar.md"), "bar\n").unwrap();
    fs::write(dir.join("guide.md"), "guide\n").unwrap();
    fs::write(dir.join(".git/config"), "").unwrap();
    fs::write(dir.join(".hidden"), "").unwrap();
    simple(&root, "storyline", "Write the storyline first.");
    // Not skills: a folder without SKILL.md and a loose file.
    fs::create_dir_all(root.join("notes")).unwrap();
    fs::write(root.join("README.md"), "x").unwrap();

    let (set, diags) = SkillSet::discover(&[
        SkillRoot::new(SkillSource::Builtin, tmp.path().join("does-not-exist")),
        SkillRoot::new(SkillSource::User, &root),
    ]);
    assert!(diags.is_empty(), "{diags:?}");
    assert_eq!(set.len(), 2);
    assert_eq!(set.names(), vec!["chart-selection", "storyline"]);

    let list = set.list();
    assert_eq!(
        list[0],
        SkillInfo {
            name: "chart-selection".into(),
            description: "Pick the right chart for a message.".into(),
            source: SkillSource::User,
            has_scripts: true,
            dir: fs::canonicalize(&dir).unwrap(),
        }
    );
    assert!(!list[1].has_scripts);

    let s = set.get("chart-selection").unwrap();
    assert_eq!(s.license.as_deref(), Some("Apache-2.0"));
    assert_eq!(s.allowed_tools, vec!["slide.render"]);
    // Only .py files directly under scripts/ are scripts; everything else is a resource.
    assert_eq!(
        s.scripts,
        vec!["scripts/a_first.py", "scripts/make_chart.py"]
    );
    assert_eq!(
        s.resources,
        vec![
            "examples/bar.md",
            "guide.md",
            "scripts/lib/helper.py",
            "scripts/run.sh"
        ]
    );
    assert!(!s.always && s.triggers.is_empty() && s.paths.is_empty());
    let json = serde_json::to_value(&list[0]).unwrap();
    assert_eq!(json["source"], "user");
}

#[test]
fn invalid_skills_are_skipped_with_diagnostics() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    simple(root, "good", "A good skill.");
    write_skill(root, "bad-name", "name: Bad_Name\ndescription: x", "b");
    write_skill(
        root,
        "too-long-name",
        &format!("name: {}\ndescription: x", "a".repeat(65)),
        "b",
    );
    write_skill(root, "no-name", "description: x", "b");
    write_skill(root, "no-description", "name: no-description", "b");
    write_skill(
        root,
        "blank-description",
        "name: blank-description\ndescription: \"  \"",
        "b",
    );
    write_skill(
        root,
        "long-description",
        &format!("name: long-description\ndescription: {}", "d".repeat(1025)),
        "b",
    );
    write_skill(
        root,
        "max-description",
        &format!("name: max-description\ndescription: {}", "é".repeat(1024)),
        "b",
    );
    write_skill(root, "bad-yaml", "name: [unclosed", "b");
    write_skill(root, "wrong-type", "name: 12\ndescription: [a, b]", "b");
    fs::create_dir_all(root.join("no-front-matter")).unwrap();
    fs::write(root.join("no-front-matter/SKILL.md"), "# Just markdown\n").unwrap();
    fs::create_dir_all(root.join("unterminated")).unwrap();
    fs::write(
        root.join("unterminated/SKILL.md"),
        "---\nname: unterminated\n",
    )
    .unwrap();
    fs::create_dir_all(root.join("not-utf8")).unwrap();
    fs::write(root.join("not-utf8/SKILL.md"), [0xff, 0xfe, 0x00]).unwrap();

    let (set, diags) = SkillSet::discover(&[SkillRoot::new(SkillSource::Project, root)]);
    // 64-character names and 1024-character descriptions are the limits, inclusive.
    assert_eq!(set.names(), vec!["good", "max-description"]);
    assert!(diags
        .iter()
        .all(|d| d.skipped && d.source == SkillSource::Project));
    let by_folder = |folder: &str| {
        diags
            .iter()
            .find(|d| d.path.file_name().unwrap() == folder)
            .unwrap_or_else(|| panic!("no diagnostic for {folder}"))
    };
    assert_eq!(by_folder("bad-name").kind, DiagnosticKind::InvalidName);
    assert_eq!(by_folder("bad-name").name.as_deref(), Some("Bad_Name"));
    assert!(by_folder("bad-name").message.contains("^[a-z0-9-]{1,64}$"));
    assert_eq!(by_folder("too-long-name").kind, DiagnosticKind::InvalidName);
    assert_eq!(by_folder("no-name").kind, DiagnosticKind::InvalidName);
    assert_eq!(
        by_folder("no-description").kind,
        DiagnosticKind::InvalidDescription
    );
    assert_eq!(
        by_folder("blank-description").kind,
        DiagnosticKind::InvalidDescription
    );
    assert_eq!(
        by_folder("long-description").kind,
        DiagnosticKind::InvalidDescription
    );
    assert!(by_folder("long-description").message.contains("1025"));
    assert_eq!(by_folder("bad-yaml").kind, DiagnosticKind::BadFrontMatter);
    assert_eq!(by_folder("wrong-type").kind, DiagnosticKind::BadFrontMatter);
    assert_eq!(
        by_folder("no-front-matter").kind,
        DiagnosticKind::BadFrontMatter
    );
    assert_eq!(
        by_folder("unterminated").kind,
        DiagnosticKind::BadFrontMatter
    );
    assert_eq!(by_folder("not-utf8").kind, DiagnosticKind::Unreadable);
    assert_eq!(diags.len(), 11);
    assert!(
        is_valid_name("a-1") && !is_valid_name("") && !is_valid_name("a b") && !is_valid_name("É")
    );
}

#[test]
fn later_roots_override_earlier_ones() {
    let tmp = tempfile::tempdir().unwrap();
    let (builtin, user, project) = (
        tmp.path().join("builtin"),
        tmp.path().join("user"),
        tmp.path().join("project"),
    );
    simple(&builtin, "storyline", "builtin storyline");
    simple(&builtin, "contract", "builtin contract");
    simple(&user, "storyline", "user storyline");
    simple(&user, "mine", "user only");
    simple(&project, "storyline", "project storyline");
    let (set, diags) = SkillSet::discover(&[
        SkillRoot::new(SkillSource::Builtin, &builtin),
        SkillRoot::new(SkillSource::User, &user),
        SkillRoot::new(SkillSource::Project, &project),
    ]);
    assert_eq!(set.names(), vec!["contract", "mine", "storyline"]);
    let s = set.get("storyline").unwrap();
    assert_eq!(
        (s.source, s.description.as_str()),
        (SkillSource::Project, "project storyline")
    );
    assert_eq!(set.get("contract").unwrap().source, SkillSource::Builtin);
    assert_eq!(
        set.load("storyline").unwrap(),
        "# storyline\n\nBody of storyline.\n"
    );
    // Two informational diagnostics, nothing skipped.
    assert_eq!(
        kinds(&diags),
        vec![DiagnosticKind::Overrides, DiagnosticKind::Overrides]
    );
    assert!(diags.iter().all(|d| !d.skipped));
    assert!(diags[0].message.contains("replaces the builtin skill"));
}

#[test]
fn managed_skills_cannot_be_overridden() {
    let tmp = tempfile::tempdir().unwrap();
    let (builtin, user, project, managed) = (
        tmp.path().join("builtin"),
        tmp.path().join("user"),
        tmp.path().join("project"),
        tmp.path().join("managed"),
    );
    simple(&builtin, "brand", "builtin brand");
    simple(&user, "brand", "user brand");
    simple(&project, "brand", "project brand");
    simple(&managed, "brand", "managed brand");
    simple(&project, "local", "project local");

    // Documented order: managed last. It wins like any later root.
    let (set, diags) = SkillSet::discover(&[
        SkillRoot::new(SkillSource::Builtin, &builtin),
        SkillRoot::new(SkillSource::User, &user),
        SkillRoot::new(SkillSource::Project, &project),
        SkillRoot::new(SkillSource::Managed, &managed),
    ]);
    assert_eq!(set.get("brand").unwrap().description, "managed brand");
    assert!(diags.iter().all(|d| d.kind == DiagnosticKind::Overrides));

    // Managed first: later roots cannot replace it, and say so.
    let (set, diags) = SkillSet::discover(&[
        SkillRoot::new(SkillSource::Managed, &managed),
        SkillRoot::new(SkillSource::User, &user),
        SkillRoot::new(SkillSource::Project, &project),
    ]);
    let s = set.get("brand").unwrap();
    assert_eq!(
        (s.source, s.description.as_str()),
        (SkillSource::Managed, "managed brand")
    );
    assert_eq!(set.names(), vec!["brand", "local"]);
    assert_eq!(
        kinds(&diags),
        vec![
            DiagnosticKind::ManagedOverride,
            DiagnosticKind::ManagedOverride
        ]
    );
    assert!(diags
        .iter()
        .all(|d| d.skipped && d.name.as_deref() == Some("brand")));
    assert_eq!(diags[0].source, SkillSource::User);

    // A second managed root may replace a managed skill.
    let managed2 = tmp.path().join("managed2");
    simple(&managed2, "brand", "managed brand v2");
    let (set, _) = SkillSet::discover(&[
        SkillRoot::new(SkillSource::Managed, &managed),
        SkillRoot::new(SkillSource::Managed, &managed2),
    ]);
    assert_eq!(set.get("brand").unwrap().description, "managed brand v2");
}

#[test]
fn duplicate_names_in_one_root_and_folder_name_mismatch() {
    let tmp = tempfile::tempdir().unwrap();
    write_skill(
        tmp.path(),
        "a-folder",
        "name: same\ndescription: first",
        "one",
    );
    write_skill(
        tmp.path(),
        "b-folder",
        "name: same\ndescription: second",
        "two",
    );
    let (set, diags) = SkillSet::discover(&[SkillRoot::new(SkillSource::User, tmp.path())]);
    assert_eq!(set.len(), 1);
    assert_eq!(set.get("same").unwrap().description, "first");
    assert_eq!(
        kinds(&diags),
        vec![
            DiagnosticKind::NameMismatch,
            DiagnosticKind::DuplicateInRoot
        ]
    );
    assert!(!diags[0].skipped && diags[1].skipped);
}

#[test]
fn load_returns_the_body_only_and_reads_fresh() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = simple(tmp.path(), "storyline", "Write the storyline first.");
    let (set, _) = SkillSet::discover(&[SkillRoot::new(SkillSource::User, tmp.path())]);
    let body = set.load("storyline").unwrap();
    assert_eq!(body, "# storyline\n\nBody of storyline.\n");
    assert!(!body.contains("description:"));
    assert!(matches!(set.load("nope"), Err(SkillError::UnknownSkill(n)) if n == "nope"));
    // Edited on disk after discovery: the next load sees it.
    fs::write(
        dir.join(SKILL_FILE),
        "---\nname: storyline\ndescription: d\n---\nNew body.\n",
    )
    .unwrap();
    assert_eq!(set.load("storyline").unwrap(), "New body.\n");
    fs::write(dir.join(SKILL_FILE), "broken").unwrap();
    assert!(matches!(
        set.load("storyline"),
        Err(SkillError::Invalid { .. })
    ));
}

fn resource_fixture() -> (tempfile::TempDir, SkillSet, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("skills");
    let dir = simple(&root, "brand", "Brand guide.");
    fs::create_dir_all(dir.join("assets")).unwrap();
    fs::create_dir_all(dir.join("scripts")).unwrap();
    fs::write(dir.join("guide.md"), "Use blue.\n").unwrap();
    fs::write(
        dir.join("assets/logo.PNG"),
        [0x89, b'P', b'N', b'G', 1, 2, 3],
    )
    .unwrap();
    fs::write(dir.join("assets/photo.jpeg"), [0xff, 0xd8, 0xff]).unwrap();
    fs::write(dir.join("assets/font.bin"), [0xff, 0x00, 0xfe]).unwrap();
    fs::write(dir.join("scripts/palette.py"), "print('blue')\n").unwrap();
    fs::write(dir.join("scripts/tool.sh"), "echo\n").unwrap();
    fs::write(dir.join("other.py"), "print('not a script')\n").unwrap();
    // Files outside the skill folder.
    fs::write(tmp.path().join("secret.txt"), "TOP SECRET").unwrap();
    simple(&root, "other", "Another skill.");
    fs::write(root.join("other/private.md"), "other's file").unwrap();
    let (set, diags) = SkillSet::discover(&[SkillRoot::new(SkillSource::User, &root)]);
    assert!(diags.is_empty(), "{diags:?}");
    (tmp, set, dir)
}

#[test]
fn read_resource_returns_text_and_images() {
    let (_tmp, set, _dir) = resource_fixture();
    assert_eq!(
        set.read_resource("brand", "guide.md").unwrap(),
        Resource::Text("Use blue.\n".into())
    );
    assert_eq!(
        set.read_resource("brand", "./assets//logo.PNG").unwrap(),
        Resource::Image {
            media_type: "image/png".into(),
            bytes: vec![0x89, b'P', b'N', b'G', 1, 2, 3]
        }
    );
    assert!(matches!(
        set.read_resource("brand", "assets\\photo.jpeg").unwrap(),
        Resource::Image { media_type, .. } if media_type == "image/jpeg"
    ));
    // Scripts and SKILL.md are readable as text.
    assert!(matches!(
        set.read_resource("brand", "scripts/palette.py").unwrap(),
        Resource::Text(_)
    ));
    assert!(matches!(
        set.read_resource("brand", "SKILL.md").unwrap(),
        Resource::Text(_)
    ));
    // Binary that is not an image, missing files, directories, unknown skills.
    assert!(matches!(
        set.read_resource("brand", "assets/font.bin"),
        Err(SkillError::NotText { .. })
    ));
    assert!(matches!(
        set.read_resource("brand", "missing.md"),
        Err(SkillError::NotFound { .. })
    ));
    assert!(matches!(
        set.read_resource("brand", "assets"),
        Err(SkillError::NotFound { .. })
    ));
    assert!(matches!(
        set.read_resource("nope", "guide.md"),
        Err(SkillError::UnknownSkill(_))
    ));
}

#[test]
fn read_resource_enforces_size_caps() {
    let (_tmp, set, dir) = resource_fixture();
    let set = set.with_limits(ResourceLimits {
        max_text_bytes: 10,
        max_image_bytes: 4,
    });
    assert_eq!(
        set.read_resource("brand", "guide.md").unwrap(),
        Resource::Text("Use blue.\n".into())
    );
    fs::write(dir.join("guide.md"), "Use blue!!\n").unwrap();
    assert_eq!(
        set.read_resource("brand", "guide.md").unwrap(),
        Resource::TooLarge {
            size: 11,
            limit: 10
        }
    );
    assert_eq!(
        set.read_resource("brand", "assets/logo.PNG").unwrap(),
        Resource::TooLarge { size: 7, limit: 4 }
    );
    assert!(matches!(
        set.read_resource("brand", "assets/photo.jpeg").unwrap(),
        Resource::Image { .. }
    ));
    let d = ResourceLimits::default();
    assert_eq!(
        (d.max_text_bytes, d.max_image_bytes),
        (256 * 1024, 5 * 1024 * 1024)
    );
}

#[test]
fn read_resource_rejects_path_escapes() {
    let (tmp, set, dir) = resource_fixture();
    let secret = tmp.path().join("secret.txt");
    let absolute = secret.to_string_lossy().to_string();
    for bad in [
        "../other/private.md",
        "../../secret.txt",
        "assets/../../../secret.txt",
        "assets/../guide.md",
        "..",
        "..\\..\\secret.txt",
        "/etc/passwd",
        "\\\\server\\share\\x",
        "C:\\Windows\\win.ini",
        "C:secret.txt",
        "~/secret.txt",
        "",
        "  ",
        ".",
        "guide.md\0.png",
        absolute.as_str(),
    ] {
        let r = set.read_resource("brand", bad);
        assert!(
            matches!(r, Err(SkillError::InvalidPath { .. })),
            "{bad:?} -> {r:?}"
        );
    }
    #[cfg(unix)]
    {
        // Symbolic links that leave the folder are refused after canonicalization; links
        // that stay inside are fine.
        std::os::unix::fs::symlink(&secret, dir.join("leak.txt")).unwrap();
        std::os::unix::fs::symlink(tmp.path(), dir.join("up")).unwrap();
        std::os::unix::fs::symlink(dir.join("guide.md"), dir.join("alias.md")).unwrap();
        for bad in ["leak.txt", "up/secret.txt", "up/skills/other/private.md"] {
            let r = set.read_resource("brand", bad);
            assert!(
                matches!(r, Err(SkillError::InvalidPath { reason, .. }) if reason.contains("outside")),
                "{bad:?} -> {r:?}"
            );
        }
        assert_eq!(
            set.read_resource("brand", "alias.md").unwrap(),
            Resource::Text("Use blue.\n".into())
        );
        // Links are not listed as resources at discovery.
        let (set2, _) =
            SkillSet::discover(&[SkillRoot::new(SkillSource::User, tmp.path().join("skills"))]);
        let listed = &set2.get("brand").unwrap().resources;
        assert!(!listed
            .iter()
            .any(|r| r.contains("leak") || r.starts_with("up")));
    }
    let _ = dir;
}

#[test]
fn script_paths_are_limited_to_scripts_py() {
    let (tmp, set, dir) = resource_fixture();
    let expected = fs::canonicalize(dir.join("scripts/palette.py")).unwrap();
    assert_eq!(set.script_path("brand", "palette.py").unwrap(), expected);
    assert_eq!(
        set.script_path("brand", "scripts/palette.py").unwrap(),
        expected
    );
    for bad in [
        "tool.sh",
        "scripts/tool.sh",
        "other.py/x",
        "assets/logo.PNG",
        "scripts/sub/x.py",
    ] {
        assert!(
            matches!(
                set.script_path("brand", bad),
                Err(SkillError::NotAScript { .. })
            ),
            "{bad}"
        );
    }
    // `other.py` exists at the top of the folder but is not under scripts/.
    assert!(matches!(
        set.script_path("brand", "other.py"),
        Err(SkillError::NotFound { .. })
    ));
    assert!(matches!(
        set.script_path("brand", "missing.py"),
        Err(SkillError::NotFound { .. })
    ));
    assert!(matches!(
        set.script_path("brand", "../x.py"),
        Err(SkillError::InvalidPath { .. })
    ));
    assert!(matches!(
        set.script_path("nope", "x.py"),
        Err(SkillError::UnknownSkill(_))
    ));
    #[cfg(unix)]
    {
        fs::write(tmp.path().join("evil.py"), "import os\n").unwrap();
        std::os::unix::fs::symlink(tmp.path().join("evil.py"), dir.join("scripts/evil.py"))
            .unwrap();
        std::os::unix::fs::symlink(dir.join("other.py"), dir.join("scripts/inner.py")).unwrap();
        assert!(matches!(
            set.script_path("brand", "evil.py"),
            Err(SkillError::InvalidPath { .. })
        ));
        assert!(matches!(
            set.script_path("brand", "inner.py"),
            Err(SkillError::NotAScript { .. })
        ));
    }
    let _ = tmp;
}

#[test]
fn system_prompt_block_has_names_and_descriptions_only() {
    let tmp = tempfile::tempdir().unwrap();
    write_skill(
        tmp.path(),
        "storyline",
        "name: storyline\ndescription: |\n  Write the storyline first.\n  Then the slides.",
        "SECRET BODY TEXT that must not be in the prompt.\n",
    );
    simple(tmp.path(), "charts", "Pick the right chart.");
    simple(tmp.path(), "disabled", "Switched off by the user.");
    let (set, _) = SkillSet::discover(&[SkillRoot::new(SkillSource::Builtin, tmp.path())]);
    let block = set.system_prompt_block(&["storyline", "charts", "unknown"]);
    let mut lines = block.lines();
    assert!(lines
        .next()
        .unwrap()
        .starts_with("SKILLS (name: description)."));
    assert_eq!(lines.next(), Some("- charts: Pick the right chart."));
    assert_eq!(
        lines.next(),
        Some("- storyline: Write the storyline first. Then the slides.")
    );
    assert_eq!(lines.next(), None);
    assert!(!block.contains("SECRET BODY"));
    assert!(!block.contains("disabled"));
    assert!(!block.contains(&tmp.path().display().to_string()));
    // All skills, owned names.
    assert_eq!(set.system_prompt_block(&set.names()).lines().count(), 4);
    assert_eq!(set.system_prompt_block::<&str>(&[]), "");
    assert_eq!(set.system_prompt_block(&["unknown"]), "");
    assert_eq!(SkillSet::new().system_prompt_block(&["storyline"]), "");
}

#[test]
fn legacy_trigger_and_path_matching() {
    let tmp = tempfile::tempdir().unwrap();
    // The example of Analyst ARCHITECTURE 5.16: no description.
    write_skill(
        tmp.path(),
        "curve-fitting",
        "name: curve-fitting\ntriggers: [\"fit\", \"regression\", \"least squares\"]\npaths: [\"analysis/fitting*.py\"]\nalways: false",
        "When fitting models: report parameter uncertainties from the covariance.\n",
    );
    write_skill(
        tmp.path(),
        "house-rules",
        "name: house-rules\nalways: true",
        "Always on.\n",
    );
    simple(
        tmp.path(),
        "modern",
        "An Agent Skill without legacy fields.",
    );
    let (set, diags) = SkillSet::discover(&[SkillRoot::new(SkillSource::User, tmp.path())]);
    assert!(diags.is_empty(), "{diags:?}");
    let fit = set.get("curve-fitting").unwrap();
    assert_eq!(fit.description, "");
    assert_eq!(fit.triggers.len(), 3);
    let none: [&str; 0] = [];

    assert!(fit.matches("Please FIT a line to this", &none));
    assert!(fit.matches("run a Least Squares solve", &none));
    assert!(fit.matches("regression, then plot", &none));
    // Word boundaries: "fit" does not match inside other words.
    assert!(!fit.matches("the profit and fitness numbers", &none));
    assert!(!fit.matches("plot the residuals", &none));
    // Path globs against files in the context.
    assert!(fit.matches("plot it", &["analysis/fitting_tools.py"]));
    assert!(fit.matches("plot it", &["analysis\\fitting.py".to_string()]));
    assert!(!fit.matches("plot it", &["analysis/io.py", "scripts/fit.py"]));

    let names = |v: Vec<&Skill>| v.into_iter().map(|s| s.name.clone()).collect::<Vec<_>>();
    assert_eq!(names(set.matching("plot it", &none)), vec!["house-rules"]);
    assert_eq!(
        names(set.matching("fit this", &none)),
        vec!["curve-fitting", "house-rules"]
    );
    // Skills without legacy fields never match; they are loaded on demand.
    assert!(!set
        .get("modern")
        .unwrap()
        .matches("modern agent skill", &["modern"]));
    // A legacy skill without a description is listed by name only.
    assert!(set
        .system_prompt_block(&["curve-fitting"])
        .ends_with("- curve-fitting\n"));
}
