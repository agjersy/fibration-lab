#!/usr/bin/env python3
"""Fail if anything but the network gate (crates/wb-net) can open an outbound connection.

The app may reach the network for model endpoints, package installs, and SSH targets only, and
only through wb-net. This check is static and deliberately blunt:

1. No crate except wb-net depends on an HTTP or socket client crate.
2. No Rust source outside wb-net builds an HTTP client.
3. The Python packages that ship (the kernels, the figure backend, the deck SDK) import no
   network client module and open no sockets.
4. The desktop shells ship a content security policy with no remote or dev-server origin, and
   neither they nor the renderers pull in an updater or HTTP plugin.
5. No crate except the development bridge (crates/serve, `fibration-serve`) depends on a
   WebSocket or HTTP server crate, and neither the desktop app nor the CLI links the bridge or
   a server crate (checked through Cargo.lock, transitively). The shipped app has no listener.

It cannot see subprocesses: `uv` and `ssh` are covered by the rule that the core asks the gate
before it spawns them (wb_net::Gate::authorize_package_index / authorize_ssh).
"""
import json
import pathlib
import re
import sys
import tomllib

ROOT = pathlib.Path(__file__).resolve().parent.parent
GATE = ROOT / "crates" / "wb-net"
BRIDGE = ROOT / "crates" / "serve"
BRIDGE_CRATE = "fibration-serve"
SHIPPED = ("fibration-desktop", "fibration-cli")
SKIP_DIRS = {"target", "node_modules", ".venv", "dist", ".git", "gen", "__pycache__"}

HTTP_CRATES = {
    "reqwest", "hyper", "hyper-util", "ureq", "isahc", "curl", "surf", "attohttpc", "minreq",
    "awc", "tungstenite", "tokio-tungstenite", "rmcp", "octocrab", "ssh2", "russh", "async-ssh2-tokio",
    "tauri-plugin-http", "tauri-plugin-updater", "tauri-plugin-websocket", "tauri-plugin-upload",
}  # fmt: skip
SERVER_CRATES = {
    "tokio-tungstenite", "tungstenite", "async-tungstenite", "axum", "axum-server", "hyper-server",
    "warp", "actix", "actix-web", "actix-server", "actix-http", "tiny_http", "tide", "poem", "rocket",
    "salvo", "gotham", "simple-websockets", "ws",
}  # fmt: skip
RUST_CLIENT = re.compile(
    r"reqwest::(blocking|Client\s*::\s*(new|builder)|ClientBuilder|get\s*\()|\bClientBuilder::new\s*\("
)
PY_IMPORT = re.compile(
    r"^\s*(?:import|from)\s+(urllib\.request|urllib3|http\.client|http\.server|requests|httpx|aiohttp|"
    r"ftplib|smtplib|poplib|imaplib|telnetlib|xmlrpc|websocket|websockets|paramiko|pycurl)\b",
    re.M,
)
PY_SOCKET = re.compile(r"\bsocket\.(socket|create_connection|create_server|socketpair)\s*\(")
PY_PACKAGES = [
    "python/kernel/fibration_kernel",
    "python/kernel/fibration_mpl",
    "present/python/kernel/present_kernel",
    "present/python/sdk/present_sdk",
]
NPM_FORBIDDEN = {
    "@tauri-apps/plugin-http", "@tauri-apps/plugin-updater", "@tauri-apps/plugin-websocket",
    "@tauri-apps/plugin-upload", "axios", "node-fetch", "socket.io-client",
}  # fmt: skip


def walk(base: pathlib.Path, pattern: str):
    for path in sorted(base.rglob(pattern)):
        if not SKIP_DIRS.intersection(path.relative_to(ROOT).parts):
            yield path


def rel(path: pathlib.Path) -> str:
    return path.relative_to(ROOT).as_posix()


def dependency_tables(manifest: dict):
    for section in ("dependencies", "dev-dependencies", "build-dependencies"):
        yield section, manifest.get(section, {})
    for target, table in manifest.get("target", {}).items():
        for section in ("dependencies", "dev-dependencies", "build-dependencies"):
            yield f"target.{target}.{section}", table.get(section, {})


def check_manifests(bad: list[str]) -> None:
    for manifest in walk(ROOT, "Cargo.toml"):
        if manifest.parent == ROOT:
            continue
        in_gate = GATE in manifest.parents
        in_bridge = manifest.parent == BRIDGE
        data = tomllib.loads(manifest.read_text(encoding="utf-8"))
        for section, table in dependency_tables(data):
            for name, spec in table.items():
                real = spec.get("package", name) if isinstance(spec, dict) else name
                if real in SERVER_CRATES:
                    if not in_bridge:
                        bad.append(f"{rel(manifest)}: [{section}] depends on server crate `{real}`; only crates/serve may")
                elif real in HTTP_CRATES and not in_gate:
                    bad.append(f"{rel(manifest)}: [{section}] depends on `{real}`; only crates/wb-net may")
                if real == BRIDGE_CRATE and not in_bridge:
                    bad.append(f"{rel(manifest)}: [{section}] depends on the development bridge `{real}`")


