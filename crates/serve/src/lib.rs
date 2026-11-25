//! fibration-serve: the development bridge (docs/IPC.md section 11).
//!
//! Hosts the same `Workbench` the desktop shell does and speaks the shell's commands and the
//! per-session events over one WebSocket, so the renderer in a plain browser (the Vite dev
//! server, the Playwright suite) talks to the real core. It is a separate binary that the
//! shipped app never links (`tools/check_egress.py` enforces both).
//!
//! Rules: it binds loopback addresses only; every connection must carry the token printed at
//! start (`?token=...` on the WebSocket URL, since browsers cannot set headers on a WebSocket);
//! a browser `Origin` must be a loopback origin; it serves no files and sends no CORS headers.
//!
//! Wire format, one JSON object per text message:
//! - request `{"id": n, "cmd": "<command>", "args": {...}}`
//! - reply `{"id": n, "ok": <result>}` or `{"id": n, "error": IpcError}`
//! - event `{"channel": c, "event": SessionEvent}` for the `channel` number the client passed
//!   to `session_subscribe { session, channel }` (the WebSocket's stand-in for a Tauri channel).

use fibration_core::commands::{dispatch, DialogFilter, Host};
use fibration_core::service::Workbench;
use fibration_protocol::ui::{IpcError, SessionEvent};
use futures::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::{Arc, Mutex};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::handshake::server::{ErrorResponse, Request, Response};
use tokio_tungstenite::tungstenite::http::StatusCode;
use tokio_tungstenite::tungstenite::Message;

/// A fresh random token: 32 bytes, hex.
pub fn random_token() -> String {
    use rand::RngCore;
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    hex::encode(bytes)
}

/// Refuses any address that is not a loopback address.
pub fn check_bind(addr: SocketAddr) -> anyhow::Result<()> {
    if addr.ip().is_loopback() {
        Ok(())
    } else {
        anyhow::bail!("fibration-serve binds loopback addresses only, not {addr}")
    }
}

/// Binds the listener after [`check_bind`]. Port 0 picks a free port.
pub async fn bind(addr: SocketAddr) -> anyhow::Result<TcpListener> {
    check_bind(addr)?;
    Ok(TcpListener::bind(addr).await?)
}

/// What the bridge answers for the native dialogs it cannot show: fixed answers given at start
/// (`--pick-dir`, `--pick-file`), or "cancelled".
#[derive(Debug, Clone, Default)]
pub struct Dialogs {
    pub directory: Option<String>,
    pub files: Vec<String>,
}

struct Sub {
    conn: u64,
    session: String,
    handle: tokio::task::JoinHandle<()>,
}

/// The bridge: the workbench, the token, and the live subscriptions of all connections.
pub struct Bridge {
    workbench: Arc<Workbench>,
    token: String,
    dialogs: Dialogs,
    subs: Mutex<HashMap<String, Sub>>,
    next_conn: std::sync::atomic::AtomicU64,
}

impl Bridge {
    pub fn new(workbench: Arc<Workbench>, token: String, dialogs: Dialogs) -> Arc<Self> {
        Arc::new(Bridge {
            workbench,
            token,
            dialogs,
            subs: Mutex::new(HashMap::new()),
            next_conn: Default::default(),
        })
    }

    pub fn workbench(&self) -> &Arc<Workbench> {
        &self.workbench
    }

    fn drop_subs(&self, keep: impl Fn(&Sub) -> bool) {
        let Ok(mut subs) = self.subs.lock() else {
            return;
        };
        subs.retain(|_, s| {
            let k = keep(s);
            if !k {
                s.handle.abort();
            }
            k
        });
    }

