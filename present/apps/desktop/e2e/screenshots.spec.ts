// Saves a screenshot of the canvas for each of the eight mock slides into e2e/screenshots/
// (gitignored) for visual review, plus the whole workbench in both themes.
import { mkdirSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { expect, test } from "@playwright/test";

const OUT = join(dirname(fileURLToPath(import.meta.url)), "screenshots");

test("screenshots of the eight mock slides", async ({ page }) => {
  mkdirSync(OUT, { recursive: true });
  await page.goto("/");
  const items = page.getByTestId("rail-item");
  await expect(items).toHaveCount(8);
  for (let i = 0; i < 8; i++) {
    await items.nth(i).click();
    await expect(page.getByTestId("slide-position")).toHaveText(`Slide ${i + 1} of 8`);
    const canvas = page.getByTestId("slide-canvas");
    await expect(canvas).toBeVisible();
    await page.waitForTimeout(150);
    await canvas.screenshot({ path: join(OUT, `slide-${i + 1}.png`) });
  }
  await items.nth(1).click();
  await page.screenshot({ path: join(OUT, "workbench-light.png") });
  await page.emulateMedia({ colorScheme: "dark" });
  await page.waitForTimeout(100);
  await page.screenshot({ path: join(OUT, "workbench-dark.png") });
});
