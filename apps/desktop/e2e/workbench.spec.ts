import type { Page } from "@playwright/test";
import { createSession, expect, expectNoErrors, test, trackErrors } from "./harness";

async function openDecaySim(page: Page) {
  await page.locator('[data-path="scripts"]').click();
  await page.locator('[data-path="scripts/decay_sim.py"]').click();
  const editor = page.locator('[data-testid="editor"][data-path="scripts/decay_sim.py"]');
  await expect(editor).toBeVisible();
  await expect(editor.locator(".cm-content")).toContainText("ArgumentParser");
  return editor;
}

test.describe("workbench (real core)", () => {
  test.beforeEach(({ page }) => trackErrors(page));
  test.afterEach(({ page }) => expectNoErrors(page));

  test("create a session, edit and save a script, run it and see the figure", async ({ page, bridge }) => {
    await createSession(page, bridge);
    const editor = await openDecaySim(page);
    const tab = page.locator(".dv-tab", { hasText: "decay_sim.py" });

    // Edit: append a comment on the first line and save with Ctrl+S.
    await editor.locator(".cm-content").click();
    await page.keyboard.press("ControlOrMeta+Home");
    await page.keyboard.press("End");
    await page.keyboard.type("  # edited in e2e");
    await expect(tab).toContainText("•");
    await page.keyboard.press("ControlOrMeta+s");
    await expect(tab).not.toContainText("•");
    await expect(editor.locator(".editor-banner")).toHaveCount(0);

    // Run from the Scripts form.
    await page.locator(".dv-tab", { hasText: "Scripts" }).click();
    const card = page.locator('[data-script="decay_sim"]');
    await card.locator(".script-header").click();
    await expect(card.getByLabel("--tau", { exact: false })).toHaveValue("1");
    await card.getByLabel("--tau", { exact: false }).fill("1.8");
    await card.getByRole("button", { name: "Run", exact: true }).click();

    const runOutput = page.getByTestId("run-output");
    await expect(runOutput).toBeVisible();
    await expect(runOutput.getByTestId("run-stdout")).toContainText('"half_life"', { timeout: 30_000 });
    await expect(runOutput).toContainText("exit 0", { timeout: 30_000 });
    await expect(page.getByTestId("figure-tab").filter({ hasText: "decay" })).toBeVisible();
    await expect(page.getByTestId("figure-frame").locator("img")).toBeVisible();
    await expect(card).toContainText("exit 0");
  });

  test("build-mode chat: review and accept a proposal, then run mode and terminal", async ({ page, bridge }) => {
    await createSession(page, bridge);
    const chat = page.getByTestId("chat");
    await expect(chat.getByTestId("mode-select")).toHaveValue("build");
    const input = chat.getByLabel("Message the agent");
    await input.fill("Add a double exponential model and check the residuals");
    await input.press("ControlOrMeta+Enter");
    await expect(chat.getByTestId("user-message")).toContainText("double exponential");
    await expect(chat.getByTestId("tool-card").first()).toBeVisible({ timeout: 15_000 });

    const proposal = chat.getByTestId("proposal-card");
    await expect(proposal).toBeVisible({ timeout: 20_000 });
    await expect(proposal).toContainText("analysis/models.py");
    await proposal.getByTestId("review-proposal").click();
    const review = page.getByTestId("proposal-review");
    await expect(review).toBeVisible();
    await expect(review.getByTestId("merge-editor").locator(".cm-content")).toContainText("double_exp", { timeout: 15_000 });
    await expect(review.locator(".cm-chunkButtons").first()).toBeVisible();
    await review.getByTestId("accept-all").click();
    await expect(review).toHaveCount(0);
    await expect(proposal).toContainText("accepted");

    await expect(chat.getByTestId("run-card")).toBeVisible({ timeout: 30_000 });
    await expect(chat.getByTestId("summary-card")).toBeVisible({ timeout: 30_000 });
    await expect(chat.getByTestId("summary-card")).toContainText("analysis/models.py");
    await expect(chat.getByTestId("run-card").locator(".thumb img").first()).toBeVisible({ timeout: 15_000 });
    await expect(page.getByTestId("figure-tab").filter({ hasText: "decay" })).toBeVisible();

    // Terminal: Ctrl+` opens one; run ls.
    await page.keyboard.press("Control+`");
    const term = page.getByTestId("xterm");
    await expect(term).toBeVisible({ timeout: 15_000 });
    await expect(term.locator(".xterm-rows")).toHaveText(/\S/, { timeout: 15_000 });
    await page.keyboard.type("ls");
    await page.keyboard.press("Enter");
    await expect(term.locator(".xterm-rows")).toContainText("scripts", { timeout: 15_000 });
    await expect(term.locator(".xterm-rows")).toContainText("notes.md");

    // Run mode: no proposals, two runs.
    await chat.getByTestId("mode-select").selectOption("run");
    // The select reflects the backend's session_changed, so this waits for the mode switch to land.
    await expect(chat.getByTestId("mode-select")).toHaveValue("run");
    await input.fill("Sweep tau over 1 and 3 and compare the fits");
    await chat.getByTestId("chat-send").click();
    await expect(chat.getByTestId("run-card")).toHaveCount(3, { timeout: 60_000 });
    await expect(chat.getByTestId("summary-card")).toHaveCount(2, { timeout: 30_000 });
    await expect(chat.getByTestId("proposal-card")).toHaveCount(1);
  });

  test("command palette lists bindings and the file finder opens files", async ({ page, bridge }) => {
    await createSession(page, bridge);
    await page.keyboard.press("ControlOrMeta+Shift+P");
    const palette = page.getByTestId("palette");
    await expect(palette).toBeVisible();
    await expect(palette.getByRole("option", { name: /Toggle left dock/ })).toContainText("Ctrl+B");
    await page.keyboard.press("Escape");
    await expect(palette).toHaveCount(0);
    await page.keyboard.press("ControlOrMeta+p");
    await page.getByLabel("Palette query").fill("models");
    await expect(palette.getByRole("option", { name: /analysis\/models\.py/ })).toBeVisible();
    await page.keyboard.press("Enter");
    await expect(page.locator('[data-testid="editor"][data-path="analysis/models.py"] .cm-content')).toContainText("single_exp");
    await page.locator(".dv-tab", { hasText: "Equations" }).click();
    await expect(page.getByTestId("equations-panel")).toContainText("single_exp");
    await expect(page.getByTestId("equations-panel").locator(".katex").first()).toBeVisible({ timeout: 30_000 });
  });
});
