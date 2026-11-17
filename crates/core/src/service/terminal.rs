//! Terminals (ARCHITECTURE 5.15) on `portable-pty`. Local: `$SHELL` (fallback `/bin/sh`) in the
//! repo with the session venv on PATH and the figure-capture environment, so anything run from
//! the terminal produces figures. Remote: `ssh -t <host>` into the mirror with the same setup.
//! A reader thread forwards output as `terminal_data` (lossy UTF-8) and the exit as
//! `terminal_exit`, both on the hub's terminal path (bounded and coalescing, see `events`).
//! The shell never inherits provider API keys (U11), and its `PATH` is the widened tool path.

use super::error::{ServiceError, ServiceResult};
use super::events::EventHub;
use crate::target::Target;
use crate::util::{sh_quote, sh_quote_path};
use analyst_protocol::ui::TerminalId;
use portable_pty::{native_pty_system, ChildKiller, CommandBuilder, MasterPty, PtySize};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
/// The figure-capture environment exported into every terminal (PROTOCOL 1.4 and 1.6).
#[derive(Debug, Clone)]
pub struct CaptureEnv {
    /// Directory containing the `analyst_mpl` package (prepended to `PYTHONPATH`); `None` when
    /// it cannot be located locally.
    pub mpl_dir: Option<PathBuf>,
    /// `ANALYST_OUT_DIR` for this terminal.
    pub out_dir: PathBuf,
    /// `ANALYST_RUN_ID` for this terminal (`terminal-<stamp>`).
    pub run_id: String,
    /// The configured output directory, repo-relative (`out` by default).
    pub out_rel: String,
}

/// Environment variables that commonly hold provider API keys. They are removed from every
/// child process the app starts (terminal shells, kernels) together with the `api_key_env`
/// names of the configured providers.
pub const COMMON_KEY_VARS: &[&str] = &[
    "ANTHROPIC_API_KEY",
    "OPENAI_API_KEY",
    "OPENROUTER_API_KEY",
    "GEMINI_API_KEY",
    "GOOGLE_API_KEY",
    "MISTRAL_API_KEY",
    "GROQ_API_KEY",
    "DEEPSEEK_API_KEY",
];

/// Every variable to scrub for a global config: the common ones plus each provider's
/// `api_key_env`.
pub fn key_env_names(global: &analyst_protocol::config::GlobalConfig) -> Vec<String> {
    let mut v: Vec<String> = COMMON_KEY_VARS.iter().map(|s| s.to_string()).collect();
    for p in &global.providers {
        if let Some(n) = &p.api_key_env {
            if !n.is_empty() && !v.contains(n) {
                v.push(n.clone());
            }
        }
    }
    v
}

impl CaptureEnv {
    /// A fresh capture environment under `<repo>/<out_dir>/terminal/<stamp>`.
    pub fn new(repo: &Path, out_dir: &str, mpl_dir: Option<PathBuf>) -> CaptureEnv {
        let stamp = crate::util::stamped_id();
        let out_rel = crate::util::normalize_rel_path(out_dir).unwrap_or_else(|_| "out".into());
        CaptureEnv {
            mpl_dir,
            out_dir: repo.join(&out_rel).join("terminal").join(&stamp),
            run_id: format!("terminal-{stamp}"),
            out_rel,
        }
    }

    /// Environment variables for a local terminal.
    pub fn local_vars(&self, repo: &Path) -> Vec<(String, String)> {
        let mut vars = vec![
            ("MPLBACKEND".to_string(), "module://analyst_mpl".to_string()),
            (
                "ANALYST_OUT_DIR".to_string(),
                self.out_dir.to_string_lossy().to_string(),
            ),
            ("ANALYST_RUN_ID".to_string(), self.run_id.clone()),
            (
                "ANALYST_ROOT".to_string(),
                repo.to_string_lossy().to_string(),
            ),
            ("PYTHONUNBUFFERED".to_string(), "1".to_string()),
        ];
        let mut pythonpath: Vec<String> = Vec::new();
        if let Some(m) = &self.mpl_dir {
            pythonpath.push(m.to_string_lossy().to_string());
        }
        if let Ok(existing) = std::env::var("PYTHONPATH") {
            if !existing.is_empty() {
                pythonpath.push(existing);
            }
        }
        pythonpath.push(repo.to_string_lossy().to_string());
        vars.push(("PYTHONPATH".to_string(), pythonpath.join(":")));
        // Apps started from a launcher have a minimal PATH; use the widened tool path.
        let path = crate::util::tool_path_env().to_string_lossy().to_string();
        let venv = repo.join(".venv");
        if venv.is_dir() {
            vars.push((
                "VIRTUAL_ENV".to_string(),
                venv.to_string_lossy().to_string(),
            ));
            let bin = venv.join("bin").to_string_lossy().to_string();
            vars.push(("PATH".to_string(), format!("{bin}:{path}")));
        } else {
            vars.push(("PATH".to_string(), path));
        }
        vars
    }

