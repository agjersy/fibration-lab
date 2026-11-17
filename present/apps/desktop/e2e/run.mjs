// Runs Playwright with the arguments given to `pnpm test:e2e`. pnpm 10 forwards a literal "--"
// (`pnpm test:e2e -- --trace off`), which Playwright would read as the start of file filters.
import { spawnSync } from "node:child_process";

const args = process.argv.slice(2).filter((a, i) => !(i === 0 && a === "--"));
const result = spawnSync("playwright", ["test", ...args], { stdio: "inherit", shell: process.platform === "win32" });
process.exit(result.status ?? 1);
