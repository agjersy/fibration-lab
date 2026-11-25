import { existsSync, readdirSync } from "node:fs";
import { join } from "node:path";
import { defineConfig } from "@playwright/test";

/**
 * CI runs `playwright install chromium`, so the bundled browser is used and
 * this returns undefined. Sandboxes that ship a different Chromium build can
 * point at it with PLAYWRIGHT_CHROMIUM_EXECUTABLE; failing that, a Chromium
 * found under PLAYWRIGHT_BROWSERS_PATH (or /opt/pw-browsers) is used when the
 * build this Playwright version expects is not installed there.
 */
function chromiumExecutable(): string | undefined {
  const explicit = process.env.PLAYWRIGHT_CHROMIUM_EXECUTABLE;
  if (explicit) return explicit;
  const root = process.env.PLAYWRIGHT_BROWSERS_PATH ?? "/opt/pw-browsers";
  if (!existsSync(root)) return undefined;
  let names: string[];
  try {
    names = readdirSync(root);
  } catch {
    return undefined;
  }
  // Playwright 1.56.1 pairs with chromium build 1194; when it is present the default lookup works.
  if (names.some((n) => n === "chromium-1194" || n === "chromium_headless_shell-1194")) return undefined;
  const candidates = [
    ...names
      .filter((n) => /^chromium-\d+$/.test(n))
      .sort()
      .reverse()
      .flatMap((n) => [join(root, n, "chrome-linux64", "chrome"), join(root, n, "chrome-linux", "chrome")]),
    join(root, "chromium"),
  ];
  return candidates.find((p) => existsSync(p));
}

const executablePath = chromiumExecutable();

// The specs run the built bundle (`pnpm build` first) against the real core: e2e/harness.ts
// starts one `fibration-serve` per worker (from `target/debug`, or FIBRATION_BIN_DIR) with the
// scripted model server, and resets the fixture projects before each test.

export default defineConfig({
  testDir: "./e2e",
  timeout: 60_000,
  expect: { timeout: 10_000 },
  fullyParallel: false,
  workers: 1,
  retries: 0,
  reporter: [["list"]],
  use: {
    baseURL: "http://127.0.0.1:4173",
    headless: true,
    viewport: { width: 1440, height: 900 },
    trace: "retain-on-failure",
    ...(executablePath ? { launchOptions: { executablePath } } : {}),
  },
  projects: [{ name: "chromium", use: { browserName: "chromium" } }],
  webServer: {
    command: "pnpm vite preview --host 127.0.0.1 --port 4173 --strictPort",
    url: "http://127.0.0.1:4173",
    reuseExistingServer: false,
    timeout: 30_000,
  },
});