    fn subscribe(
        &self,
        conn: u64,
        args: Value,
        out: mpsc::UnboundedSender<String>,
    ) -> Result<Value, IpcError> {
        let session = args
            .get("session")
            .and_then(Value::as_str)
            .ok_or_else(|| IpcError::invalid("session_subscribe: missing `session`"))?
            .to_string();
        let channel = args
            .get("channel")
            .and_then(Value::as_u64)
            .ok_or_else(|| IpcError::invalid("session_subscribe: missing numeric `channel`"))?;
        let rt = self.workbench.session(&session).map_err(IpcError::from)?;
        let mut rx = rt.subscribe();
        drop(rt);
        let id = format!("sub-{}", fibration_core::util::stamped_id());
        let handle = tokio::spawn(async move {
            while let Ok(event) = rx.recv().await {
                if matches!(event, SessionEvent::Resync) {
                    tracing::warn!("channel {channel}: fell behind, asking the renderer to resync");
                }
                let msg = json!({ "channel": channel, "event": event }).to_string();
                if out.send(msg).is_err() {
                    break;
                }
            }
        });
        if let Ok(mut subs) = self.subs.lock() {
            subs.insert(
                id.clone(),
                Sub {
                    conn,
                    session,
                    handle,
                },
            );
        }
        Ok(Value::String(id))
    }

    fn unsubscribe(&self, args: &Value) -> Result<Value, IpcError> {
        let id = args
            .get("subscription")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let sub = self.subs.lock().ok().and_then(|mut s| s.remove(id));
        match sub {
            Some(s) => {
                s.handle.abort();
                Ok(Value::Null)
            }
            None => Err(IpcError::not_found(format!(
                "subscription `{id}` not found"
            ))),
        }
    }
}

struct ServeHost<'a>(&'a Bridge);

#[async_trait::async_trait]
impl Host for ServeHost<'_> {
    fn session_closed(&self, session: &str) {
        self.0.drop_subs(|s| s.session != session);
    }

    async fn pick_directory(&self, _title: Option<String>) -> Result<Option<String>, IpcError> {
        Ok(self.0.dialogs.directory.clone())
    }

    async fn pick_files(
        &self,
        _title: Option<String>,
        _filters: Vec<DialogFilter>,
        multiple: bool,
    ) -> Result<Option<Vec<String>>, IpcError> {
        let files = &self.0.dialogs.files;
        Ok(match (files.is_empty(), multiple) {
            (true, _) => None,
            (false, true) => Some(files.clone()),
            (false, false) => Some(vec![files[0].clone()]),
        })
    }

    fn reveal(&self, path: &Path) -> Result<(), IpcError> {
        tracing::info!("reveal (not shown by the bridge): {}", path.display());
        Ok(())
    }

    fn open(&self, path: &Path) -> Result<(), IpcError> {
        tracing::info!("open (not shown by the bridge): {}", path.display());
        Ok(())
    }
}

