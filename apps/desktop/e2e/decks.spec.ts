// The deck workbench in the one window (mock backend): create a deck from a template, edit a
// slide in place, reorder slides, place a run's figure on a slide and follow its citation to the
// run, accept a slide proposal in the one chat, export.
import { expect, test } from "@playwright/test";
import type { Locator, Page } from "@playwright/test";

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

/** Creates a deck through the Decks view and its New deck dialog. */
async function createDeck(page: Page, title = "Q3 Review") {
  await page.locator(".dv-tab", { hasText: "Decks" }).click();
  const view = page.getByTestId("decks-view");
  await view.getByTestId("new-deck").click();
  const dialog = page.getByRole("dialog", { name: "New deck" });
  await dialog.getByLabel("Title").fill(title);
  await dialog.getByLabel("Template").fill(TEMPLATE);
  await dialog.getByTestId("create-deck").click();
  await expect(dialog).toHaveCount(0);
  // The new deck opens in the Slides rail (beside the Decks view in the sidebar) and the Canvas.
  await expect(page.getByTestId("slides-view")).toContainText(title);
  await expect(page.getByTestId("canvas")).toBeVisible();
}

/** Adds a slide after the selected one and gives it a title, typed in place on the canvas. */
async function addSlide(page: Page, title: string) {
  const rail = page.getByTestId("slides-view");
  const before = await rail.getByTestId("rail-item").count();
  await rail.getByTestId("add-slide").click();
  await expect(rail.getByTestId("rail-item")).toHaveCount(before + 1);
  await editTitle(page, title);
}

async function editTitle(page: Page, title: string) {
  const canvas = page.getByTestId("slide-canvas");
  await canvas.locator('[data-ph="title"]').click();
  const editor = canvas.locator('[data-slide-editor="title"]');
  await expect(editor).toBeFocused();
  await page.keyboard.press("ControlOrMeta+a");
  await page.keyboard.type(title);
  // Leaving the field commits the edit (slide_update with the expected hash).
  await page.getByTestId("slide-position").click();
  await expect(canvas.locator('[data-ph="title"]')).toContainText(title);
}

function railTitles(page: Page): Locator {
  return page.getByTestId("slides-view").getByTestId("rail-item");
}

