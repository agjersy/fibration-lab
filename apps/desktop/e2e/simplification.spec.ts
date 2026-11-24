import { createSession, expect, expectNoErrors, test, trackErrors } from "./harness";

test.describe("simplification round (real core)", () => {
  test.beforeEach(({ page }) => trackErrors(page));
  test.afterEach(({ page }) => expectNoErrors(page));

  test("settings: three policy presets, with the detailed controls under Advanced", async ({ page, bridge }) => {
    await createSession(page, bridge);
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

  test("figures: one tab per figure, shown as a PNG; the package list is cached", async ({ page, bridge }) => {
    await createSession(page, bridge);
    await page.locator(".dv-tab", { hasText: "Scripts" }).click();
    const card = page.locator('[data-script="decay_sim"]');
    await card.locator(".script-header").click();
    await card.getByRole("button", { name: "Run", exact: true }).click();
    await expect(page.getByTestId("run-output")).toContainText("exit 0", { timeout: 30_000 });
    await expect(page.getByTestId("figure-tab")).toHaveCount(1);
    const img = page.getByTestId("figure-frame").locator("img");
    await expect(img).toBeVisible();
    await expect(img).toHaveAttribute("src", /^data:image\/png;base64,/);
    // The image decodes: matplotlib's default 6.4 in at 100 dpi.
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