    /// The remote shell command for `ssh -t`: enter the mirror, export the environment, exec a
    /// login shell.
    pub fn remote_command(&self, mirror: &str) -> String {
        let m = sh_quote_path(mirror);
        let stamp = self
            .out_dir
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        // The configured out_dir, quoted as one word; `$PWD` stays outside the quotes.
        let out_rel = sh_quote(&format!("{}/terminal/{stamp}", self.out_rel));
        format!(
            "cd {m} && export VIRTUAL_ENV=\"$PWD/.venv\" PATH=\"$PWD/.venv/bin:$HOME/.analyst/bin:$PATH\" \
MPLBACKEND=module://analyst_mpl ANALYST_ROOT=\"$PWD\" ANALYST_OUT_DIR=\"$PWD\"/{out_rel} \
ANALYST_RUN_ID={run_id} PYTHONUNBUFFERED=1 \
PYTHONPATH=\"$($HOME/.analyst/tooling/.venv/bin/python -c 'import analyst_mpl,os;print(os.path.dirname(os.path.dirname(analyst_mpl.__file__)))' 2>/dev/null):$PWD\"; \
exec \"${{SHELL:-/bin/sh}}\" -l",
            run_id = sh_quote(&self.run_id),
        )
    }
}

/// Locate the directory containing `analyst_mpl` for the local target: ask the tooling
/// interpreter, else look for `python/kernel/analyst_mpl` in a development checkout.
pub fn find_mpl_dir(tooling_python: &str) -> Option<PathBuf> {
    let out = std::process::Command::new(crate::util::resolve_tool(tooling_python))
        .args([
            "-c",
            "import analyst_mpl, os; print(os.path.dirname(os.path.dirname(analyst_mpl.__file__)))",
        ])
        .stdin(std::process::Stdio::null())
        .output()
        .ok()?;
    if out.status.success() {
        let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if !s.is_empty() && Path::new(&s).join("analyst_mpl").is_dir() {
            return Some(PathBuf::from(s));
        }
    }
    let mut starts = Vec::new();
    if let Ok(cwd) = std::env::current_dir() {
        starts.push(cwd);
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(p) = exe.parent() {
            starts.push(p.to_path_buf());
        }
    }
    for s in starts {
        for dir in s.ancestors() {
            let c = dir.join("python/kernel");
            if c.join("analyst_mpl/__init__.py").is_file() {
                return Some(c);
            }
        }
    }
    None
}

/// How to open a terminal.
#[derive(Debug, Clone)]
pub struct TerminalSpec {
    pub repo: PathBuf,
    pub target: Target,
    /// SSH control directory for multiplexing (remote targets).
    pub control_dir: Option<PathBuf>,
    pub capture: CaptureEnv,
    /// Shell for local terminals; `None` = `$SHELL` or `/bin/sh`.
    pub shell: Option<String>,
    /// Environment variables removed from the shell (provider API keys, see
    /// [`key_env_names`]).
    pub env_remove: Vec<String>,
    pub cols: u16,
    pub rows: u16,
}

/// One open terminal.
pub struct Terminal {
    pub id: TerminalId,
    pub session: String,
    master: Mutex<Box<dyn MasterPty + Send>>,
    writer: Mutex<Box<dyn Write + Send>>,
    killer: Mutex<Box<dyn ChildKiller + Send + Sync>>,
    pub out_dir: PathBuf,
    pub run_id: String,
}

