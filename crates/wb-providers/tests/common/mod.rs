//! Minimal local HTTP/1.1 test server (tokio only; no mock framework). One request per connection; the handler sees the full recorded request.
#![allow(dead_code)]

use serde_json::Value;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpListener;
use wb_providers::keys::{KeyRef, KeyStores};

#[derive(Debug, Clone)]
pub struct Recorded {
    pub method: String,
    pub target: String,
    /// Header names lower-cased.
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Recorded {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    }

    pub fn json(&self) -> Value {
        serde_json::from_slice(&self.body).expect("request body is JSON")
    }

    pub fn body_text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
}

#[derive(Debug, Clone)]
pub struct Reply {
    pub status: u16,
    pub content_type: &'static str,
    pub body: String,
}

impl Reply {
    pub fn sse(body: impl Into<String>) -> Reply {
        Reply {
            status: 200,
            content_type: "text/event-stream",
            body: body.into(),
        }
    }

    pub fn json(status: u16, body: Value) -> Reply {
        Reply {
            status,
            content_type: "application/json",
            body: body.to_string(),
        }
    }
}

/// Called with the request and its zero-based sequence number.
pub type Handler = Arc<dyn Fn(&Recorded, usize) -> Reply + Send + Sync>;

pub struct TestServer {
    pub addr: SocketAddr,
    requests: Arc<Mutex<Vec<Recorded>>>,
}

impl TestServer {
    /// Plain HTTP on 127.0.0.1. The server's address is allowed at the network gate as a
    /// model endpoint, as a configured provider's `base_url` would be.
    pub async fn start(
        handler: impl Fn(&Recorded, usize) -> Reply + Send + Sync + 'static,
    ) -> Self {
        let handler: Handler = Arc::new(handler);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        wb_net::gate()
            .allow_model_endpoint(&format!("http://{addr}"))
            .unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let recorded = requests.clone();
        tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    return;
                };
                let (handler, recorded) = (handler.clone(), recorded.clone());
                tokio::spawn(serve(stream, handler, recorded));
            }
        });
        TestServer { addr, requests }
    }

    pub fn url(&self) -> String {
        format!("http://{}", self.addr)
    }

    pub fn requests(&self) -> Vec<Recorded> {
        self.requests.lock().unwrap().clone()
    }
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

async fn read_head<S: AsyncRead + Unpin>(stream: &mut S) -> Option<(Vec<u8>, Vec<u8>)> {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 8192];
    loop {
        if let Some(i) = find(&buf, b"\r\n\r\n") {
            let rest = buf.split_off(i + 4);
            return Some((buf, rest));
        }
        let n = stream.read(&mut tmp).await.ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&tmp[..n]);
    }
}

fn parse_head(head: &[u8]) -> (String, String, Vec<(String, String)>) {
    let text = String::from_utf8_lossy(head);
    let mut lines = text.split("\r\n");
    let mut first = lines.next().unwrap_or("").split(' ');
    let method = first.next().unwrap_or("").to_string();
    let target = first.next().unwrap_or("").to_string();
    let headers = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(n, v)| (n.trim().to_ascii_lowercase(), v.trim().to_string()))
        .collect();
    (method, target, headers)
}

async fn serve<S: AsyncRead + AsyncWrite + Unpin>(
    mut stream: S,
    handler: Handler,
    recorded: Arc<Mutex<Vec<Recorded>>>,
) {
    let Some((head, mut body)) = read_head(&mut stream).await else {
        return;
    };
    let (method, target, headers) = parse_head(&head);
    let len: usize = headers
        .iter()
        .find(|(n, _)| n == "content-length")
        .and_then(|(_, v)| v.parse().ok())
        .unwrap_or(0);
    let mut tmp = [0u8; 8192];
    while body.len() < len {
        match stream.read(&mut tmp).await {
            Ok(0) | Err(_) => return,
            Ok(n) => body.extend_from_slice(&tmp[..n]),
        }
    }
    let req = Recorded {
        method,
        target,
        headers,
        body,
    };
    let seq = {
        let mut r = recorded.lock().unwrap();
        r.push(req.clone());
        r.len() - 1
    };
    let reply = handler(&req, seq);
    let reason = if reply.status == 200 { "OK" } else { "Error" };
    let out = format!(
        "HTTP/1.1 {} {reason}\r\ncontent-type: {}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
        reply.status,
        reply.content_type,
        reply.body.len(),
        reply.body
    );
    let _ = stream.write_all(out.as_bytes()).await;
    let _ = stream.shutdown().await;
}

/// Key stores backed by a temporary home `.env` holding `entries` (variable, value). The
/// variable names used in these tests are never set in the process environment.
pub struct TestKeys {
    _home: tempfile::TempDir,
    stores: KeyStores,
}

impl std::ops::Deref for TestKeys {
    type Target = KeyStores;
    fn deref(&self) -> &KeyStores {
        &self.stores
    }
}

pub fn key_stores(entries: &[(&str, &str)]) -> TestKeys {
    let home = tempfile::tempdir().unwrap();
    let stores = KeyStores::for_home(home.path());
    for (var, value) in entries {
        stores.set(&KeyRef::Env(var.to_string()), value).unwrap();
    }
    TestKeys {
        _home: home,
        stores,
    }
}
