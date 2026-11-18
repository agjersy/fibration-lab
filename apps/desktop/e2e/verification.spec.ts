// Human verification in the one window (mock backend): mark an equation verified, see a code
// change reset it, and get the export warning for a deck that depends on unverified items.
import { expect, test } from "@playwright/test";
import type { Page } from "@playwright/test";

const REPO = "/home/user/projects/beam-sim";
const TEMPLATE = "/home/user/templates/corp.potx";

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
  await expect(page.getByTestId("workbench")).toBeVisible();
}

async function palette(page: Page, command: string) {
  await page.keyboard.press("ControlOrMeta+Shift+P");
  await page.getByLabel("Palette query").fill(command);
  await page.getByTestId("palette").getByRole("option", { name: new RegExp(`^${command}`) }).first().click();
}

async function openModels(page: Page) {
  await page.keyboard.press("ControlOrMeta+p");
  await page.getByLabel("Palette query").fill("models");
  await page.getByTestId("palette").getByRole("option", { name: /analysis\/models\.py/ }).click();
  const editor = page.locator('[data-testid="editor"][data-path="analysis/models.py"]');
  await expect(editor.locator(".cm-content")).toContainText("single_exp");
  return editor;
}

async function showEquations(page: Page) {
  await page.locator(".dv-tab", { hasText: "Equations" }).click();
  const row = page.getByTestId("equation-single_exp");
  await expect(row).toBeVisible({ timeout: 15_000 });
  return row;
}