impl Terminal {
    /// Spawn the pty and its reader thread. Output and exit go to the hub's terminal path.
    pub fn open(
        id: TerminalId,
        session: &str,
        spec: &TerminalSpec,
        events: EventHub,
    ) -> ServiceResult<Terminal> {
        let pty = native_pty_system();
        let pair = pty
            .openpty(PtySize {
                rows: spec.rows.max(1),
                cols: spec.cols.max(1),
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| ServiceError::internal(format!("opening pty: {e}")))?;
        let mut cmd = match &spec.target {
            Target::Local => {
                let shell = spec
                    .shell
                    .clone()
                    .or_else(|| std::env::var("SHELL").ok().filter(|s| !s.is_empty()))
                    .unwrap_or_else(|| "/bin/sh".to_string());
                let mut c = CommandBuilder::new(&shell);
                if shell.ends_with("bash") || shell.ends_with("zsh") || shell.ends_with("fish") {
                    c.arg("-l");
                }
                c.cwd(&spec.repo);
                for (k, v) in spec.capture.local_vars(&spec.repo) {
                    c.env(k, v);
                }
                c
            }
            Target::Ssh { host, .. } => {
                let mirror = spec.target.mirror_dir(&spec.repo).unwrap_or_default();
                let mut line =
                    crate::kernel::transport::ssh_prefix(host, spec.control_dir.as_deref());
                // `-t` forces a tty; BatchMode stays so a missing key fails fast.
                line.insert(1, "-t".to_string());
                line.push(spec.capture.remote_command(&mirror));
                crate::target::validate_host(host)
                    .map_err(|e| ServiceError::invalid(e.to_string()))?;
                let mut c = CommandBuilder::new(crate::util::resolve_tool(&line[0]));
                c.args(&line[1..]);
                c.cwd(&spec.repo);
                c
            }
        };
        for name in COMMON_KEY_VARS {
            cmd.env_remove(name);
        }
        for name in &spec.env_remove {
            cmd.env_remove(name);
        }
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");
        let child = pair
            .slave
            .spawn_command(cmd)
            .map_err(|e| ServiceError::internal(format!("spawning shell: {e}")))?;
        drop(pair.slave);
        let reader = pair
            .master
            .try_clone_reader()
            .map_err(|e| ServiceError::internal(format!("pty reader: {e}")))?;
        let writer = pair
            .master
            .take_writer()
            .map_err(|e| ServiceError::internal(format!("pty writer: {e}")))?;
        let killer = child.clone_killer();
        let tid = id.clone();
        std::thread::Builder::new()
            .name(format!("pty-{id}"))
            .spawn(move || reader_loop(tid, reader, child, events))
            .map_err(|e| ServiceError::internal(format!("pty thread: {e}")))?;
        Ok(Terminal {
            id,
            session: session.to_string(),
            master: Mutex::new(pair.master),
            writer: Mutex::new(writer),
            killer: Mutex::new(killer),
            out_dir: spec.capture.out_dir.clone(),
            run_id: spec.capture.run_id.clone(),
        })
    }

    pub fn write(&self, data: &str) -> ServiceResult<()> {
        let mut w = self
            .writer
            .lock()
            .map_err(|_| ServiceError::internal("terminal writer poisoned"))?;
        w.write_all(data.as_bytes())
            .and_then(|_| w.flush())
            .map_err(|e| ServiceError::io(format!("terminal write: {e}")))
    }

    pub fn resize(&self, cols: u16, rows: u16) -> ServiceResult<()> {
        let m = self
            .master
            .lock()
            .map_err(|_| ServiceError::internal("terminal master poisoned"))?;
        m.resize(PtySize {
            rows: rows.max(1),
            cols: cols.max(1),
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|e| ServiceError::io(format!("terminal resize: {e}")))
    }

    /// Kill the shell; the reader thread then reports `terminal_exit`.
    pub fn close(&self) {
        if let Ok(mut k) = self.killer.lock() {
            let _ = k.kill();
        }
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        self.close();
    }
}

fn reader_loop(
    id: TerminalId,
    mut reader: Box<dyn Read + Send>,
    mut child: Box<dyn portable_pty::Child + Send + Sync>,
    events: EventHub,
) {
    let mut buf = [0u8; 8192];
    let mut carry: Vec<u8> = Vec::new();
    loop {
        match reader.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                carry.extend_from_slice(&buf[..n]);
                let (text, rest) = decode_prefix(&carry);
                events.terminal_data(&id, &text);
                carry = rest;
            }
        }
    }
    if !carry.is_empty() {
        events.terminal_data(&id, &String::from_utf8_lossy(&carry));
    }
    let code = child.wait().ok().map(|st| {
        if st.success() {
            0
        } else {
            st.exit_code() as i64
        }
    });
    events.terminal_exit(&id, code);
}

/// Decode as much of `bytes` as is valid UTF-8, keeping an incomplete trailing sequence (at
/// most 3 bytes) for the next read; invalid sequences are replaced.
fn decode_prefix(bytes: &[u8]) -> (String, Vec<u8>) {
    match std::str::from_utf8(bytes) {
        Ok(s) => (s.to_string(), vec![]),
        Err(e) => {
            let valid = e.valid_up_to();
            match e.error_len() {
                // Incomplete sequence at the end: keep it for the next chunk.
                None => (
                    String::from_utf8_lossy(&bytes[..valid]).to_string(),
                    bytes[valid..].to_vec(),
                ),
                Some(_) => (String::from_utf8_lossy(bytes).to_string(), vec![]),
            }
        }
    }
}

/// All terminals of the app, keyed by id.
#[derive(Default)]
pub struct TerminalManager {
    terminals: Mutex<HashMap<TerminalId, Arc<Terminal>>>,
}