test.describe("deck workbench (mock backend)", () => {
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
    expect(await page.evaluate(() => (globalThis as unknown as { __analystErrors?: string[] }).__analystErrors), "window.__analystErrors").toEqual([]);
  });

  test("create a deck from a template and edit a slide title in place", async ({ page }) => {
    await createSession(page);
    await createDeck(page);
    await page.locator(".dv-tab", { hasText: "Decks" }).click();
    await expect(page.getByTestId("decks-view").getByTestId("deck-item")).toContainText("decks/q3-review");
    await page.locator(".dv-tab", { hasText: "Slides" }).click();
    await addSlide(page, "Margin recovered to 18%");
    await expect(railTitles(page).first()).toHaveAttribute("aria-label", "Slide 1: Margin recovered to 18%");
    // The edit is in the backend: the outline reads it back.
    await palette(page, "Show deck outline");
    await expect(page.getByTestId("outline-view").getByLabel("Title of slide 1")).toHaveValue("Margin recovered to 18%");
    // Analysis and deck panels share the dock: the editor group holds the canvas and the outline.
    await expect(page.locator(".dv-tab", { hasText: "Canvas" })).toBeVisible();
    await expect(page.locator(".dv-tab", { hasText: "Project" })).toBeVisible();
  });

  test("reorder slides by dragging in the rail", async ({ page }) => {
    await createSession(page);
    await createDeck(page);
    await addSlide(page, "First");
    await addSlide(page, "Second");
    await addSlide(page, "Third");
    await expect(railTitles(page)).toHaveCount(3);
    await expect(railTitles(page).nth(2)).toHaveAttribute("aria-label", "Slide 3: Third");
    // Drag "Third" onto the top half of "First": it lands first.
    const first = railTitles(page).nth(0);
    const box = await first.boundingBox();
    if (!box) throw new Error("no rail item");
    await railTitles(page).nth(2).dragTo(first, { targetPosition: { x: box.width / 2, y: 4 } });
    await expect(railTitles(page).nth(0)).toHaveAttribute("aria-label", "Slide 1: Third");
    await expect(railTitles(page).nth(1)).toHaveAttribute("aria-label", "Slide 2: First");
    await expect(railTitles(page).nth(2)).toHaveAttribute("aria-label", "Slide 3: Second");
  });

  test("place a run's figure on a slide and follow its citation to the run", async ({ page }) => {
    await createSession(page);
    // A run that writes a figure.
    await page.locator(".dv-tab", { hasText: "Scripts" }).click();
    const card = page.locator('[data-script="decay_sim"]');
    await card.locator(".script-header").click();
    await card.getByLabel("--tau", { exact: false }).fill("1.8");
    await card.getByRole("button", { name: "Run", exact: true }).click();
    await expect(page.getByTestId("run-output")).toContainText("exit 0", { timeout: 15_000 });
    await expect(page.getByTestId("figure-frame").locator("img")).toBeVisible();

    await createDeck(page);
    await addSlide(page, "Decay constant recovered");
    // Figures panel: a direct user action places the figure on the selected slide.
    await page.locator(".dv-tab", { hasText: "Figures" }).click();
    await page.getByTestId("place-on-slide").click();
    const canvas = page.getByTestId("slide-canvas");
    await expect(canvas.locator('[data-type="image"] img')).toBeVisible();
    const badge = canvas.getByTestId("run-cite-badge");
    await expect(badge).toBeVisible();
    await expect(badge).toHaveAttribute("title", /scripts\/decay_sim\.py --tau 1\.8/);

    // One click from the slide element to its run: Runs view, figure and run output (script, arguments).
    await page.locator(".dv-tab", { hasText: "Project" }).click();
    await badge.click();
    const runs = page.locator(".dv-tab", { hasText: "Runs" });
    await expect(runs).toHaveClass(/dv-active-tab/);
    const runId = await badge.getAttribute("data-run");
    await expect(page.locator(`[data-run="${runId}"].list-item.selected`)).toBeVisible();
    await expect(page.getByTestId("figures-panel")).toBeVisible();
    await expect(page.getByTestId("figure-tab").filter({ hasText: "decay-simulation" })).toHaveAttribute("aria-selected", "true");
    await expect(page.getByTestId("run-output")).toContainText("decay_sim.py");
    await expect(page.getByTestId("run-output")).toContainText("--tau 1.8");
    // The outline lists the run among the slide's sources.
    await palette(page, "Show deck outline");
    await expect(page.getByTestId("outline-row").first()).toContainText("run decay-simulation.png");
  });

  test("accept a slide proposal in the one chat", async ({ page }) => {
    await createSession(page);
    await createDeck(page);
    await addSlide(page, "Results");
    const chat = page.getByTestId("chat");
    const modes = chat.getByTestId("mode-select");
    await expect(modes.locator("option")).toHaveText(["Plan", "Build", "Run only", "Outline", "Slides", "Polish"]);
    await modes.selectOption("slides");
    await expect(modes).toHaveValue("slides");
    await chat.getByLabel("Message the agent").fill("Give the first slide an action title");
    await chat.getByTestId("chat-send").click();
    const proposal = chat.getByTestId("slide-proposal-card");
    await expect(proposal).toBeVisible({ timeout: 20_000 });
    await expect(proposal.getByTestId("slide-before").locator(".slide-preview")).toContainText("Results");
    await expect(proposal.getByTestId("slide-after").locator(".slide-preview")).toContainText("Decay constant recovered within 2% of the input");
    await proposal.getByTestId("proposal-accept").click();
    await expect(proposal).toContainText("accepted");
    // The accepted change reaches the deck panels, and the checker's result shows as a card.
    await expect(railTitles(page).first()).toHaveAttribute("aria-label", "Slide 1: Decay constant recovered within 2% of the input");
    await expect(chat.getByTestId("deck-check-card")).toContainText("0 errors", { timeout: 15_000 });
    await expect(chat.getByTestId("summary-card")).toBeVisible({ timeout: 15_000 });
  });

  test("export the deck", async ({ page }) => {
    await createSession(page);
    await createDeck(page);
    await addSlide(page, "Results");
    await palette(page, "Show deck exports");
    const exports = page.getByTestId("exports-panel");
    await expect(exports).toBeVisible();
    await exports.getByTestId("export-pptx").click();
    await expect(exports.getByTestId("export-item")).toContainText("exports/q3-review.pptx", { timeout: 15_000 });
    await expect(exports.getByTestId("export-item")).toContainText("pptx");
    await exports.getByRole("button", { name: "Reveal" }).click();
  });
});
