//! Minimal local HTTP/1.1 test server, CONNECT proxy, and test CA (tokio only; no mock
//! framework). One request per connection; the handler sees the full recorded request.
#![allow(dead_code)]

use serde_json::Value;
use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use wb_providers::keys::{KeyError, KeyStore, KeyStores};

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
    /// Plain HTTP on 127.0.0.1.
    pub async fn start(
        handler: impl Fn(&Recorded, usize) -> Reply + Send + Sync + 'static,
    ) -> Self {
        Self::start_inner(Arc::new(handler), None).await
    }

    /// HTTPS on 127.0.0.1 with the given certificate.
    pub async fn start_tls(
        tls: tokio_rustls::TlsAcceptor,
        handler: impl Fn(&Recorded, usize) -> Reply + Send + Sync + 'static,
    ) -> Self {
        Self::start_inner(Arc::new(handler), Some(tls)).await
    }

    async fn start_inner(handler: Handler, tls: Option<tokio_rustls::TlsAcceptor>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let recorded = requests.clone();
        tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    return;
                };
                let (handler, recorded, tls) = (handler.clone(), recorded.clone(), tls.clone());
                tokio::spawn(async move {
                    match tls {
                        // A failed handshake (client does not trust us) just ends the task.
                        Some(acceptor) => {
                            if let Ok(s) = acceptor.accept(stream).await {
                                serve(s, handler, recorded).await;
                            }
                        }
                        None => serve(stream, handler, recorded).await,
                    }
                });
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

/// A forward proxy that only understands `CONNECT host:port` and tunnels bytes.
pub struct ConnectProxy {
    pub addr: SocketAddr,
    targets: Arc<Mutex<Vec<String>>>,
}

impl ConnectProxy {
    pub async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let targets = Arc::new(Mutex::new(Vec::new()));
        let seen = targets.clone();
        tokio::spawn(async move {
            loop {
                let Ok((mut client, _)) = listener.accept().await else {
                    return;
                };
                let seen = seen.clone();
                tokio::spawn(async move {
                    let Some((head, early)) = read_head(&mut client).await else {
                        return;
                    };
                    let (method, target, _) = parse_head(&head);
                    if method != "CONNECT" {
                        let _ = client
                            .write_all(
                                b"HTTP/1.1 405 Method Not Allowed\r\ncontent-length: 0\r\n\r\n",
                            )
                            .await;
                        return;
                    }
                    seen.lock().unwrap().push(target.clone());
                    let Ok(mut upstream) = TcpStream::connect(&target).await else {
                        let _ = client
                            .write_all(b"HTTP/1.1 502 Bad Gateway\r\ncontent-length: 0\r\n\r\n")
                            .await;
                        return;
                    };
                    if client
                        .write_all(b"HTTP/1.1 200 Connection established\r\n\r\n")
                        .await
                        .is_err()
                    {
                        return;
                    }
                    let _ = upstream.write_all(&early).await;
                    let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
                });
            }
        });
        ConnectProxy { addr, targets }
    }

    pub fn url(&self) -> String {
        format!("http://{}", self.addr)
    }

    /// `host:port` of every CONNECT seen.
    pub fn targets(&self) -> Vec<String> {
        self.targets.lock().unwrap().clone()
    }
}

/// A throwaway CA and a server certificate for `localhost` signed by it.
pub struct TestPki {
    pub ca_pem: String,
    pub acceptor: tokio_rustls::TlsAcceptor,
}

pub fn test_pki() -> TestPki {
    use rcgen::{
        BasicConstraints, CertificateParams, DnType, ExtendedKeyUsagePurpose, IsCa, KeyPair,
        KeyUsagePurpose,
    };
    use tokio_rustls::rustls;

    let mut ca_params = CertificateParams::new(Vec::<String>::new()).unwrap();
    ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    ca_params
        .distinguished_name
        .push(DnType::CommonName, "wb-providers test CA");
    ca_params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
    let ca_key = KeyPair::generate().unwrap();
    let ca_cert = ca_params.self_signed(&ca_key).unwrap();

    let mut leaf_params = CertificateParams::new(vec!["localhost".to_string()]).unwrap();
    leaf_params
        .distinguished_name
        .push(DnType::CommonName, "localhost");
    leaf_params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    let leaf_key = KeyPair::generate().unwrap();
    let leaf_cert = leaf_params.signed_by(&leaf_key, &ca_cert, &ca_key).unwrap();

    let key = rustls::pki_types::PrivateKeyDer::Pkcs8(leaf_key.serialize_der().into());
    let config = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .unwrap()
    .with_no_client_auth()
    .with_single_cert(vec![leaf_cert.der().clone()], key)
    .unwrap();
    TestPki {
        ca_pem: ca_cert.pem(),
        acceptor: tokio_rustls::TlsAcceptor::from(Arc::new(config)),
    }
}

/// In-memory key store.
#[derive(Default)]
pub struct MemoryKeys(pub Mutex<BTreeMap<String, String>>);

impl KeyStore for MemoryKeys {
    fn get(&self, entry: &str) -> Result<Option<String>, KeyError> {
        Ok(self.0.lock().unwrap().get(entry).cloned())
    }
    fn set(&self, entry: &str, secret: &str) -> Result<(), KeyError> {
        self.0.lock().unwrap().insert(entry.into(), secret.into());
        Ok(())
    }
    fn delete(&self, entry: &str) -> Result<(), KeyError> {
        self.0.lock().unwrap().remove(entry);
        Ok(())
    }
}

/// Key stores whose keychain holds `entries` and whose environment is empty.
pub fn key_stores(entries: &[(&str, &str)]) -> KeyStores {
    let keychain = MemoryKeys::default();
    for (k, v) in entries {
        keychain.set(k, v).unwrap();
    }
    KeyStores {
        env: Box::new(MemoryKeys::default()),
        keychain: Box::new(keychain),
    }
}