impl TerminalManager {
    pub fn new() -> Self {
        TerminalManager::default()
    }

    pub fn insert(&self, t: Terminal) -> Arc<Terminal> {
        let t = Arc::new(t);
        if let Ok(mut m) = self.terminals.lock() {
            m.insert(t.id.clone(), t.clone());
        }
        t
    }

    pub fn get(&self, id: &str) -> ServiceResult<Arc<Terminal>> {
        self.terminals
            .lock()
            .ok()
            .and_then(|m| m.get(id).cloned())
            .ok_or_else(|| ServiceError::not_found(format!("terminal `{id}` is not open")))
    }

    pub fn remove(&self, id: &str) -> Option<Arc<Terminal>> {
        self.terminals.lock().ok().and_then(|mut m| m.remove(id))
    }

    /// Close every terminal of a session.
    pub fn close_session(&self, session: &str) {
        let Ok(mut m) = self.terminals.lock() else {
            return;
        };
        let ids: Vec<String> = m
            .values()
            .filter(|t| t.session == session)
            .map(|t| t.id.clone())
            .collect();
        for id in ids {
            if let Some(t) = m.remove(&id) {
                t.close();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use analyst_protocol::ui::SessionEvent;
    use std::time::{Duration, Instant};

    #[test]
    fn utf8_carry_over() {
        let s = "héllo";
        let b = s.as_bytes();
        let (t, rest) = decode_prefix(&b[..2]);
        assert_eq!(t, "h");
        assert_eq!(rest, vec![b[1]]);
        let (t, rest) = decode_prefix(&[0xff, b'a']);
        assert_eq!(t, "\u{fffd}a");
        assert!(rest.is_empty());
    }

    #[tokio::test]
    async fn local_terminal_echoes_a_command() {
        let repo = tempfile::tempdir().unwrap();
        let hub = EventHub::new();
        let mut rx = hub.subscribe();
        // The app's own environment holds a provider key; the shell must not see it.
        std::env::set_var("ANALYST_TEST_PROVIDER_KEY", "sk-secret");
        let spec = TerminalSpec {
            repo: repo.path().to_path_buf(),
            target: Target::Local,
            control_dir: None,
            capture: CaptureEnv::new(repo.path(), "out", None),
            shell: Some("/bin/sh".into()),
            env_remove: vec!["ANALYST_TEST_PROVIDER_KEY".into()],
            cols: 80,
            rows: 24,
        };
        let t = Terminal::open("t1".into(), "s1", &spec, hub.clone()).unwrap();
        t.write("echo hello; echo key=[$ANALYST_TEST_PROVIDER_KEY]; echo $ANALYST_RUN_ID; echo $MPLBACKEND\n")
            .unwrap();
        t.resize(100, 30).unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut seen = String::new();
        while Instant::now() < deadline && !seen.contains("module://analyst_mpl") {
            match tokio::time::timeout(Duration::from_secs(1), rx.recv()).await {
                Ok(Ok(SessionEvent::TerminalData { terminal_id, data })) => {
                    assert_eq!(terminal_id, "t1");
                    seen.push_str(&data);
                }
                Ok(Ok(_)) => {}
                _ => {}
            }
        }
        assert!(seen.contains("hello"), "{seen:?}");
        assert!(seen.contains("terminal-"), "{seen:?}");
        assert!(seen.contains("key=[]"), "provider key leaked: {seen:?}");
        assert!(!seen.contains("sk-secret"), "{seen:?}");
        t.write("exit 3\n").unwrap();
        let mut code = None;
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline && code.is_none() {
            if let Ok(Ok(SessionEvent::TerminalExit { code: c, .. })) =
                tokio::time::timeout(Duration::from_secs(1), rx.recv()).await
            {
                code = Some(c);
            }
        }
        assert_eq!(code, Some(Some(3)));
    }
}

#[cfg(test)]
mod remote_tests {
    use super::*;

    #[test]
    fn remote_command_uses_the_configured_out_dir_and_quotes_paths() {
        let repo = std::path::Path::new("/r");
        let c = CaptureEnv::new(repo, "results dir", None);
        let cmd = c.remote_command("~/analyst/my repo");
        assert!(cmd.starts_with("cd ~/'analyst/my repo' && "), "{cmd}");
        assert!(
            cmd.contains("ANALYST_OUT_DIR=\"$PWD\"/'results dir/terminal/"),
            "{cmd}"
        );
        assert!(!cmd.contains("$PWD/out/terminal"), "{cmd}");
        let names = key_env_names(&Default::default());
        assert!(names.iter().any(|n| n == "ANTHROPIC_API_KEY"));
    }
}
