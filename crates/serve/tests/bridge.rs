//! The bridge end to end over a real socket: token required, loopback only, a command round
//! trip through the one command table, and a session event delivered on the client's channel.

use analyst_core::service::{Workbench, WorkbenchOptions};
use analyst_serve::{Bridge, Dialogs};
use futures::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Duration;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message;

type Ws =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

struct Served {
    _home: tempfile::TempDir,
    url: String,
    token: String,
}

async fn start() -> Served {
    let home = tempfile::tempdir().unwrap();
    let mut opts = WorkbenchOptions::new(home.path().to_path_buf());
    opts.autostart_kernel = false;
    opts.init = analyst_core::init::InitOptions {
        run_uv: false,
        git: true,
    };
    let wb = Arc::new(Workbench::new(opts).unwrap());
    let token = analyst_serve::random_token();
    let listener = analyst_serve::bind("127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();
    let url = format!("ws://{}/", listener.local_addr().unwrap());
    let bridge = Bridge::new(wb, token.clone(), Dialogs::default());
    tokio::spawn(analyst_serve::serve(listener, bridge));
    Served {
        _home: home,
        url,
        token,
    }
}

async fn connect(s: &Served) -> Ws {
    let (ws, _) = tokio_tungstenite::connect_async(format!("{}?token={}", s.url, s.token))
        .await
        .unwrap();
    ws
}

async fn call(ws: &mut Ws, id: u64, cmd: &str, args: Value) -> Value {
    ws.send(Message::text(
        json!({ "id": id, "cmd": cmd, "args": args }).to_string(),
    ))
    .await
    .unwrap();
    loop {
        let msg = tokio::time::timeout(Duration::from_secs(20), ws.next())
            .await
            .expect("reply in time")
            .unwrap()
            .unwrap();
        let Message::Text(t) = msg else { continue };
        let v: Value = serde_json::from_str(&t).unwrap();
        if v.get("id") == Some(&json!(id)) {
            return v;
        }
    }
}

fn http_status(e: tokio_tungstenite::tungstenite::Error) -> u16 {
    match e {
        tokio_tungstenite::tungstenite::Error::Http(resp) => resp.status().as_u16(),
        other => panic!("expected an HTTP refusal, got {other}"),
    }
}

#[tokio::test]
async fn a_connection_without_the_right_token_is_refused() {
    let s = start().await;
    let missing = tokio_tungstenite::connect_async(s.url.clone())
        .await
        .unwrap_err();
    assert_eq!(http_status(missing), 401);
    let wrong = tokio_tungstenite::connect_async(format!("{}?token=nope", s.url))
        .await
        .unwrap_err();
    assert_eq!(http_status(wrong), 401);
    let almost = tokio_tungstenite::connect_async(format!("{}?token={}0", s.url, s.token))
        .await
        .unwrap_err();
    assert_eq!(http_status(almost), 401);
}

#[tokio::test]
async fn a_foreign_browser_origin_is_refused_even_with_the_token() {
    let s = start().await;
    let mut req = format!("{}?token={}", s.url, s.token)
        .into_client_request()
        .unwrap();
    req.headers_mut()
        .insert("origin", "https://evil.example".parse().unwrap());
    let err = tokio_tungstenite::connect_async(req).await.unwrap_err();
    assert_eq!(http_status(err), 403);
    let mut req = format!("{}?token={}", s.url, s.token)
        .into_client_request()
        .unwrap();
    req.headers_mut()
        .insert("origin", "http://127.0.0.1:4173".parse().unwrap());
    tokio_tungstenite::connect_async(req)
        .await
        .expect("a loopback origin passes");
}

#[tokio::test]
async fn only_loopback_addresses_can_be_bound() {
    for addr in ["0.0.0.0:0", "[::]:0", "10.0.0.1:0"] {
        let err = analyst_serve::bind(addr.parse().unwrap())
            .await
            .unwrap_err();
        assert!(err.to_string().contains("loopback"), "{addr}: {err}");
    }
    analyst_serve::bind("127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();
}

#[tokio::test]
async fn a_command_round_trips_through_the_one_table() {
    let s = start().await;
    let mut ws = connect(&s).await;
    let info = call(&mut ws, 1, "app_info", json!({})).await;
    assert!(info["ok"]["version"].is_string(), "{info}");
    // Errors arrive as IpcError.
    let bad = call(&mut ws, 2, "session_info", json!({ "session": "nope" })).await;
    assert_eq!(bad["error"]["code"], "not_found", "{bad}");
    let unknown = call(&mut ws, 3, "no_such_command", json!({})).await;
    assert_eq!(unknown["error"]["code"], "not_found", "{unknown}");
    let invalid = call(&mut ws, 4, "session_info", json!({})).await;
    assert_eq!(invalid["error"]["code"], "invalid", "{invalid}");
}

#[tokio::test]
async fn a_session_event_is_delivered_on_the_clients_channel() {
    let s = start().await;
    let repo = tempfile::tempdir().unwrap();
    let mut ws = connect(&s).await;
    let created = call(
        &mut ws,
        1,
        "session_create",
        json!({ "repo_abs": repo.path(), "init": true }),
    )
    .await;
    let session = created["ok"]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("{created}"))
        .to_string();
    let sub = call(
        &mut ws,
        2,
        "session_subscribe",
        json!({ "session": session, "channel": 7 }),
    )
    .await;
    assert!(sub["ok"].as_str().unwrap().starts_with("sub-"), "{sub}");
    // A new conversation changes the session: `session_changed` reaches channel 7.
    let conv = call(
        &mut ws,
        3,
        "conversation_new",
        json!({ "session": session, "title": "t" }),
    )
    .await;
    assert!(conv["ok"]["id"].is_string(), "{conv}");
    let event = loop {
        let msg = tokio::time::timeout(Duration::from_secs(20), ws.next())
            .await
            .expect("event in time")
            .unwrap()
            .unwrap();
        let Message::Text(t) = msg else { continue };
        let v: Value = serde_json::from_str(&t).unwrap();
        if v.get("channel") == Some(&json!(7)) && v["event"]["type"] == "session_changed" {
            break v;
        }
    };
    assert_eq!(event["event"]["type"], "session_changed");
    // Unsubscribing ends it; a second unsubscribe is not_found.
    let id = sub["ok"].clone();
    assert_eq!(
        call(
            &mut ws,
            4,
            "session_unsubscribe",
            json!({ "subscription": id })
        )
        .await["ok"],
        Value::Null
    );
    assert_eq!(
        call(
            &mut ws,
            5,
            "session_unsubscribe",
            json!({ "subscription": id })
        )
        .await["error"]["code"],
        "not_found"
    );
}
