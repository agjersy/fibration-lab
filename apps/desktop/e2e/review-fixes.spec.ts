// End-to-end coverage for the defects of docs/REVIEW.md that only show in the
// assembled app, against the real core through analyst-serve.
import type { Page } from "@playwright/test";
import { createSession, expect, expectNoErrors, gotoApp, test, trackErrors } from "./harness";
import type { Bridge } from "./harness";

/** A closed session with one stored run-mode turn (a run of the decay simulation), made through the real commands. */
async function seedClosedSession(bridge: Bridge): Promise<void> {
  const info = await bridge.call<{ id: string; active_conversation: string }>("session_create", { repo_abs: bridge.repo("decay-analysis") });
  const session = info.id;
  const conversation = info.active_conversation;
  await bridge.call("chat_set_mode", { session, conversation, mode: "run" });
  await bridge.call("chat_send", { session, conversation, text: "Run the decay simulation at tau = 2 and check the fit." });
  await expect
    .poll(
      async () => {
        const s = await bridge.call<{ conversations: { id: string; busy: boolean; turns: number }[] }>("session_info", { session });
        const c = s.conversations.find((x) => x.id === conversation);
        return c !== undefined && !c.busy && c.turns >= 1;
      },
      { timeout: 60_000 },
    )
    .toBe(true);
  await bridge.call("session_close", { session });
}

async function openFileFromFinder(page: Page, query: string, path: string) {
  await page.keyboard.press("ControlOrMeta+p");
  await page.getByLabel("Palette query").fill(query);
  await page.getByTestId("palette").getByRole("option", { name: new RegExp(path.replace(/[.*+?^${}()|[\]\\/]/g, "\\$&")) }).click();
  const editor = page.locator(`[data-testid="editor"][data-path="${path}"]`);
  await expect(editor).toBeVisible();
  return editor;
}

