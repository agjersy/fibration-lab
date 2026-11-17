//! `[network]` settings against real sockets (Present PROTOCOL 4.1, 6.1): a private CA
//! through `ca_bundle`, and an explicit proxy through a local CONNECT proxy.

mod common;

use common::{key_stores, test_pki, ConnectProxy, Reply, TestServer};
use std::collections::BTreeMap;
use wb_providers::{
    client_for, collect, AuthKind, CaBundle, ChatRequest, ClientOptions, Collected, Location,
    Message, NetworkConfig, ProviderConfig, ProviderKind, ProxySetting, StopReason,
};

const ANTHROPIC_TEXT: &str = include_str!("../src/fixtures/anthropic_text.sse");

fn provider(port: u16) -> ProviderConfig {
    ProviderConfig {
        name: "gateway".into(),
        kind: ProviderKind::Anthropic,
        // The test certificate is issued for `localhost`.
        base_url: Some(format!("https://localhost:{port}")),
        auth: Some(AuthKind::Bearer),
        key: Some("keychain:gateway".into()),
        headers: BTreeMap::new(),
        location: Location::Internal,
    }
}

async fn ask(provider: &ProviderConfig, network: &NetworkConfig) -> Collected {
    let keys = key_stores(&[("gateway", "k")]);
    let client = client_for(
        provider,
        "m",
        &ClientOptions {
            network,
            keys: &keys,
            audit: None,
            home: None,
        },
    )
    .unwrap();
    collect(client.stream(ChatRequest {
        model: "m".into(),
        messages: vec![Message::user_text("hi")],
        ..Default::default()
    }))
    .await
}

#[tokio::test]
async fn private_ca_works_with_the_bundle_and_fails_without() {
    let pki = test_pki();
    let server =
        TestServer::start_tls(pki.acceptor.clone(), |_, _| Reply::sse(ANTHROPIC_TEXT)).await;
    let dir = tempfile::tempdir().unwrap();
    let bundle = dir.path().join("corp-ca.pem");
    std::fs::write(&bundle, &pki.ca_pem).unwrap();
    let p = provider(server.addr.port());

    // Without the bundle the server's certificate is not trusted: no request gets through.
    let system_only = NetworkConfig {
        proxy: ProxySetting::None,
        ca_bundle: CaBundle::System,
    };
    let c = ask(&p, &system_only).await;
    assert_eq!(c.stop, Some(StopReason::Error));
    let msg = c.stop_message.unwrap();
    assert!(msg.starts_with("request failed"), "{msg}");
    assert!(
        msg.to_lowercase().contains("certificate") || msg.contains("UnknownIssuer"),
        "expected a certificate error, got: {msg}"
    );
    assert!(server.requests().is_empty());

    // With the bundle the same request succeeds.
    let with_bundle = NetworkConfig {
        proxy: ProxySetting::None,
        ca_bundle: CaBundle::Path(bundle),
    };
    let c = ask(&p, &with_bundle).await;
    assert_eq!(c.stop, Some(StopReason::End), "{:?}", c.stop_message);
    assert_eq!(c.text, "The fit converged.");
    let reqs = server.requests();
    assert_eq!(reqs.len(), 1);
    assert_eq!(reqs[0].header("authorization"), Some("Bearer k"));

    // A bundle holding a different CA does not help.
    let other = dir.path().join("other-ca.pem");
    std::fs::write(&other, test_pki().ca_pem).unwrap();
    let wrong_bundle = NetworkConfig {
        proxy: ProxySetting::None,
        ca_bundle: CaBundle::Path(other),
    };
    assert_eq!(ask(&p, &wrong_bundle).await.stop, Some(StopReason::Error));
    assert_eq!(server.requests().len(), 1);
}

#[tokio::test]
async fn configured_proxy_carries_the_request() {
    let pki = test_pki();
    let server =
        TestServer::start_tls(pki.acceptor.clone(), |_, _| Reply::sse(ANTHROPIC_TEXT)).await;
    let proxy = ConnectProxy::start().await;
    let dir = tempfile::tempdir().unwrap();
    let bundle = dir.path().join("corp-ca.pem");
    std::fs::write(&bundle, &pki.ca_pem).unwrap();
    let p = provider(server.addr.port());

    // proxy = none: direct, the proxy sees nothing.
    let direct = NetworkConfig {
        proxy: ProxySetting::None,
        ca_bundle: CaBundle::Path(bundle.clone()),
    };
    assert_eq!(ask(&p, &direct).await.stop, Some(StopReason::End));
    assert!(proxy.targets().is_empty());
    assert_eq!(server.requests().len(), 1);

    // proxy = "<url>": the request is tunnelled through it, TLS still verified end to end
    // against the bundle.
    let proxied = NetworkConfig {
        proxy: ProxySetting::Url(proxy.url()),
        ca_bundle: CaBundle::Path(bundle),
    };
    let c = ask(&p, &proxied).await;
    assert_eq!(c.stop, Some(StopReason::End), "{:?}", c.stop_message);
    assert_eq!(c.text, "The fit converged.");
    assert_eq!(
        proxy.targets(),
        vec![format!("localhost:{}", server.addr.port())]
    );
    assert_eq!(server.requests().len(), 2);

    // A proxy that is not there: the request fails rather than going direct.
    let dead = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let dead_addr = dead.local_addr().unwrap();
    drop(dead);
    let broken = NetworkConfig {
        proxy: ProxySetting::Url(format!("http://{dead_addr}")),
        ca_bundle: CaBundle::System,
    };
    assert_eq!(ask(&p, &broken).await.stop, Some(StopReason::Error));
    assert_eq!(server.requests().len(), 2);
}
