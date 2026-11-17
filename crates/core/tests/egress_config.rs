//! The package index setting, end to end, in its own process (the configured index is
//! process-wide state, so it is not exercised from the unit tests that run in parallel).

use analyst_core::{config, egress};

#[test]
fn a_configured_index_is_the_only_one_installs_may_use() {
    let home = tempfile::tempdir().unwrap();
    wb_net::init(home.path());
    assert_eq!(egress::package_index(), wb_net::DEFAULT_PACKAGE_INDEX);
    std::fs::write(
        config::global_config_path(home.path()),
        "[packages]\nindex_url = \"https://mirror.corp.example/simple\"\n",
    )
    .unwrap();
    let global = config::load_global(home.path()).unwrap();
    assert_eq!(
        global.packages.index_url,
        "https://mirror.corp.example/simple"
    );
    assert_eq!(
        egress::package_index(),
        "https://mirror.corp.example/simple"
    );

    // Install lines name the mirror, and the gate lets exactly that through.
    let sync = egress::uv_install::<&str>("uv", &["sync"], &[]);
    assert_eq!(
        sync,
        [
            "uv",
            "sync",
            "--default-index",
            "https://mirror.corp.example/simple"
        ]
    );
    let guard = egress::guard(&sync).unwrap();
    assert!(guard.env.is_empty());
    assert_eq!(
        egress::remote_index_flag("uv sync (remote)").unwrap(),
        "--default-index 'https://mirror.corp.example/simple'"
    );
    // Public PyPI was never configured here, so an install from it is refused.
    let pypi: Vec<String> = [
        "uv",
        "sync",
        "--default-index",
        wb_net::DEFAULT_PACKAGE_INDEX,
    ]
    .map(String::from)
    .to_vec();
    let err = egress::guard(&pypi).unwrap_err().to_string();
    assert!(
        err.contains("pypi.org") && err.contains("index_url"),
        "{err}"
    );

    // Every decision is in the ledger: two allowed, one refused, no request content.
    let ledger = wb_net::ledger::read(&home.path().join("egress.jsonl")).unwrap();
    let rows: Vec<(bool, &str, &str)> = ledger
        .iter()
        .map(|e| (e.allowed, e.destination.as_str(), e.purpose.as_str()))
        .collect();
    assert_eq!(
        rows,
        [
            (true, "mirror.corp.example:443", "uv sync"),
            (true, "mirror.corp.example:443", "uv sync (remote)"),
            (false, "pypi.org:443", "uv sync"),
        ]
    );
}
