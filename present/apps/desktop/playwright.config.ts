import { existsSync } from "node:fs";
import { defineConfig } from "@playwright/test";

// A preinstalled Chromium (CI images, sandboxes) is used when the version Playwright expects is not
// downloaded: set PW_CHROMIUM_PATH, or provide /opt/pw-browsers/chromium.
const preinstalled = process.env.PW_CHROMIUM_PATH ?? (existsSync("/opt/pw-browsers/chromium") ? "/opt/pw-browsers/chromium" : undefined);

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
  },
  projects: [{ name: "chromium", use: { browserName: "chromium", ...(preinstalled ? { launchOptions: { executablePath: preinstalled } } : {}) } }],
  webServer: {
    command: "pnpm vite preview --host 127.0.0.1 --port 4173 --strictPort",
    url: "http://127.0.0.1:4173",
    reuseExistingServer: false,
    timeout: 30_000,
  },
});
