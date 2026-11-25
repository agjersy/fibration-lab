//! Remote sync against a real OpenSSH server (PROTOCOL 7).
//!
//! The test starts `sshd` on a free localhost port with a throwaway host key, user key, and
//! configuration under a temporary directory, and reaches it through an `ssh` wrapper that
//! adds `-F <that configuration>`, so nothing in `~/.ssh` is read or written. It is skipped
//! when `sshd`, `ssh`, `ssh-keygen`, or `git` is missing or the server cannot be started;
//! set `FIBRATION_REQUIRE_SSHD=1` to make that a failure instead.

#![cfg(unix)]

use fibration_core::target::{self, sync, Target};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

struct Sshd(Child);

impl Drop for Sshd {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn find(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH").unwrap_or_default();
    std::env::split_paths(&path)
        .chain(["/usr/sbin", "/sbin", "/usr/local/sbin"].map(PathBuf::from))
        .map(|d| d.join(name))
        .find(|p| p.is_file())
}

fn run(cmd: &mut Command) -> bool {
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn git(dir: &Path, args: &[&str]) -> String {
    fibration_core::checkpoint::git(dir, args, None).unwrap()
}

fn write(path: PathBuf, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

/// Start sshd under `dir` and put an `ssh` wrapper for it first on `PATH`. Returns the
/// server and the host alias, or why it could not be done.
fn start_sshd(dir: &Path, host: &str) -> Result<Sshd, String> {
    let need = |name: &str| find(name).ok_or_else(|| format!("`{name}` is not installed"));
    let (sshd, ssh, keygen) = (need("sshd")?, need("ssh")?, need("ssh-keygen")?);
    need("git")?;
    for key in ["host_key", "user_key"] {
        let ok = run(Command::new(&keygen)
            .args(["-q", "-t", "ed25519", "-N", "", "-f"])
            .arg(dir.join(key)));
        if !ok {
            return Err("ssh-keygen failed".into());
        }
    }
    std::fs::copy(dir.join("user_key.pub"), dir.join("authorized_keys")).unwrap();
    // sshd started by root wants its privilege separation directory to exist.
    let _ = std::fs::create_dir_all("/run/sshd");
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .and_then(|l| l.local_addr())
        .map_err(|e| e.to_string())?
        .port();
    let d = dir.display();
    write(
        dir.join("sshd_config"),
        &format!(
            "Port {port}\nListenAddress 127.0.0.1\nHostKey {d}/host_key\nPidFile {d}/sshd.pid\n\
AuthorizedKeysFile {d}/authorized_keys\nStrictModes no\nUsePAM no\nPasswordAuthentication no\n\
PubkeyAuthentication yes\nPermitRootLogin yes\nPermitUserEnvironment no\n"
        ),
    );
    let user = String::from_utf8_lossy(&Command::new("id").arg("-un").output().unwrap().stdout)
        .trim()
        .to_string();
    write(
        dir.join("ssh_config"),
        &format!(
            "Host {host}\n  HostName 127.0.0.1\n  Port {port}\n  User {user}\n  IdentityFile {d}/user_key\n  \
IdentitiesOnly yes\n  UserKnownHostsFile {d}/known_hosts\n  StrictHostKeyChecking no\n  LogLevel ERROR\n"
        ),
    );
    let bin = dir.join("bin");
    let wrapper = bin.join("ssh");
    write(
        wrapper.clone(),
        &format!(
            "#!/bin/sh\nexec {} -F {d}/ssh_config \"$@\"\n",
            ssh.display()
        ),
    );
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).unwrap();
    let child = Command::new(&sshd)
        .args(["-D", "-e", "-f"])
        .arg(dir.join("sshd_config"))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("sshd did not start: {e}"))?;
    let server = Sshd(child);
    // This file is its own test binary with one test, so changing PATH races with nothing.
    let mut path = vec![bin];
    path.extend(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    ));
    std::env::set_var("PATH", std::env::join_paths(path).unwrap());
    for _ in 0..50 {
        if run(Command::new(&wrapper).args(["-o", "BatchMode=yes", "--", host, "true"])) {
            return Ok(server);
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    Err("could not log in to the test sshd".into())
}

#[tokio::test]
async fn sync_over_a_real_ssh_server() {
    let tmp = tempfile::tempdir().unwrap();
    let host = "fibration-sync-test-host";
    let _server = match start_sshd(tmp.path(), host) {
        Ok(s) => s,
        Err(why) => {
            if std::env::var_os("FIBRATION_REQUIRE_SSHD").is_some() {
                panic!("FIBRATION_REQUIRE_SSHD is set, but {why}");
            }
            eprintln!("skipping sync_over_a_real_ssh_server: {why}");
            return;
        }
    };

    // The local project: tracked, untracked, ignored, and a tracked `.env`.
    let repo = tmp.path().join("proj");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q"]);
    write(repo.join(".gitignore"), "out/\n.venv/\n");
    write(repo.join("scripts/a.py"), "print(1)\n");
    write(repo.join("scripts/gone.py"), "print('gone')\n");
    write(repo.join(".env"), "ANTHROPIC_API_KEY=sk-never-leaves\n");
    write(repo.join("out/local-only.txt"), "ignored\n");
    git(&repo, &["add", "-A"]);
    git(&repo, &["add", "-f", ".env"]);
    git(&repo, &["commit", "-q", "-m", "init"]);
    write(repo.join("untracked.md"), "hello\n");
    // Hooks and URL rewrites in the user's repository must not run or apply.
    let marker = tmp.path().join("hook-ran");
    let hook = format!("#!/bin/sh\ntouch {}\n", marker.display());
    for name in ["pre-push", "reference-transaction"] {
        let p = repo.join(".git/hooks").join(name);
        write(p.clone(), &hook);
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    git(
        &repo,
        &[
            "config",
            "url.ssh://elsewhere.invalid/.insteadOf",
            &format!("ssh://{host}/"),
        ],
    );
    let head = git(&repo, &["rev-parse", "HEAD"]);
    let refs_before = git(&repo, &["for-each-ref"]);
    let index_before = std::fs::read(repo.join(".git/index")).unwrap();

    // A remote root with a space exercises every quoting path, including the git URL.
    let remote_root = tmp.path().join("remote root");
    let ssh_target = |host: &str| Target::Ssh {
        name: "lab".into(),
        host: host.into(),
        remote_root: remote_root.display().to_string(),
        python: "3.12".into(),
        sandbox: "auto".into(),
        data_dirs: vec![],
        uv_binary: None,
    };
    let target = ssh_target(host);
    let mirror = PathBuf::from(target.mirror_dir(&repo).unwrap());
    let bare = PathBuf::from(sync::bare_repo(&mirror.display().to_string()));
    // A short directory of its own: a Unix socket path is limited to about 104 bytes, and
    // macOS's temporary directory alone is half of that. (The app keeps control sockets in
    // `~/.fibration/ssh`, which is short.)
    let control_dir = tempfile::Builder::new()
        .prefix("actl-")
        .tempdir_in("/tmp")
        .unwrap();
    let control = control_dir.path().to_path_buf();
    let push = |force: bool| sync::push(&repo, &target, Some(control.as_path()), None, force);

    // A host that is not a configured target is refused before anything connects.
    let err = format!("{:#}", push(false).await.unwrap_err());
    assert!(err.contains(host) && err.contains("config.toml"), "{err}");
    assert!(
        !remote_root.exists(),
        "nothing was created on the refused host"
    );
    wb_net::gate().allow_ssh_host(host).unwrap();
    let other = ssh_target("fibration-sync-other-host");
    let err = format!(
        "{:#}",
        sync::push(&repo, &other, None, None, false)
            .await
            .unwrap_err()
    );
    assert!(err.contains("fibration-sync-other-host"), "{err}");

    // First sync.
    let first = push(false).await.unwrap();
    assert_eq!(first.method, sync::Method::Git);
    assert_eq!(first.files, 4);
    assert_eq!(first.tree_hash, sync::tree_id(&repo).unwrap());
    assert_eq!(
        std::fs::read_to_string(mirror.join("scripts/a.py")).unwrap(),
        "print(1)\n"
    );
    assert!(mirror.join("scripts/gone.py").is_file());
    assert!(mirror.join("untracked.md").is_file());
    assert!(mirror.join(".gitignore").is_file());
    assert!(!mirror.join("out").exists(), "ignored files are not sent");
    assert!(!mirror.join(".git").exists());
    assert_eq!(
        git(&bare, &["for-each-ref", "--format=%(refname)"]).trim(),
        "refs/fibration/sync"
    );
    assert_eq!(
        git(&bare, &["rev-parse", "HEAD^{tree}"]).trim(),
        first.tree_hash,
        "the mirror is a detached checkout of the pushed tree"
    );
    assert_eq!(
        git(&bare, &["rev-list", "--count", "HEAD"]).trim(),
        "1",
        "no history is sent"
    );

    // `.env` never arrives: not as a file, not as an object.
    let env_blob = git(&repo, &["hash-object", ".env"]);
    let no_env = || {
        assert!(!mirror.join(".env").exists());
        let objects = git(&bare, &["cat-file", "--batch-all-objects", "--batch-check"]);
        assert!(
            !objects.contains(env_blob.trim()),
            "the .env blob reached the host"
        );
        assert!(
            !objects.contains(head.trim()),
            "the user's history reached the host"
        );
    };
    no_env();

    // What the host owns appears after the first sync and must survive later ones. Hooks
    // planted in the host's repository must not run.
    write(mirror.join("out/fit/r1/figures/fit.png"), "PNG");
    write(mirror.join("out/fit/r1/run.json"), "{}");
    write(mirror.join("out/fit/r1/raw.npy"), "DATA");
    write(mirror.join(".venv/pyvenv.cfg"), "venv");
    write(mirror.join(".fibration/env.sh"), "env");
    for name in [
        "pre-receive",
        "update",
        "post-receive",
        "post-checkout",
        "reference-transaction",
    ] {
        let p = bare.join("hooks").join(name);
        write(p.clone(), &hook);
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    // No-op resync: same tree id, nothing sent.
    let again = push(false).await.unwrap();
    assert_eq!(again.method, sync::Method::Skipped);
    assert_eq!(again.tree_hash, first.tree_hash);

    // A deleted file disappears from the mirror; an edit and a new file arrive.
    std::fs::remove_file(repo.join("scripts/gone.py")).unwrap();
    write(repo.join("scripts/a.py"), "print(2)\n");
    write(repo.join("analysis/new mod.py"), "x = 1\n");
    let third = push(false).await.unwrap();
    assert_eq!(third.method, sync::Method::Git);
    assert_ne!(third.tree_hash, first.tree_hash);
    assert!(!mirror.join("scripts/gone.py").exists());
    assert_eq!(
        std::fs::read_to_string(mirror.join("scripts/a.py")).unwrap(),
        "print(2)\n"
    );
    assert!(mirror.join("analysis/new mod.py").is_file());
    for kept in [
        "out/fit/r1/figures/fit.png",
        "out/fit/r1/raw.npy",
        ".venv/pyvenv.cfg",
        ".fibration/env.sh",
    ] {
        assert!(mirror.join(kept).is_file(), "{kept} must survive a sync");
    }
    no_env();
    assert_eq!(push(false).await.unwrap().method, sync::Method::Skipped);
    // A forced sync sends again and changes nothing.
    assert_eq!(push(true).await.unwrap().method, sync::Method::Git);
    assert!(!marker.exists(), "a git hook ran during sync");

    // The user's repository: same branch, same refs, same index.
    assert_eq!(git(&repo, &["rev-parse", "HEAD"]), head);
    assert_eq!(git(&repo, &["for-each-ref"]), refs_before);
    assert_eq!(
        std::fs::read(repo.join(".git/index")).unwrap(),
        index_before
    );

    // Rendered products come back; data stays on the host.
    target::sync::pull_rendered(
        host,
        &mirror.display().to_string(),
        &[
            "out/fit/r1/figures/fit.png".to_string(),
            "out/fit/r1/run.json".to_string(),
            "out/fit/r1/missing.txt".to_string(),
        ],
        &repo,
        Some(control.as_path()),
    )
    .await
    .unwrap();
    assert_eq!(
        std::fs::read_to_string(repo.join("out/fit/r1/figures/fit.png")).unwrap(),
        "PNG"
    );
    assert!(repo.join("out/fit/r1/run.json").is_file());
    assert!(!repo.join("out/fit/r1/raw.npy").exists());
    // Pulled files are ignored locally, so the tree, and with it the mirror, is unchanged.
    assert_eq!(push(false).await.unwrap().method, sync::Method::Skipped);

    // Every connection was recorded by the gate as SSH to this host; stop the shared
    // connection before the server goes away.
    let _ = Command::new("ssh")
        .args([
            "-o",
            &format!("ControlPath={}/%C", control.display()),
            "-O",
            "exit",
            "--",
            host,
        ])
        .stderr(Stdio::null())
        .status();
}