def check_lockfile(bad: list[str]) -> None:
    """The shipped binaries must not link the bridge or any server crate, even transitively."""
    lock = ROOT / "Cargo.lock"
    if not lock.is_file():
        bad.append("Cargo.lock is missing (update tools/check_egress.py)")
        return
    packages = tomllib.loads(lock.read_text(encoding="utf-8")).get("package", [])
    deps: dict[str, set[str]] = {}
    for p in packages:
        deps.setdefault(p["name"], set()).update(d.split(" ")[0] for d in p.get("dependencies", []))
    for root in SHIPPED:
        if root not in deps:
            bad.append(f"Cargo.lock: `{root}` not found (update tools/check_egress.py)")
            continue
        seen, todo = set(), [root]
        while todo:
            name = todo.pop()
            if name in seen:
                continue
            seen.add(name)
            todo.extend(deps.get(name, ()))
        for forbidden in sorted(seen & (SERVER_CRATES | {BRIDGE_CRATE})):
            bad.append(f"Cargo.lock: `{root}` links `{forbidden}`; the shipped app must have no listener")


def check_rust(bad: list[str]) -> None:
    for source in walk(ROOT, "*.rs"):
        if GATE in source.parents:
            continue
        for n, line in enumerate(source.read_text(encoding="utf-8", errors="replace").splitlines(), 1):
            if line.lstrip().startswith("//"):
                continue
            if RUST_CLIENT.search(line):
                bad.append(f"{rel(source)}:{n}: builds an HTTP client; use wb_net::gate().request(..)")


def check_python(bad: list[str]) -> None:
    for package in PY_PACKAGES:
        base = ROOT / package
        if not base.is_dir():
            bad.append(f"{package}: expected Python package is missing (update tools/check_egress.py)")
            continue
        for source in walk(base, "*.py"):
            text = source.read_text(encoding="utf-8", errors="replace")
            for m in PY_IMPORT.finditer(text):
                n = text.count("\n", 0, m.start()) + 1
                bad.append(f"{rel(source)}:{n}: imports `{m.group(1)}`; shipped Python must not use the network")
            for m in PY_SOCKET.finditer(text):
                n = text.count("\n", 0, m.start()) + 1
                bad.append(f"{rel(source)}:{n}: opens a socket; shipped Python must not use the network")


def check_shells(bad: list[str]) -> None:
    confs = list(walk(ROOT, "tauri.conf.json"))
    if not confs:
        bad.append("no tauri.conf.json found (update tools/check_egress.py)")
    for conf in confs:
        data = json.loads(conf.read_text(encoding="utf-8"))
        security = data.get("app", {}).get("security", {})
        csp = security.get("csp")
        if not isinstance(csp, str) or not csp.strip():
            bad.append(f"{rel(conf)}: no release content security policy")
            continue
        for origin in re.findall(r"\b(?:https?|wss?)://[^\s;']+", csp):
            # Tauri's own in-process schemes are served from these pseudo-hosts on some platforms.
            if origin not in ("http://ipc.localhost", "http://asset.localhost"):
                bad.append(f"{rel(conf)}: release CSP allows `{origin}`")
        if re.search(r"(?:^|[\s;])\*(?:[\s;]|$)", csp):
            bad.append(f"{rel(conf)}: release CSP contains a wildcard source")
        if "updater" in data.get("plugins", {}):
            bad.append(f"{rel(conf)}: the updater plugin is configured")
    for package in walk(ROOT, "package.json"):
        data = json.loads(package.read_text(encoding="utf-8"))
        for section in ("dependencies", "devDependencies", "optionalDependencies"):
            for name in data.get(section, {}):
                if name in NPM_FORBIDDEN:
                    bad.append(f"{rel(package)}: [{section}] depends on `{name}`")


def main() -> int:
    bad: list[str] = []
    check_manifests(bad)
    check_lockfile(bad)
    check_rust(bad)
    check_python(bad)
    check_shells(bad)
    if bad:
        print("network egress outside the gate:\n  " + "\n  ".join(bad))
        return 1
    print("ok: only crates/wb-net can reach the network; only crates/serve (never shipped) listens")
    return 0


if __name__ == "__main__":
    sys.exit(main())
