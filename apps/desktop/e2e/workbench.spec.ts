import { expect, test } from "@playwright/test";
import type { Page } from "@playwright/test";

interface Call {
  cmd: string;
  args: Record<string, unknown>;
}

async function calls(page: Page, cmd: string): Promise<Call[]> {
  return page.evaluate((name) => {
    const b = (window as unknown as { __PRESENT_BACKEND__: { calls: { cmd: string; args: Record<string, unknown> }[] } }).__PRESENT_BACKEND__;
    return b.calls.filter((c) => c.cmd === name);
  }, cmd);
}

async function open(page: Page) {
  await page.goto("/");
  await expect(page.getByTestId("rail-item")).toHaveCount(8);
  await expect(page.getByTestId("project-tab").locator(".dot.ready")).toBeVisible();
}

const railIds = (page: Page) => page.getByTestId("rail-item").evaluateAll((els) => els.map((e) => e.getAttribute("data-slide-id")));

test.describe("workbench (mock backend)", () => {
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

  test("the demo project loads with eight thumbnails, sections, badges and the dock regions", async ({ page }) => {
    await open(page);
    const tab = page.getByTestId("project-tab");
    await expect(tab).toContainText("Demo review");
    await expect(tab).toContainText("Corp 2026");
    await expect(page.getByTestId("rail-section")).toHaveText(["Opening", "Results", "Plan"]);
    await expect(page.getByTestId("lock-icon")).toHaveCount(1);
    await expect(page.getByTestId("badge-red")).toHaveCount(1);
    await expect(page.getByTestId("badge-grey")).toHaveCount(1);
    await expect(page.getByTestId("rail-item").nth(6).getByTestId("badge-errors")).toHaveText("1");
    // Every thumbnail is a live preview of its slide.
    await expect(page.locator('[data-testid="rail-item"] .slide-preview')).toHaveCount(8);
    await expect(page.getByTestId("rail-item").nth(2).locator("rect.sp-bar")).toHaveCount(8);
    for (const title of ["Slides", "Outline", "References", "Assets", "Skills", "Slide", "Agent", "Sources", "Checks", "Notes", "Exports"]) {
      await expect(page.locator(".dv-tab", { hasText: new RegExp(`^${title}$`) })).toHaveCount(1);
    }
    await expect(page.getByTestId("slide-canvas")).toContainText("Quarterly review");
    await expect(page.getByTestId("mode-switch").getByRole("button", { name: "Build" })).toHaveAttribute("aria-pressed", "true");
    await expect(page.getByTestId("model-picker")).toHaveValue("gateway/claude-opus");
  });

  test("select a slide, edit a bullet in place, and slide_update carries the edited document", async ({ page }) => {
    await open(page);
    await page.getByTestId("rail-item").nth(1).click();
    await expect(page.getByTestId("slide-position")).toHaveText("Slide 2 of 8");
    const canvas = page.getByTestId("slide-canvas");
    const body = canvas.locator('[data-ph="body"]');
    await expect(body.locator("p")).toHaveCount(4);

    await body.locator("p").nth(3).click();
    const editor = body.locator("[data-slide-editor]");
    await expect(editor).toBeFocused();
    // No citation markers while editing: they are not text.
    await expect(editor.locator(".sp-cite")).toHaveCount(0);
    await page.keyboard.press("Control+End");
    await page.keyboard.type(" this quarter");
    // Enter makes a paragraph, Tab indents it, Ctrl+B makes bold text.
    await page.keyboard.press("Enter");
    await page.keyboard.press("Tab");
    await page.keyboard.type("Mostly ");
    await page.keyboard.press("Control+b");
    await page.keyboard.type("logistics");
    await page.getByTestId("slide-position").click();

    await expect(body.locator("[data-slide-editor]")).toHaveCount(0);
    await expect(body.locator("p")).toHaveCount(5);
    await expect(body.locator("p").nth(3)).toContainText("Operating costs down 4% on procurement savings this quarter");
    await expect(body.locator("p").nth(4)).toHaveAttribute("data-level", "1");
    // The left dock is still there: Ctrl+B went to the editor, not to the global keymap.
    await expect(page.getByTestId("slides-view")).toBeVisible();

    const updates = await calls(page, "slide_update");
    expect(updates).toHaveLength(1);
    const { slide, expected_hash } = updates[0]!.args as { slide: { id: string; placeholders: Record<string, { type: string; id: string; text: { paragraphs: { level?: number; runs: { t: string; b?: boolean; cite?: string[] }[] }[] } }> }; expected_hash: string };
    expect(slide.id).toBe("s-margin");
    expect(expected_hash).toMatch(/^[0-9a-f]{16}$/);
    const paragraphs = slide.placeholders.body!.text.paragraphs;
    expect(paragraphs).toHaveLength(5);
    // The edited run keeps its citation; untouched paragraphs are unchanged.
    expect(paragraphs[3]).toEqual({ level: 0, runs: [{ t: "Operating costs down 4% on procurement savings this quarter", cite: ["ref:r-mail@51b0e9d2c3f4#c64-139"] }] });
    expect(paragraphs[4]!.level).toBe(1);
    expect(paragraphs[4]!.runs.map((r) => [r.t, !!r.b])).toEqual([["Mostly ", false], ["logistics", true]]);
    expect(paragraphs[0]!.runs[1]).toMatchObject({ t: "3.1 pts", b: true, color: "accent2" });
    expect(slide.placeholders.title!.id).toBe("e-t002");
    // The rail thumbnail shows the edit too.
    await expect(page.getByTestId("rail-item").nth(1)).toContainText("this quarter");

    // Escape cancels an edit: nothing is sent.
    await canvas.locator('[data-ph="title"]').click();
    await page.keyboard.type("zzz");
    await page.keyboard.press("Escape");
    await expect(canvas.locator('[data-ph="title"]')).not.toContainText("zzz");
    expect(await calls(page, "slide_update")).toHaveLength(1);
  });

  test("switch the layout with the picker; content is carried over by placeholder key", async ({ page }) => {
    await open(page);
    await page.getByTestId("rail-item").nth(1).click();
    await page.getByTestId("layout-button").click();
    const picker = page.getByTestId("layout-picker");
    await expect(picker.locator(".layout-option")).toHaveCount(12);
    await expect(picker.locator('[data-layout="Title and Content"]')).toHaveAttribute("aria-selected", "true");
    await expect(picker.locator('[data-layout="Two Content"] img')).toBeVisible();
    await expect(picker).toContainText("Two things side by side");
    await picker.locator('[data-layout="Two Content"]').click();
    await expect(picker).toHaveCount(0);
    const canvas = page.getByTestId("slide-canvas");
    await expect(canvas).toHaveAttribute("data-layout", "Two Content");
    await expect(page.getByTestId("layout-button")).toContainText("Two Content");
    await expect(canvas.locator('[data-ph="body"] p')).toHaveCount(4);
    await expect(canvas.locator('[data-ph="body-2"]')).toContainText("Click to add text");
    const update = (await calls(page, "slide_update"))[0]!.args as { slide: { layout: string; placeholders: Record<string, unknown> } };
    expect(update.slide.layout).toBe("Two Content");
    expect(Object.keys(update.slide.placeholders).sort()).toEqual(["body", "title"]);
  });

  test("reorder slides by drag and by keyboard", async ({ page }) => {
    await open(page);
    const items = page.getByTestId("rail-item");
    // Drag slide 2 onto the lower half of slide 3: it lands after it.
    const target = (await items.nth(2).boundingBox())!;
    await items.nth(1).dragTo(items.nth(2), { targetPosition: { x: target.width / 2, y: target.height - 6 } });
    await expect.poll(() => railIds(page)).toEqual(["s-title", "s-revenue", "s-margin", "s-regions", "s-process", "s-timeline", "s-site", "s-notes"]);
    expect((await calls(page, "slide_move")).at(-1)!.args).toMatchObject({ slide_id: "s-margin", after: "s-revenue" });
    await expect(page.locator('[data-slide-id="s-margin"] .rail-gutter')).toContainText("3");

    // Keyboard: Alt+ArrowUp on the focused rail item moves it back up.
    await page.locator('[data-testid="rail-item"][data-slide-id="s-margin"]').focus();
    await page.keyboard.press("Alt+ArrowUp");
    await expect.poll(() => railIds(page)).toEqual(["s-title", "s-margin", "s-revenue", "s-regions", "s-process", "s-timeline", "s-site", "s-notes"]);
    expect((await calls(page, "slide_move")).at(-1)!.args).toMatchObject({ slide_id: "s-margin", after: "s-title" });
    await expect(page.getByTestId("slide-position")).toHaveText("Slide 2 of 8");
  });

  test("rail context menu: duplicate, lock and delete", async ({ page }) => {
    await open(page);
    const items = page.getByTestId("rail-item");
    await items.nth(7).click({ button: "right" });
    await page.getByRole("menuitem", { name: "Duplicate" }).click();
    await expect(items).toHaveCount(9);
    await expect(page.getByTestId("slide-position")).toHaveText("Slide 9 of 9");
    await items.nth(8).click({ button: "right" });
    await page.getByRole("menuitem", { name: "Lock", exact: true }).click();
    await expect(page.getByTestId("lock-icon")).toHaveCount(2);
    await expect(page.getByTestId("lock-toggle")).toHaveAttribute("aria-pressed", "true");
    await page.getByTestId("lock-toggle").click();
    await expect(page.getByTestId("lock-icon")).toHaveCount(1);
    await items.nth(8).click({ button: "right" });
    await page.getByRole("menuitem", { name: "Delete" }).click();
    await expect(items).toHaveCount(8);
    await expect(page.getByTestId("slide-position")).toHaveText("Slide 8 of 8");
  });

  test("outline view: editing a title issues slide_update and the canvas follows", async ({ page }) => {
    await open(page);
    await page.locator(".dv-tab", { hasText: /^Outline$/ }).click();
    const rows = page.getByTestId("outline-row");
    await expect(rows).toHaveCount(8);
    const input = page.getByLabel("Title of slide 5");
    await expect(input).toHaveValue("Pricing changes follow four steps");
    await input.fill("Pricing follows four steps");
    await input.press("Enter");
    await expect(page.getByTestId("slide-canvas")).toContainText("Pricing follows four steps");
    const update = (await calls(page, "slide_update"))[0]!.args as { slide: { id: string; placeholders: Record<string, { type: string }> } };
    expect(update.slide.id).toBe("s-process");
    expect(update.slide.placeholders.body!.type).toBe("diagram");
    await page.getByLabel("Layout of slide 8").selectOption("Two Content");
    await expect(rows.nth(7).locator("select")).toHaveValue("Two Content");
    await expect(page.getByLabel("Lock slide 4")).toBeChecked();
  });

  test("a citation marker opens the source passage in the Sources panel", async ({ page }) => {
    await open(page);
    await page.getByTestId("rail-item").nth(1).click();
    await page.getByTestId("slide-canvas").locator(".sp-cite").first().click();
    await expect(page.getByTestId("source-text").locator("mark")).toHaveText("Gross margin rose 2.9 pts quarter on quarter, to 18% of net revenue.");
    // Clicking a marker does not start editing.
    await expect(page.locator("[data-slide-editor]")).toHaveCount(0);
    await page.getByTestId("rail-item").nth(3).click();
    await page.getByTestId("slide-canvas").locator(".sp-cite").nth(2).click();
    await expect(page.getByTestId("source-table").locator("td.hl")).toHaveText(["APAC", "90"]);
  });

  test("toggle the theme; the slide keeps the template's colours", async ({ page }) => {
    await open(page);
    await page.getByTestId("settings").click();
    await page.getByLabel("Theme").selectOption("dark");
    await expect(page.locator("html")).toHaveAttribute("data-theme", "dark");
    await expect(page.getByTestId("providers")).toContainText("gateway");
    await expect(page.getByTestId("engines")).toContainText("libreoffice");
    await page.getByRole("button", { name: "Done" }).click();
    const colors = await page.evaluate(() => ({
      chrome: getComputedStyle(document.querySelector(".topbar")!).backgroundColor,
      slide: getComputedStyle(document.querySelector('[data-testid="slide-canvas"]')!).backgroundColor,
      title: getComputedStyle(document.querySelector('[data-testid="slide-canvas"] [data-ph="title"] p')!).color,
    }));
    expect(colors.slide).toBe("rgb(255, 255, 255)");
    expect(colors.title).toBe("rgb(27, 31, 36)");
    expect(colors.chrome).not.toBe("rgb(243, 243, 243)");
    // The preference survives a reload.
    await page.reload();
    await expect(page.locator("html")).toHaveAttribute("data-theme", "dark");
    await expect(page.getByTestId("rail-item")).toHaveCount(8);
  });

  test("command palette, go to slide, dock toggles and layout persistence", async ({ page }) => {
    await open(page);
    await page.keyboard.press("Control+Shift+P");
    const palette = page.getByTestId("palette");
    await expect(palette).toBeVisible();
    await palette.getByLabel("Palette query").fill("theme dark");
    await expect(palette.locator(".palette-item").first()).toContainText("Theme: dark");
    await page.keyboard.press("Enter");
    await expect(palette).toHaveCount(0);
    await expect(page.locator("html")).toHaveAttribute("data-theme", "dark");

    await page.keyboard.press("Control+p");
    await page.getByLabel("Palette query").fill("roll-out");
    await page.keyboard.press("Enter");
    await expect(page.getByTestId("slide-position")).toHaveText("Slide 6 of 8");

    await page.keyboard.press("Control+b");
    await expect(page.getByTestId("slides-view")).toBeHidden();
    await page.keyboard.press("Control+b");
    await expect(page.getByTestId("slides-view")).toBeVisible();
    // The layout is saved through project_layout_set (debounced).
    await expect.poll(async () => (await calls(page, "project_layout_set")).length).toBeGreaterThan(0);
    const saved = (await calls(page, "project_layout_set")).at(-1)!.args as { layout: { panels: Record<string, unknown> } };
    expect(Object.keys(saved.layout.panels)).toContain("canvas");
  });

  test("fidelity render flips thumbnails, and an edit brings the live preview back", async ({ page }) => {
    await open(page);
    await expect(page.locator('.rail-thumb[data-thumb="preview"]')).toHaveCount(8);
    await page.keyboard.press("Control+Shift+P");
    await page.getByLabel("Palette query").fill("render fidelity");
    await page.keyboard.press("Enter");
    await expect(page.locator('.rail-thumb[data-thumb="fidelity"]')).toHaveCount(8);
    await page.locator(".dv-tab", { hasText: /^Outline$/ }).click();
    const input = page.getByLabel("Title of slide 1");
    await input.fill("Quarterly review 2026");
    await input.press("Enter");
    await page.locator(".dv-tab", { hasText: /^Slides$/ }).click();
    await expect(page.locator('.rail-thumb[data-thumb="fidelity"]')).toHaveCount(7);
    await expect(page.getByTestId("rail-item").first().locator(".slide-preview")).toContainText("Quarterly review 2026");
  });

  test("create a project from the dialog, switch tabs, and pick a degraded model", async ({ page }) => {
    await open(page);
    await page.getByTestId("new-project").click();
    await page.getByLabel("Folder").fill("/home/user/decks/board-pre-read");
    await page.getByLabel("Template").fill("/home/user/templates/corp.potx");
    await page.getByLabel("Deck mode").selectOption("code");
    await page.getByTestId("create-project").click();
    const tabs = page.getByTestId("project-tab");
    await expect(tabs).toHaveCount(2);
    await expect(tabs.nth(1)).toHaveAttribute("aria-selected", "true");
    await expect(tabs.nth(1)).toContainText("board-pre-read");
    await expect(tabs.nth(1)).toContainText("code");
    // Code mode: the canvas is read-only.
    await expect(page.getByTestId("code-mode-banner")).toBeVisible();
    await expect(page.getByTestId("rail-item")).toHaveCount(1);
    await expect(page.getByTestId("layout-button")).toBeDisabled();
    await page.locator('[data-testid="canvas"] [data-ph="title"]').click();
    await expect(page.locator("[data-slide-editor]")).toHaveCount(0);

    await tabs.nth(0).click();
    await expect(page.getByTestId("rail-item")).toHaveCount(8);
    await expect(page.getByTestId("degraded-badge")).toHaveCount(0);
    await page.getByTestId("model-picker").selectOption("local/open-weights-32b");
    await expect(page.getByTestId("degraded-badge")).toBeVisible();
    await expect(page.getByTestId("degraded-badge")).toHaveAttribute("title", /no vision/);
    await page.getByTestId("mode-switch").getByRole("button", { name: "Outline" }).click();
    await expect(page.getByTestId("mode-switch").getByRole("button", { name: "Outline" })).toHaveAttribute("aria-pressed", "true");
  });
});