test.describe("review fixes (real core)", () => {
  test.beforeEach(({ page }) => trackErrors(page));
  test.afterEach(async ({ page }) => {
    await expectNoErrors(page);
    await expect(page.locator(".toast.error")).toHaveCount(0);
  });

  test("B2/U5/U8: opening the seeded closed session works on the first click", async ({ page, bridge }) => {
    await seedClosedSession(bridge);
    await gotoApp(page, bridge);
    await expect(page.getByTestId("welcome")).toBeVisible();
    const tab = page.getByTestId("session-tab").filter({ hasText: "decay-analysis" });
    await expect(tab).toBeVisible();
    await tab.click();

    // One click: the workbench is up before the kernel is, and the event channel works.
    await expect(page.getByTestId("workbench")).toBeVisible();
    await expect(page.locator('[data-path="scripts"]')).toBeVisible();
    await expect(tab.locator(".dot.ready")).toBeVisible({ timeout: 15_000 });

    // The stored conversation is back: user message, tool call, run card with its figure, summary.
    const chat = page.getByTestId("chat");
    await expect(chat.getByTestId("user-message")).toContainText("decay simulation at tau = 2");
    await expect(chat.getByTestId("tool-card")).toHaveCount(1);
    await expect(chat.getByTestId("run-card")).toHaveCount(1);
    await expect(chat.getByTestId("run-card").locator(".thumb img")).toBeVisible();
    await expect(chat.getByTestId("summary-card")).toContainText("the fit agrees");
    await expect(chat.getByTestId("mode-select")).toHaveValue("run");

    // Runs and figures loaded without waiting for the kernel; the index followed it.
    await expect(page.getByTestId("figure-tab").filter({ hasText: "decay" })).toBeVisible();
    await page.locator(".dv-tab", { hasText: "Runs" }).click();
    await expect(page.locator("[data-run]")).toHaveCount(1);
    await page.locator("[data-run]").click();
    await expect(page.getByTestId("run-stdout")).toContainText('"half_life"');
    await page.locator(".dv-tab", { hasText: "Scripts" }).click();
    await expect(page.locator('[data-script="decay_sim"]')).toBeVisible();

    // Events flow: a run started now streams and finishes.
    await page.locator('[data-script="decay_sim"] .script-header').click();
    await page.locator('[data-script="decay_sim"]').getByRole("button", { name: "Run", exact: true }).click();
    await expect(page.getByTestId("run-output")).toContainText("exit 0", { timeout: 30_000 });

    // Close and reopen: still one click.
    await tab.getByRole("button", { name: /Close session/ }).click();
    await expect(page.getByTestId("welcome")).toBeVisible();
    await tab.click();
    await expect(page.getByTestId("workbench")).toBeVisible();
    await expect(tab.locator(".dot.ready")).toBeVisible({ timeout: 15_000 });
    await expect(page.getByTestId("chat").getByTestId("user-message")).toContainText("decay simulation");
  });

  test("B9: run form with positional, choices, boolean, count and required options", async ({ page, bridge }) => {
    await createSession(page, bridge);
    await page.locator(".dv-tab", { hasText: "Scripts" }).click();
    const card = page.locator('[data-script="fit_decay"]');
    await card.locator(".script-header").click();
    const command = card.getByTestId("command-line");
    await expect(command).toHaveText("python scripts/fit_decay.py");

    // Required fields are validated before anything runs.
    await card.getByRole("button", { name: "Run", exact: true }).click();
    await expect(card.getByTestId("form-errors")).toBeVisible();
    await expect(card.getByRole("alert").filter({ hasText: "required" })).toHaveCount(2);
    await page.locator(".dv-tab", { hasText: "Runs" }).click();
    await expect(page.locator("[data-run]")).toHaveCount(0);
    await page.locator(".dv-tab", { hasText: "Scripts" }).click();

    await card.getByLabel("input", { exact: false }).first().fill("data/sample.csv");
    await card.getByLabel("--label").fill("run 3");
    // Choices show an explicit default entry.
    const model = card.getByLabel("--model");
    await expect(model).toHaveValue("");
    await expect(model.locator("option").first()).toHaveText("(default: single)");
    await model.selectOption("double");
    // store_false is reachable, BooleanOptionalAction is tri-state, count repeats the flag.
    await card.getByLabel("--no-plot").check();
    await card.getByLabel("--weights").selectOption("no");
    await card.getByLabel("--verbose", { exact: false }).fill("2");
    await expect(card.getByLabel("--bounds", { exact: false })).toHaveValue("0.1 10");
    await card.getByLabel("extra arguments").fill('--note "two words"');
    await expect(command).toHaveText("python scripts/fit_decay.py --model double --label 'run 3' --no-plot --no-weights --verbose --verbose data/sample.csv --note 'two words'");
    await expect(card.getByTestId("form-errors")).toHaveCount(0);

    await card.getByRole("button", { name: "Run", exact: true }).click();
    const stdout = page.getByTestId("run-stdout");
    await expect(stdout).toContainText('argv: ["--model","double","--label","run 3","--no-plot","--no-weights","--verbose","--verbose","data/sample.csv","--note","two words"]', { timeout: 15_000 });
    await expect(page.getByTestId("run-output")).toContainText("exit 0", { timeout: 30_000 });

    // Two scripts open at once: every field has its own label target.
    const other = page.locator('[data-script="decay_sim"]');
    await other.locator(".script-header").click();
    await other.getByLabel("--tau", { exact: false }).fill("1.1");
    await expect(other.getByTestId("command-line")).toHaveText("python scripts/decay_sim.py --tau 1.1");
    const ids = await page.locator(".script-body input, .script-body select").evaluateAll((els) => els.map((e) => e.id));
    expect(new Set(ids).size).toBe(ids.length);
  });

  test("proposal review: rejecting one chunk writes only the accepted change", async ({ page, bridge }) => {
    await createSession(page, bridge);
    const chat = page.getByTestId("chat");
    const input = chat.getByLabel("Message the agent");
    await input.fill("Add a double exponential model");
    await input.press("ControlOrMeta+Enter");
    const proposal = chat.getByTestId("proposal-card");
    await expect(proposal).toBeVisible({ timeout: 20_000 });
    await proposal.getByTestId("review-proposal").click();
    const review = page.getByTestId("proposal-review");
    await expect(review.getByTestId("merge-editor").locator(".cm-content")).toContainText("double_exp", { timeout: 15_000 });
    await expect(review).toContainText("2 pending chunks");

    // Reject the first chunk (the new function); keep the docstring change.
    await review.locator(".cm-chunkButtons").first().locator('button[name="reject"]').click();
    await expect(review).toContainText("1 pending chunk");
    await expect(review.getByTestId("merge-editor").locator(".cm-content")).not.toContainText("def double_exp");

    // Accepting from the chat card goes through the open review tab: the reviewed text is what gets written.
    await proposal.getByTestId("proposal-accept").click();
    await expect(review).toHaveCount(0);
    await expect(proposal).toContainText("accepted");
    await expect(chat).toContainText("Applied your edited version", { timeout: 15_000 });
    await expect(chat.getByTestId("summary-card")).toBeVisible({ timeout: 30_000 });

    const editor = await openFileFromFinder(page, "models", "analysis/models.py");
    await expect(editor.locator(".cm-content")).toContainText("(observed minus fit)");
    await expect(editor.locator(".cm-content")).not.toContainText("double_exp");
    await expect(editor.locator(".cm-content")).toContainText("def single_exp");
  });

  test("B13: a focused terminal receives Ctrl+R and friends; app shortcuts stay off", async ({ page, bridge }) => {
    await createSession(page, bridge);
    await page.keyboard.press("Control+`");
    const term = page.getByTestId("xterm");
    await expect(term.locator(".xterm-rows")).toHaveText(/\S/, { timeout: 15_000 });
    await term.click();

    // Ctrl+R reaches readline instead of toggling the right dock.
    await page.keyboard.press("Control+r");
    await expect(term.locator(".xterm-rows")).toContainText("(reverse-i-search)", { timeout: 10_000 });
    await expect(page.getByTestId("chat")).toBeVisible();
    // Ctrl+B, Ctrl+P, Ctrl+S, Ctrl+G, Ctrl+W, Ctrl+L, Ctrl+J do not toggle docks, open the palette, move focus or close anything.
    for (const key of ["b", "p", "s", "g", "w", "l", "j"]) await page.keyboard.press(`Control+${key}`);
    // Ctrl+S reached the real terminal as XOFF; Ctrl+Q (XON) lets its output flow again.
    await page.keyboard.press("Control+q");
    await expect(page.locator('[data-path="scripts"]')).toBeVisible();
    await expect(term).toBeVisible();
    await expect(page.getByTestId("palette")).toHaveCount(0);
    await expect(page.getByTestId("chat").getByLabel("Message the agent")).not.toBeFocused();
    await expect(term.locator(".xterm-helper-textarea")).toBeFocused();
    // Ctrl+J was a newline for the shell; let it print its prompt before typing on.
    await page.waitForTimeout(400);
    await page.keyboard.type("echo still-here");
    await page.keyboard.press("Enter");
    await expect(term.locator(".xterm-rows")).toContainText("still-here");

    // The allow-list still works from inside the terminal.
    await page.keyboard.press("Control+Shift+P");
    await expect(page.getByTestId("palette")).toBeVisible();
    // While the palette (a modal) is open, other shortcuts are suppressed.
    await page.keyboard.press("Control+b");
    await page.keyboard.press("Escape");
    await expect(page.getByTestId("palette")).toHaveCount(0);
    await expect(page.locator('[data-path="scripts"]')).toBeVisible();

    // U7: a second terminal does not destroy the first one's screen.
    await page.keyboard.press("Control+`");
    await expect(page.locator(".terminal-tab")).toHaveCount(2);
    await expect(page.getByTestId("xterm").locator(".xterm-rows")).not.toContainText("still-here");
    await page.locator(".terminal-tab", { hasText: "Terminal 1" }).click();
    await expect(page.getByTestId("xterm").locator(".xterm-rows")).toContainText("still-here");

    // Outside the terminal the same chord is the app's again.
    await page.locator('[data-path="notes.md"]').click();
    await expect(page.locator('[data-testid="editor"][data-path="notes.md"] .cm-content')).toContainText("Notes and decisions");
    await page.keyboard.press("Control+r");
    await expect(page.getByTestId("chat")).toBeHidden();
  });

  test("B8/U6/U7: undo after open, unsaved-changes guard on tab close, expanded folders survive file changes", async ({ page, bridge }) => {
    await createSession(page, bridge);
    await page.locator('[data-path="scripts"]').click();
    await page.locator('[data-path="scripts/decay_sim.py"]').click();
    const editor = page.locator('[data-testid="editor"][data-path="scripts/decay_sim.py"]');
    const tab = page.locator(".dv-tab", { hasText: "decay_sim.py" });
    await expect(editor.locator(".cm-content")).toContainText("ArgumentParser");

    // Undo right after opening must not empty the file.
    await editor.locator(".cm-content").click();
    await page.keyboard.press("ControlOrMeta+z");
    await page.keyboard.press("ControlOrMeta+z");
    await expect(editor.locator(".cm-content")).toContainText("ArgumentParser");
    await expect(tab).not.toContainText("•");

    // Edit, then try to close: the guard asks; cancel keeps the tab and the edit.
    await page.keyboard.press("ControlOrMeta+Home");
    await page.keyboard.type("# draft\n");
    await expect(tab).toContainText("•");
    await tab.getByRole("button", { name: "Close tab" }).click();
    await expect(page.getByRole("dialog", { name: /Close decay_sim.py/ })).toBeVisible();
    // Shortcuts are suppressed while the dialog is open.
    await page.keyboard.press("ControlOrMeta+b");
    await expect(page.locator('[data-path="scripts"]')).toBeVisible();
    await page.getByTestId("unsaved-cancel").click();
    await expect(editor.locator(".cm-content")).toContainText("# draft");

    // Saving a .py file emits files_changed: the expanded scripts folder keeps its children.
    await page.keyboard.press("ControlOrMeta+s");
    await expect(tab).not.toContainText("•");
    await expect(page.locator('.tree-row[data-path="scripts/decay_sim.py"]')).toBeVisible();
    await expect(page.locator('[data-path="scripts/fit_decay.py"]')).toBeVisible();

    // Edit again and discard: the tab closes and the buffer is released, so reopening shows the saved text.
    await editor.locator(".cm-content").click();
    await page.keyboard.press("ControlOrMeta+Home");
    await page.keyboard.type("# throwaway\n");
    await page.keyboard.press("ControlOrMeta+w");
    await page.getByTestId("unsaved-discard").click();
    await expect(editor).toHaveCount(0);
    await page.locator('.tree-row[data-path="scripts/decay_sim.py"]').click();
    await expect(editor.locator(".cm-content")).toContainText("# draft");
    await expect(editor.locator(".cm-content")).not.toContainText("# throwaway");

    // A new file in an expanded folder appears without collapsing it.
    await page.locator('[data-path="scripts"]').click({ button: "right" });
    await page.getByText("New file…").click();
    await page.getByRole("dialog", { name: "New file" }).getByRole("textbox").fill("extra.py");
    await page.getByRole("dialog", { name: "New file" }).getByRole("button", { name: "Create" }).click();
    await expect(page.locator('.tree-row[data-path="scripts/extra.py"]')).toBeVisible();
    await expect(page.locator('.tree-row[data-path="scripts/decay_sim.py"]')).toBeVisible();
  });

  test("settings: provider keys", async ({ page, bridge }) => {
    await createSession(page, bridge);
    await expect(page.getByTestId("unsandboxed-badge")).toHaveCount(0);
    await page.getByRole("button", { name: "Settings" }).click();
    const dialog = page.getByRole("dialog", { name: "Settings" });
    const rows = dialog.getByTestId("provider-row");
    await expect(rows).toHaveCount(3);

    // A key from the environment is read-only and names its variable.
    const openai = dialog.locator('[data-provider="openai"]');
    await expect(openai).toContainText("OPENAI_API_KEY");
    await expect(openai.locator("input")).toHaveCount(0);

    const anthropic = dialog.locator('[data-provider="anthropic"]');
    await expect(anthropic).toContainText("no key");
    await expect(anthropic.getByRole("button", { name: "Remove" })).toBeDisabled();
    await anthropic.locator('input[type="password"]').fill("sk-ant-test");
    await anthropic.getByRole("button", { name: "Save" }).click();
    await expect(anthropic.locator(".badge")).toHaveText("stored");
    await expect(anthropic.locator('input[type="password"]')).toHaveValue("");
    await anthropic.getByRole("button", { name: "Remove" }).click();
    await expect(anthropic.locator(".badge")).toHaveText("no key");

  });

  test("the unsandboxed badge on a session whose kernel has no sandbox", async ({ page, unsandboxedBridge }) => {
    // The real core's one local target is sandboxed; a second bridge runs its kernels with
    // resource limits only, the state a remote target without bubblewrap puts a session in.
    await createSession(page, unsandboxedBridge, "decay-analysis");
    const badge = page.getByTestId("unsandboxed-badge");
    await expect(badge).toBeVisible({ timeout: 15_000 });
    await expect(badge).toHaveAttribute("title", /ask(s)? for your approval/);
    // Closing the session stops its kernel, which shares the project with the other bridge.
    await page.getByTestId("session-tab").filter({ hasText: "decay-analysis" }).getByRole("button", { name: /Close session/ }).click();
    await expect(page.getByTestId("welcome")).toBeVisible();
  });

  test("B7: a 1.9 MiB output stream stays responsive, is bounded, and can be shown in full", async ({ page, bridge }) => {
    await createSession(page, bridge);
    await page.locator(".dv-tab", { hasText: "Scripts" }).click();
    const card = page.locator('[data-script="flood"]');
    await card.locator(".script-header").click();
    await card.getByRole("button", { name: "Run", exact: true }).click();

    const panel = page.getByTestId("run-output");
    const stdout = page.getByTestId("run-stdout");
    // The whole run finishes quickly; the old <pre> needed minutes of main-thread time.
    await expect(panel).toContainText("exit 0", { timeout: 30_000 });
    await expect(stdout).toContainText("residual rms");
    // The carriage-return progress bar shows its final state only.
    await expect(page.getByTestId("run-stderr")).toContainText("fit 100%");
    await expect(page.getByTestId("run-stderr")).not.toContainText("fit  25%");
    await expect(page.getByTestId("output-truncated")).toContainText("256.0 KiB");

    // Only a window of rows is in the DOM, whatever the size of the text.
    const rendered = await stdout.locator(".output-row").count();
    expect(rendered).toBeLessThan(200);
    const tailRows = Number(await stdout.getAttribute("data-rows"));
    expect(tailRows).toBeGreaterThan(1000);
    expect(tailRows).toBeLessThan(4000);

    // The full output (run_get reads up to 2 MiB per stream) is still cheap to show and scroll.
    await page.getByRole("button", { name: "Show full output" }).click();
    await expect(page.getByTestId("output-truncated")).toHaveCount(0);
    await expect.poll(async () => Number(await stdout.getAttribute("data-rows")), { timeout: 10_000 }).toBeGreaterThan(15_000);
    expect(await stdout.locator(".output-row").count()).toBeLessThan(200);
    const t0 = Date.now();
    await stdout.evaluate((el) => (el.scrollTop = 0));
    await expect(stdout).toContainText("Simulating n=200");
    await stdout.evaluate((el) => (el.scrollTop = el.scrollHeight));
    await expect(stdout).toContainText("residual rms");
    expect(Date.now() - t0).toBeLessThan(3000);
    await page.getByRole("button", { name: "Back to live tail" }).click();
    await expect(page.getByTestId("output-truncated")).toBeVisible();
  });
});
