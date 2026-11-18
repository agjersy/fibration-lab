import { expect, test } from "@playwright/test";
import type { Page } from "@playwright/test";

const REPO = "/home/user/projects/beam-sim";

async function createSession(page: Page) {
  await page.goto("/");
  await expect(page.getByTestId("topbar")).toBeVisible();
  await page.getByTestId("new-session").click();
  await page.getByLabel("Repository").fill(REPO);
  await page.getByLabel("Target").selectOption("local");
  await page.getByLabel("Model").selectOption({ index: 1 });
  await page.getByText("Initialize as an analysis repo").click();
  await page.getByTestId("create-session").click();
  const tab = page.getByTestId("session-tab").filter({ hasText: "beam-sim" });
  await expect(tab.locator(".dot.ready")).toBeVisible({ timeout: 15_000 });
  await expect(page.getByRole("dialog")).toHaveCount(0);
}

test.describe("simplification round (mock backend)", () => {
  test.beforeEach(async ({ page }) => {
    const errors: string[] = [];
    page.on("pageerror", (e) => errors.push(e.message));
    page.on("console", (m) => {
      if (m.type() === "error") errors.push(m.text());
    });
    (page as Page & { __errors?: string[] }).__errors = errors;
  });

  test.afterEach(async ({ page }) => {
    expect((page as Page & { __errors?: string[] }).__errors ?? [], "no console errors or page errors").toEqual([]);
  });

  test("settings: three policy presets, with the detailed controls under Advanced", async ({ page }) => {
    await createSession(page);
    await page.getByRole("button", { name: "Settings" }).click();
    const dialog = page.getByRole("dialog", { name: "Settings" });
    await expect(dialog.getByTestId("policy-presets").getByRole("radio")).toHaveCount(3);
    // The detailed controls are folded away until asked for.
    await expect(dialog.locator("#st-packages")).toBeHidden();

    await dialog.getByTestId("policy-preset-read-only").click();
    await expect(dialog.getByTestId("policy-preset-read-only")).toBeChecked();
    await dialog.getByTestId("policy-advanced").locator("summary").click();
    await expect(dialog.locator("#st-edits")).toHaveValue("deny");
    await expect(dialog.locator("#st-packages")).toHaveValue("deny");
    await expect(dialog.locator("#st-shell")).toHaveValue("deny");
    await expect(dialog.locator("#st-git")).toHaveValue("deny");
    await expect(dialog.locator("#st-runs")).toHaveValue("auto");

    await dialog.getByTestId("policy-preset-auto").click();
    await expect(dialog.locator("#st-edits")).toHaveValue("auto");
    await expect(dialog.locator("#st-packages")).toHaveValue("ask");

    // A detailed key still overrides; the dialog then names no preset.
    await dialog.locator("#st-edits").selectOption("auto-new-files");
    await expect(dialog.getByTestId("policy-custom")).toBeVisible();
    await expect(dialog.getByTestId("policy-presets").getByRole("radio", { checked: true })).toHaveCount(0);
    await dialog.getByTestId("policy-preset-review").click();
    await expect(dialog.locator("#st-edits")).toHaveValue("review");
    await expect(dialog.getByTestId("policy-custom")).toHaveCount(0);
  });

  test("figures: one tab per figure, shown as a PNG; the package list is cached", async ({ page }) => {
    await createSession(page);
    await page.locator(".dv-tab", { hasText: "Scripts" }).click();
    const card = page.locator('[data-script="decay_sim"]');
    await card.locator(".script-header").click();
    await card.getByRole("button", { name: "Run", exact: true }).click();
    await expect(page.getByTestId("run-output")).toContainText("exit 0", { timeout: 15_000 });
    await expect(page.getByTestId("figure-tab")).toHaveCount(1);
    const img = page.getByTestId("figure-frame").locator("img");
    await expect(img).toBeVisible();
    await expect(img).toHaveAttribute("src", /^data:image\/png;base64,/);
    // The image decodes: the mock's PNG is a real one.
    await expect.poll(() => img.evaluate((el) => (el as unknown as { naturalWidth: number }).naturalWidth)).toBe(640);
    // A single format: no alternates offered.
    await expect(page.getByTestId("figure-format")).toHaveCount(0);

    await page.locator(".dv-tab", { hasText: "Packages" }).click();
    const packages = page.getByTestId("packages-panel");
    await expect(packages.locator("tbody tr").first()).toBeVisible({ timeout: 15_000 });
    const rows = await packages.locator("tbody tr").count();
    // Leaving and coming back shows the cached list at once.
    await page.locator(".dv-tab", { hasText: "Figures" }).click();
    await page.locator(".dv-tab", { hasText: "Packages" }).click();
    await expect(packages.locator("tbody tr")).toHaveCount(rows);
    await packages.getByTestId("packages-refresh").click();
    await expect(packages.getByTestId("packages-refresh")).toHaveText("Refresh");
    await expect(packages.locator("tbody tr")).toHaveCount(rows);
  });
});
