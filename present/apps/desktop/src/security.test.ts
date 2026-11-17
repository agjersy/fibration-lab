import { describe, expect, it } from "vitest";
import capabilities from "../src-tauri/capabilities/default.json";
import conf from "../src-tauri/tauri.conf.json";

// The window of a release build may load and connect to the app itself only. The Vite dev
// server is allowed in `devCsp`, which Tauri applies to `tauri dev` alone.

function directives(csp: string): Map<string, string[]> {
  const out = new Map<string, string[]>();
  for (const part of csp.split(";")) {
    const [name, ...sources] = part.trim().split(/\s+/);
    if (name) out.set(name, sources);
  }
  return out;
}

describe("release security configuration", () => {
  const security = conf.app.security;

  it("has a release CSP with no dev server and no remote origin", () => {
    expect(security.csp).not.toMatch(/localhost:\d+/);
    expect(security.csp).not.toMatch(/\bwss?:/);
    expect(security.csp).not.toContain("https:");
    expect(security.csp).not.toContain("unsafe-eval");
    const d = directives(security.csp);
    expect(d.get("script-src")).toEqual(["'self'"]);
    expect(d.get("connect-src")).toEqual(["'self'", "ipc:", "http://ipc.localhost"]);
    expect(d.get("default-src")).toEqual(["'self'", "ipc:", "http://ipc.localhost"]);
    // The only http origins are Tauri's own IPC and asset hosts.
    const http = security.csp.match(/https?:\/\/[^\s;]+/g) ?? [];
    expect(new Set(http)).toEqual(new Set(["http://ipc.localhost", "http://asset.localhost"]));
  });

  it("keeps the dev server in devCsp only", () => {
    const dev = directives(security.devCsp);
    expect(dev.get("script-src")).toContain("http://localhost:5173");
    expect(dev.get("connect-src")).toContain("ws://localhost:5173");
    // Apart from the dev server the two policies are the same.
    const stripped = security.devCsp.replace(/ (http|ws):\/\/localhost:5173/g, "");
    expect(stripped).toBe(security.csp);
    expect(conf.build.devUrl).toBe("http://localhost:5173");
  });

  it("grants the window no permission the renderer does not use", () => {
    expect(capabilities.windows).toEqual(["main"]);
    expect(capabilities.permissions).toEqual([]);
    expect(conf.app.withGlobalTauri).toBe(false);
  });
});