/// Constant-time comparison of two tokens.
fn same_token(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// A browser origin the bridge accepts: a loopback HTTP origin (the renderer's dev or preview
/// server). Clients that send no `Origin` (not a browser) pass on the token alone.
fn loopback_origin(origin: &str) -> bool {
    let Some(rest) = origin.strip_prefix("http://") else {
        return false;
    };
    let host = rest.rsplit_once(':').map_or(rest, |(h, port)| {
        if port.chars().all(|c| c.is_ascii_digit()) {
            h
        } else {
            rest
        }
    });
    matches!(host, "127.0.0.1" | "localhost" | "[::1]")
}

fn query_token(req: &Request) -> Option<String> {
    req.uri().query()?.split('&').find_map(|kv| {
        let (k, v) = kv.split_once('=')?;
        (k == "token").then(|| v.to_string())
    })
}

fn refuse(status: StatusCode, why: &str) -> ErrorResponse {
    let mut resp = ErrorResponse::new(Some(why.to_string()));
    *resp.status_mut() = status;
    resp
}

/// Accepts connections until the listener fails.
pub async fn serve(listener: TcpListener, bridge: Arc<Bridge>) -> anyhow::Result<()> {
    loop {
        let (stream, peer) = listener.accept().await?;
        let bridge = bridge.clone();
        tokio::spawn(async move {
            if let Err(e) = connection(stream, peer, bridge).await {
                tracing::debug!("connection from {peer}: {e:#}");
            }
        });
    }
}

async fn connection(
    stream: TcpStream,
    peer: SocketAddr,
    bridge: Arc<Bridge>,
) -> anyhow::Result<()> {
    if !peer.ip().is_loopback() {
        anyhow::bail!("refused a non-loopback peer");
    }
    let token = bridge.token.clone();
    // The callback's signature is tungstenite's; its error is the HTTP refusal itself.
    #[allow(clippy::result_large_err)]
    let check = move |req: &Request, resp: Response| -> Result<Response, ErrorResponse> {
        if let Some(origin) = req.headers().get("origin") {
            if !origin.to_str().is_ok_and(loopback_origin) {
                return Err(refuse(StatusCode::FORBIDDEN, "origin not allowed"));
            }
        }
        match query_token(req) {
            Some(t) if same_token(&t, &token) => Ok(resp),
            _ => Err(refuse(StatusCode::UNAUTHORIZED, "missing or wrong token")),
        }
    };
    let ws = tokio_tungstenite::accept_hdr_async(stream, check).await?;
    let conn = bridge
        .next_conn
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let (mut sink, mut source) = ws.split();
    let (tx, mut rx) = mpsc::unbounded_channel::<String>();
    let writer = tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            if sink.send(Message::text(msg)).await.is_err() {
                break;
            }
        }
    });
    while let Some(msg) = source.next().await {
        let text = match msg? {
            Message::Text(t) => t.to_string(),
            Message::Close(_) => break,
            _ => continue,
        };
        let bridge = bridge.clone();
        let tx = tx.clone();
        tokio::spawn(async move {
            let reply = request(&bridge, conn, &text, tx.clone()).await;
            let _ = tx.send(reply.to_string());
        });
    }
    bridge.drop_subs(|s| s.conn != conn);
    drop(tx);
    writer.abort();
    Ok(())
}

async fn request(
    bridge: &Bridge,
    conn: u64,
    text: &str,
    out: mpsc::UnboundedSender<String>,
) -> Value {
    let req: Value = match serde_json::from_str(text) {
        Ok(v) => v,
        Err(e) => {
            return json!({ "id": null, "error": IpcError::invalid(format!("bad request: {e}")) })
        }
    };
    let id = req.get("id").cloned().unwrap_or(Value::Null);
    let cmd = req.get("cmd").and_then(Value::as_str).unwrap_or_default();
    let args = req.get("args").cloned().unwrap_or(Value::Null);
    let result = match cmd {
        "session_subscribe" => bridge.subscribe(conn, args, out),
        "session_unsubscribe" => bridge.unsubscribe(&args),
        _ => dispatch(&bridge.workbench, &ServeHost(bridge), cmd, args).await,
    };
    match result {
        Ok(v) => json!({ "id": id, "ok": v }),
        Err(e) => json!({ "id": id, "error": e }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_loopback_origins_pass() {
        for ok in [
            "http://127.0.0.1:4173",
            "http://localhost:5173",
            "http://localhost",
            "http://[::1]:80",
        ] {
            assert!(loopback_origin(ok), "{ok}");
        }
        for bad in [
            "https://evil.example",
            "http://evil.example:4173",
            "http://127.0.0.1.evil.example",
            "null",
            "file://",
            "http://localhost.evil:1",
        ] {
            assert!(!loopback_origin(bad), "{bad}");
        }
    }

    #[test]
    fn tokens_compare_exactly() {
        assert!(same_token("abc", "abc"));
        assert!(!same_token("abc", "abd"));
        assert!(!same_token("abc", "abcd"));
        assert_eq!(random_token().len(), 64);
        assert_ne!(random_token(), random_token());
    }

    #[test]
    fn non_loopback_binds_are_refused() {
        assert!(check_bind("127.0.0.1:0".parse().unwrap()).is_ok());
        assert!(check_bind("[::1]:0".parse().unwrap()).is_ok());
        assert!(check_bind("0.0.0.0:0".parse().unwrap()).is_err());
        assert!(check_bind("192.168.1.2:8080".parse().unwrap()).is_err());
    }
}