test.describe("verification (mock backend)", () => {
  test.beforeEach(async ({ page }) => {
    const errors: string[] = [];
    page.on("pageerror", (e) => errors.push(e.message));
    page.on("console", (m) => {
      if (m.type() === "error") errors.push(m.text());
    });
    (page as Page & { __errors?: string[] }).__errors = errors;
  });

  test.afterEach(async ({ page }) => {
    const errors = (page as Page & { __errors?: string[] }).__errors ?? [];
    expect(errors, "no console errors or page errors").toEqual([]);
  });

  test("mark an equation verified; a code change resets it to unverified", async ({ page }) => {
    await createSession(page);
    const editor = await openModels(page);
    const row = await showEquations(page);
    await expect(row.getByTestId("equation-verify-state")).toHaveText("unverified");
    await row.getByTestId("equation-verify").click();
    await expect(row.getByTestId("equation-verify-state")).toHaveText("verified");
    await expect(row.getByTestId("equation-verify")).toHaveCount(0);

    // The code changes: the verification was for the old content.
    await editor.locator(".cm-content").click();
    await page.keyboard.press("ControlOrMeta+End");
    await page.keyboard.type("\n# tweak\n");
    await page.keyboard.press("ControlOrMeta+s");
    await expect(row.getByTestId("equation-verify-state")).toHaveText("unverified", { timeout: 10_000 });

    // The queue lists it again, marked as changed since it was verified; `v` verifies it from the keyboard.
    await palette(page, "Show verification queue");
    const queue = page.getByTestId("verify-panel");
    const item = queue.getByTestId("verify-item").filter({ hasText: "single_exp in analysis/models.py" });
    await expect(item).toHaveAttribute("data-state", "unverified");
    await expect(item).toContainText("changed");
    await item.click();
    await queue.getByTestId("verify-list").focus();
    await page.keyboard.press("v");
    await expect(queue.getByTestId("verify-item").filter({ hasText: "single_exp in analysis/models.py" })).toHaveCount(0);
    await expect(queue).toContainText("1 verified");
  });

  test("exporting a deck that depends on unverified items asks first", async ({ page }) => {
    await createSession(page);
    await openModels(page);
    // A deck with a slide.
    await page.locator(".dv-tab", { hasText: "Decks" }).click();
    await page.getByTestId("decks-view").getByTestId("new-deck").click();
    const dialog = page.getByRole("dialog", { name: "New deck" });
    await dialog.getByLabel("Title").fill("Q3 Review");
    await dialog.getByLabel("Template").fill(TEMPLATE);
    await dialog.getByTestId("create-deck").click();
    await expect(page.getByTestId("canvas")).toBeVisible();
    const rail = page.getByTestId("slides-view");
    await rail.getByTestId("add-slide").click();
    await expect(rail.getByTestId("rail-item")).toHaveCount(1);

    // The equation goes on the slide with its badge; clicking it opens the Equations panel at the function.
    const row = await showEquations(page);
    await row.getByTestId("equation-place").click();
    const canvas = page.getByTestId("slide-canvas");
    await expect(canvas.getByTestId("slide-equation")).toBeVisible();
    await expect(canvas.getByTestId("equation-badge")).toHaveText("declared ✓");
    await expect(canvas.getByTestId("slide-verify-badge")).toHaveAttribute("data-state", "unverified");
    await canvas.getByTestId("equation-badge").click();
    await expect(page.getByTestId("equation-single_exp")).toHaveAttribute("data-focused", "true");

    // Export: a confirm dialog lists what the deck rests on that nobody verified.
    await palette(page, "Show deck exports");
    const exports = page.getByTestId("exports-panel");
    await exports.getByTestId("export-pptx").click();
    const confirm = page.getByTestId("confirm-dialog");
    await expect(confirm).toBeVisible();
    await expect(confirm.getByTestId("confirm-item")).toHaveCount(2);
    await expect(confirm).toContainText("single_exp in analysis/models.py");
    await page.getByTestId("confirm-cancel").click();
    await expect(confirm).toHaveCount(0);
    await expect(exports.getByTestId("export-item")).toHaveCount(0);

    await exports.getByTestId("export-pptx").click();
    await page.getByTestId("confirm-ok").click();
    await expect(exports.getByTestId("export-item")).toContainText("exports/q3-review.pptx", { timeout: 15_000 });

    // Once everything is verified, the export goes through without asking.
    await palette(page, "Show verification queue");
    const queue = page.getByTestId("verify-panel");
    const onSlides = queue.locator('[data-kind="slide_claim"], [data-kind="equation"]');
    await expect(onSlides).toHaveCount(2);
    await queue.getByTestId("verify-list").focus();
    await page.keyboard.press("v");
    await expect(onSlides).toHaveCount(1);
    await page.keyboard.press("v");
    await expect(onSlides).toHaveCount(0);
    await page.locator(".dv-tab", { hasText: "Canvas" }).click();
    await expect(canvas.getByTestId("slide-verify-badge")).toHaveAttribute("data-state", "human_verified");
    await palette(page, "Show deck exports");
    await exports.getByTestId("export-md").click();
    await exports.getByTestId("export-pdf").click();
    await expect(page.getByTestId("confirm-dialog")).toHaveCount(0);
  });

  test("refresh a stale equation from the analysis through a normal proposal", async ({ page }) => {
    await createSession(page);
    const editor = await openModels(page);
    await page.locator(".dv-tab", { hasText: "Decks" }).click();
    await page.getByTestId("decks-view").getByTestId("new-deck").click();
    const dialog = page.getByRole("dialog", { name: "New deck" });
    await dialog.getByLabel("Title").fill("Q3 Review");
    await dialog.getByLabel("Template").fill(TEMPLATE);
    await dialog.getByTestId("create-deck").click();
    const rail = page.getByTestId("slides-view");
    await rail.getByTestId("add-slide").click();
    await expect(rail.getByTestId("rail-item")).toHaveCount(1);
    const row = await showEquations(page);
    await row.getByTestId("equation-place").click();
    await expect(page.getByTestId("slide-canvas").getByTestId("slide-equation")).toBeVisible();

    // The source of the equation changes: the checker reports it stale.
    await page.locator(".dv-tab", { hasText: "models.py" }).click();
    await editor.locator(".cm-content").click();
    await page.keyboard.press("ControlOrMeta+End");
    await page.keyboard.type("\n# tweak\n");
    await page.keyboard.press("ControlOrMeta+s");
    await palette(page, "Show deck checks");
    const checks = page.getByTestId("checks-panel");
    await checks.getByTestId("run-deck-checks").click();
    await expect(checks.locator('[data-testid="check-row"][data-rule="stale"]')).toHaveCount(1);
    const canvas = page.getByTestId("slide-canvas");
    await page.locator(".dv-tab", { hasText: "Canvas" }).click();
    await expect(canvas.getByTestId("element-refresh")).toBeVisible();

    // Refresh all stale: a before/after card in the chat listing what changes.
    await checks.getByTestId("refresh-all-stale").click();
    const card = page.getByTestId("chat").getByTestId("slide-proposal-card");
    await expect(card).toBeVisible({ timeout: 10_000 });
    await expect(card.getByTestId("proposal-message")).toContainText("equation of single_exp");
    await expect(card.getByTestId("proposal-message")).toContainText("Invalidates");
    await expect(card.getByTestId("slide-before")).toBeVisible();
    await card.getByTestId("proposal-accept").click();
    await expect(card).toContainText("accepted");
    await checks.getByTestId("run-deck-checks").click();
    await expect(checks.locator('[data-testid="check-row"][data-rule="stale"]')).toHaveCount(0);
  });
});
