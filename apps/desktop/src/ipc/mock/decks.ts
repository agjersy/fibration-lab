// The deck commands of the MockBackend (IPC.md section 9): decks of an open session kept in
// memory, with the event shapes of the real service (`{type: "deck", deck, event}` on the
// session channel, `decks_changed` when a deck is created). Every deck uses the template
// description of the corp template fixture (the real output of `template inspect`), so the deck
// panels draw real layouts.
import { registerDeckFile } from "../../deck/files";
import type { Commands } from "../commands";
import type { ArtifactEntry, CheckpointView, Deck, DeckSummary, IpcError, SessionEvent, SessionId, TaskId } from "../types";
import { TEMPLATE } from "./deck/fixtures";

type DeckCommand =
  | "decks_list"
  | "deck_create"
  | "deck_open"
  | "template_thumbnails"
  | "template_replace"
  | "deck_get"
  | "slide_update"
  | "slide_create"
  | "slide_delete"
  | "slide_duplicate"
  | "slide_move"
  | "slide_lock"
  | "slide_place_figure"
  | "slide_preview_png"
  | "chart_compute"
  | "deck_code_read"
  | "deck_code_write"
  | "deck_build"
  | "deck_switch_mode"
  | "render_fidelity"
  | "deck_check_run"
  | "refs_list"
  | "refs_add"
  | "refs_remove"
  | "refs_remove_unused"
  | "refs_search"
  | "source_passage"
  | "assets_list"
  | "asset_add"
  | "exports_list"
  | "export_run"
  | "export_reveal"
  | "export_open"
  | "history_list"
  | "history_restore"
  | "history_revert_turn"
  | "artifacts_list";

export type DeckHandlers = {
  [K in DeckCommand]: (args: Commands[K]["args"]) => Promise<Commands[K]["result"]> | Commands[K]["result"];
};

export interface DeckMockEnv {
  /** Throws `not_found` unless the session exists and is open. */
  requireOpen: (session: SessionId) => void;
  emit: (session: SessionId, event: SessionEvent) => void;
  nextId: () => number;
  /** Resolves after a simulated delay. */
  later: (ms: number) => Promise<void>;
  /** What the session's runs produced (the artifact index). */
  artifacts: (session: SessionId) => ArtifactEntry[];
  /** A loadable URL of a run's figure, for placing it on a slide. */
  figureUrl: (session: SessionId, path: string) => string | null;
}

/** What the mock agent needs to propose slide changes. */
export interface DeckAgentApi {
  /** The first slide of the session's first deck. */
  firstSlide(session: SessionId): { deck: string; slide: Deck.Slide } | null;
  /** Writes an accepted slide proposal (`by: "agent"`, as the service reports it). */
  applySlide(session: SessionId, deck: string, slide: Deck.Slide): void;
}

interface MockDeck {
  summary: DeckSummary;
  deck: Deck.Deck;
  slides: Map<string, { slide: Deck.Slide; hash: string }>;
  refs: Deck.RefView[];
  assets: Deck.AssetView[];
  exports: Deck.ExportView[];
  code: Map<string, string>;
}

function ipcError(code: IpcError["code"], message: string): IpcError {
  return { code, message };
}

function hashOf(value: unknown): string {
  const text = JSON.stringify(value);
  let h = 0;
  for (let i = 0; i < text.length; i++) h = (Math.imul(31, h) + text.charCodeAt(i)) | 0;
  return (h >>> 0).toString(16).padStart(8, "0");
}

const LAYOUTS = TEMPLATE.masters.flatMap((m) => m.layouts);

function template(name: string): Deck.TemplateDescription {
  return { ...structuredClone(TEMPLATE), path: `template/${name}` };
}

export function makeDeckHandlers(env: DeckMockEnv): DeckHandlers {
  return makeDeckMock(env).handlers;
}

export function makeDeckMock(env: DeckMockEnv): { handlers: DeckHandlers; agent: DeckAgentApi } {
  const decks = new Map<SessionId, Map<string, MockDeck>>();
  const checkpoints = new Map<SessionId, CheckpointView[]>();

  const of = (session: SessionId): Map<string, MockDeck> => {
    env.requireOpen(session);
    let m = decks.get(session);
    if (!m) {
      m = new Map();
      decks.set(session, m);
    }
    return m;
  };
  const deckOf = (session: SessionId, slug: string): MockDeck => {
    const d = of(session).get(slug);
    if (!d) throw ipcError("not_found", `no deck named \`${slug}\``);
    return d;
  };
  const emitDeck = (session: SessionId, deck: string, event: Deck.DeckEvent): void =>
    env.emit(session, { type: "deck", deck, event });
  const checkpoint = (session: SessionId, label: string): string => {
    const id = `cp-${env.nextId()}`;
    const list = checkpoints.get(session) ?? [];
    list.unshift({ id, created_at: new Date().toISOString(), label, conversation: null, turn: null });
    checkpoints.set(session, list);
    return id;
  };
  const task = (session: SessionId, deck: string, label: string, work: () => string | null): TaskId => {
    const id = `deck-task-${env.nextId()}`;
    emitDeck(session, deck, { type: "task_progress", task_id: id, fraction: null, message: label });
    void (async () => {
      await env.later(200);
      let message: string | null;
      let ok = true;
      try {
        message = work();
      } catch (e) {
        ok = false;
        message = (e as IpcError).message ?? String(e);
      }
      emitDeck(session, deck, { type: "task_done", task_id: id, ok, message });
    })();
    return id;
  };
  const order = (d: MockDeck): string[] => d.deck.sections.flatMap((s) => s.slides);
  const view = (d: MockDeck, id: string): Deck.SlideView => {
    const entry = d.slides.get(id);
    if (!entry) throw ipcError("not_found", `no slide ${id}`);
    const index = order(d).indexOf(id);
    const section = d.deck.sections.find((s) => s.slides.includes(id))?.title ?? "";
    return {
      slide: structuredClone(entry.slide),
      hash: entry.hash,
      index,
      section,
      thumb: { kind: "preview", path: null, stale: true },
      checks: { errors: 0, warnings: 0 },
      numbers: { green: 0, amber: 0, red: 0, grey: 0 },
    };
  };
  const changed = (session: SessionId, d: MockDeck, changedIds: string[], removed: string[] = [], by = "user"): void => {
    d.summary.slides = order(d).length;
    emitDeck(session, d.summary.slug, { type: "deck_changed", deck: structuredClone(d.deck), changed: changedIds, removed, by });
  };
  const newSlide = (layout: string): Deck.Slide => {
    const titleKey = LAYOUTS.find((l) => l.name === layout)?.placeholders.find((p) => p.type === "title" || p.type === "ctrTitle")?.key;
    return {
      id: `s-${env.nextId().toString(36).padStart(6, "0")}`,
      kind: "layout",
      layout,
      placeholders: titleKey ? { [titleKey]: { type: "text", id: `e-${env.nextId()}`, text: { paragraphs: [{ runs: [{ t: "" }] }] } } } : {},
      elements: [],
    };
  };
  /** Image elements whose run citation is stale (the run's artifact is stale in the index). */
  const staleFindings = (session: SessionId, d: MockDeck): Deck.Diagnostic[] => {
    const stale = new Set(env.artifacts(session).filter((a) => a.stale).map((a) => `${a.run_id}\n${a.path}`));
    const out: Deck.Diagnostic[] = [];
    order(d).forEach((id, index) => {
      const slide = d.slides.get(id)?.slide;
      if (!slide) return;
      const contents = [...Object.values(slide.placeholders), ...slide.elements.map((e) => e.content)];
      for (const c of contents) {
        if (c && c.type === "image" && c.run_cite && stale.has(`${c.run_cite.run_id}\n${c.run_cite.artifact}`)) {
          out.push({ rule: "stale", severity: "warn", message: `the figure comes from run ${c.run_cite.run_id}; the code has changed since`, slide_id: id, element_id: c.id, slide_index: index });
        }
      }
    });
    return out;
  };
  const insert = (d: MockDeck, id: string, after?: string, section?: string): void => {
    for (const s of d.deck.sections) s.slides = s.slides.filter((x) => x !== id);
    if (after) {
      const s = d.deck.sections.find((x) => x.slides.includes(after));
      if (!s) throw ipcError("not_found", `no slide ${after}`);
      s.slides.splice(s.slides.indexOf(after) + 1, 0, id);
      return;
    }
    let target = d.deck.sections.find((x) => x.title === (section ?? ""));
    if (!target) {
      target = d.deck.sections[d.deck.sections.length - 1];
      if (!target) {
        target = { title: section ?? "", slides: [] };
        d.deck.sections.push(target);
      }
    }
    target.slides.push(id);
  };

  const handlers: DeckHandlers = {
    decks_list: ({ session }) => [...of(session).values()].map((d) => structuredClone(d.summary)),

    deck_create: ({ session, title, template_abs, slug, code }) => {
      const m = of(session);
      const s =
        slug ??
        title
          .toLowerCase()
          .replace(/[^a-z0-9]+/g, "-")
          .replace(/^-+|-+$/g, "");
      if (!/^[a-z0-9][a-z0-9_-]{0,63}$/.test(s)) throw ipcError("invalid", `\`${s}\` is not a deck name`);
      if (m.has(s)) throw ipcError("conflict", `a deck named \`${s}\` already exists`);
      const templateName = template_abs.split(/[\\/]/).pop() ?? "template.pptx";
      const mode: Deck.DeckMode = code ? "code" : "structured";
      const d: MockDeck = {
        summary: { slug: s, title, mode, slides: 0, template: templateName, open: false },
        deck: { schema_version: 1, mode, title, template: `template/${templateName}`, sections: [{ title: "", slides: [] }] },
        slides: new Map(),
        refs: [],
        assets: [],
        exports: [],
        code: new Map(code ? [["deck/build.py", "from present_sdk import Deck\n"]] : []),
      };
      m.set(s, d);
      env.emit(session, { type: "decks_changed", decks: [...m.values()].map((x) => structuredClone(x.summary)) });
      return structuredClone(d.summary);
    },

    deck_open: ({ session, deck }) => {
      const d = deckOf(session, deck);
      d.summary.open = true;
      return {
        slug: deck,
        title: d.deck.title,
        path_abs: `/mock/decks/${deck}`,
        template_name: d.summary.template,
        deck_mode: d.deck.mode,
        slides: order(d).length,
        kernel: "ready",
        kernel_network: "bwrap",
        engines: [{ kind: "libreoffice", available: false, detail: "not in the mock" }],
        sandbox: null,
        embedding: null,
        template: template(d.summary.template),
        deck: structuredClone(d.deck),
      };
    },

    template_thumbnails: ({ session, deck }) => {
      deckOf(session, deck);
      return LAYOUTS.map((l) => ({ layout: l.name, png: "" }));
    },

    template_replace: ({ session, deck, template_abs }) => {
      const d = deckOf(session, deck);
      return task(session, deck, "Replacing the template", () => {
        checkpoint(session, "before replacing the template");
        d.summary.template = template_abs.split(/[\\/]/).pop() ?? d.summary.template;
        d.deck.template = `template/${d.summary.template}`;
        return null;
      });
    },

    deck_get: ({ session, deck }) => {
      const d = deckOf(session, deck);
      return { deck: structuredClone(d.deck), slides: order(d).map((id) => view(d, id)) };
    },

    slide_update: ({ session, deck, slide, expected_hash }) => {
      const d = deckOf(session, deck);
      const entry = d.slides.get(slide.id);
      if (!entry) throw ipcError("not_found", `no slide ${slide.id}`);
      if (expected_hash && expected_hash !== entry.hash) throw ipcError("conflict", `slide ${slide.id} changed since it was read`);
      d.slides.set(slide.id, { slide: structuredClone(slide), hash: hashOf(slide) });
      changed(session, d, [slide.id]);
      return view(d, slide.id);
    },

    slide_create: ({ session, deck, after, layout }) => {
      const d = deckOf(session, deck);
      if (d.deck.mode === "code") throw ipcError("invalid", "the deck is in code generation mode");
      const slide = newSlide(layout);
      d.slides.set(slide.id, { slide, hash: hashOf(slide) });
      insert(d, slide.id, after);
      changed(session, d, [slide.id]);
      return view(d, slide.id);
    },

    slide_delete: ({ session, deck, slide_id }) => {
      const d = deckOf(session, deck);
      if (!d.slides.delete(slide_id)) throw ipcError("not_found", `no slide ${slide_id}`);
      for (const s of d.deck.sections) s.slides = s.slides.filter((x) => x !== slide_id);
      changed(session, d, [], [slide_id]);
      return null;
    },

    slide_duplicate: ({ session, deck, slide_id }) => {
      const d = deckOf(session, deck);
      const entry = d.slides.get(slide_id);
      if (!entry) throw ipcError("not_found", `no slide ${slide_id}`);
      const copy: Deck.Slide = { ...structuredClone(entry.slide), id: newSlide(entry.slide.layout).id, locked: undefined };
      d.slides.set(copy.id, { slide: copy, hash: hashOf(copy) });
      insert(d, copy.id, slide_id);
      changed(session, d, [copy.id]);
      return view(d, copy.id);
    },

    slide_move: ({ session, deck, slide_id, after, section }) => {
      const d = deckOf(session, deck);
      if (!d.slides.has(slide_id)) throw ipcError("not_found", `no slide ${slide_id}`);
      insert(d, slide_id, after, section);
      changed(session, d, [slide_id]);
      return null;
    },

    slide_lock: ({ session, deck, slide_id, element_id, locked }) => {
      const d = deckOf(session, deck);
      const entry = d.slides.get(slide_id);
      if (!entry) throw ipcError("not_found", `no slide ${slide_id}`);
      if (element_id) {
        const set = new Set(entry.slide.locked_elements ?? []);
        if (locked) set.add(element_id);
        else set.delete(element_id);
        entry.slide.locked_elements = [...set];
      } else {
        entry.slide.locked = locked || undefined;
      }
      entry.hash = hashOf(entry.slide);
      changed(session, d, [slide_id]);
      return null;
    },

    slide_place_figure: ({ session, deck, slide_id, run_id, artifact, placeholder }) => {
      const d = deckOf(session, deck);
      const entry = d.slides.get(slide_id);
      if (!entry) throw ipcError("not_found", `no slide ${slide_id}`);
      const a = env.artifacts(session).find((x) => x.run_id === run_id && x.path === artifact);
      if (!a) throw ipcError("not_found", `run \`${run_id}\` has no artifact \`${artifact}\``);
      const ext = artifact.split(".").pop()?.toLowerCase() ?? "";
      if (a.kind !== "figure" || !["png", "svg", "jpg", "jpeg", "gif"].includes(ext)) throw ipcError("invalid", `\`${artifact}\` is not an image a slide can show; only figures are placed`);
      const layout = LAYOUTS.find((l) => l.name === entry.slide.layout);
      const candidates = (layout?.placeholders ?? []).filter((p) => p.accepts.includes("image")).map((p) => p.key);
      const key = placeholder ?? candidates.find((k) => !entry.slide.placeholders[k] || entry.slide.placeholders[k]?.type === "empty");
      if (!key) throw ipcError("invalid", `layout "${entry.slide.layout}" has no free placeholder for an image`);
      const asset = `assets/${hashOf(`${run_id}/${artifact}`).padEnd(16, "0")}.${ext}`;
      const url = env.figureUrl(session, artifact);
      if (url) registerDeckFile(session, deck, asset, url);
      const id = `e-${env.nextId()}`;
      const slide: Deck.Slide = {
        ...structuredClone(entry.slide),
        placeholders: {
          ...structuredClone(entry.slide.placeholders),
          [key]: { type: "image", id, asset, alt: a.title ?? `Figure from ${a.script}`, run_cite: { run_id, artifact, script: a.script, args: [...a.args], ...(a.tree_hash ? { tree_hash: a.tree_hash } : {}) } },
        },
      };
      if (!d.assets.some((x) => x.path === asset)) d.assets.push({ path: asset, sha256: hashOf(asset), width: 800, height: 600 });
      d.slides.set(slide_id, { slide, hash: hashOf(slide) });
      changed(session, d, [slide_id]);
      return view(d, slide_id);
    },

    slide_preview_png: ({ session, deck, slide_id }) => {
      const d = deckOf(session, deck);
      if (!d.slides.has(slide_id)) throw ipcError("not_found", `no slide ${slide_id}`);
      return null;
    },

    chart_compute: ({ session, deck }) => {
      deckOf(session, deck);
      return { table: { columns: ["Region", "Revenue"], rows: [["North", 120], ["South", 95]], loc: "Sales!A1:B3" } };
    },

    deck_code_read: ({ session, deck, path }) => {
      const d = deckOf(session, deck);
      const content = d.code.get(path);
      if (content === undefined) throw ipcError("not_found", `${path} does not exist`);
      return { path, content, hash: hashOf(content) };
    },

    deck_code_write: ({ session, deck, path, content, expected_hash }) => {
      const d = deckOf(session, deck);
      if (!path.startsWith("deck/") || !path.endsWith(".py")) throw ipcError("invalid", "only Python files under deck/ can be read or written here");
      const current = d.code.get(path);
      if (expected_hash && current !== undefined && hashOf(current) !== expected_hash) throw ipcError("conflict", `${path} changed since it was read`);
      d.code.set(path, content);
      return { hash: hashOf(content) };
    },

    deck_build: ({ session, deck }) => {
      const d = deckOf(session, deck);
      return task(session, deck, "Building the deck", () => {
        if (d.deck.mode !== "code") throw ipcError("invalid", "the deck is not in code generation mode");
        emitDeck(session, deck, { type: "build_output", stream: "stdout", chunk: "built\n" });
        return "built";
      });
    },

    deck_switch_mode: ({ session, deck, to, via }) => {
      const d = deckOf(session, deck);
      return task(session, deck, "Switching deck mode", () => {
        if (d.deck.mode === to) return "already in that mode";
        if (to === "structured" && via !== "revert") throw ipcError("invalid", 'switching back needs via: "revert"');
        if (to === "code") {
          checkpoint(session, "before switching the deck to code mode");
          d.code.set("deck/build.py", "from present_sdk import Deck\n");
        }
        d.deck.mode = to;
        d.summary.mode = to;
        changed(session, d, order(d));
        return null;
      });
    },

    render_fidelity: ({ session, deck }) => {
      const d = deckOf(session, deck);
      return task(session, deck, "Rendering slides", () => `${order(d).length} slides rendered`);
    },

    deck_check_run: ({ session, deck }) => staleFindings(session, deckOf(session, deck)),

    refs_list: ({ session, deck }) => structuredClone(deckOf(session, deck).refs),

    refs_add: ({ session, deck, paths_abs }) => {
      const d = deckOf(session, deck);
      return task(session, deck, "Adding references", () => {
        for (const p of paths_abs) {
          const name = p.split(/[\\/]/).pop() ?? p;
          const ext = name.split(".").pop()?.toLowerCase() ?? "";
          const kinds: Record<string, Deck.RefKind> = { pdf: "pdf", docx: "docx", pptx: "pptx", xlsx: "xlsx", csv: "csv", md: "markdown", png: "image" };
          d.refs.push({
            id: `r-${env.nextId().toString(16).padStart(8, "0")}`,
            version: hashOf(p).slice(0, 12).padEnd(12, "0"),
            kind: kinds[ext] ?? "text",
            title: name,
            size_bytes: 1024,
            pages: null,
            status: "ready",
            error: null,
            cited_by: [],
            parent: null,
          });
        }
        emitDeck(session, deck, { type: "refs_changed", refs: structuredClone(d.refs) });
        return `${paths_abs.length} added`;
      });
    },

    refs_remove: ({ session, deck, ref_id }) => {
      const d = deckOf(session, deck);
      const r = d.refs.find((x) => x.id === ref_id);
      if (!r) throw ipcError("not_found", `no reference ${ref_id}`);
      if (r.cited_by.length > 0) throw ipcError("invalid", "the reference is cited by slides; remove the citations first");
      d.refs = d.refs.filter((x) => x.id !== ref_id);
      emitDeck(session, deck, { type: "refs_changed", refs: structuredClone(d.refs) });
      return null;
    },

    refs_remove_unused: ({ session, deck }) => {
      const d = deckOf(session, deck);
      const unused = d.refs.filter((r) => r.cited_by.length === 0 && r.parent === null).map((r) => r.id);
      d.refs = d.refs.filter((r) => !unused.includes(r.id));
      emitDeck(session, deck, { type: "refs_changed", refs: structuredClone(d.refs) });
      return unused;
    },

    refs_search: ({ session, deck, query }) => {
      const d = deckOf(session, deck);
      const q = query.toLowerCase();
      return d.refs
        .filter((r) => r.title.toLowerCase().includes(q) || q.length > 0)
        .slice(0, 12)
        .map((r) => ({ cite: `ref:${r.id}@${r.version}#p1`, title: r.title, heading_path: [], snippet: `… ${query} …` }));
    },

    source_passage: ({ session, deck, cite }) => {
      const d = deckOf(session, deck);
      const id = cite.replace(/^ref:/, "").split("@")[0];
      const r = d.refs.find((x) => x.id === id);
      if (!r) throw ipcError("not_found", `no reference ${id}`);
      return { cite, ref_title: r.title, kind: r.kind, text: "The passage behind the citation.", highlight: null, page_png: null, table: null, highlight_cells: null };
    },

    assets_list: ({ session, deck }) => structuredClone(deckOf(session, deck).assets),

    asset_add: ({ session, deck, path_abs }) => {
      const d = deckOf(session, deck);
      const ext = path_abs.split(".").pop()?.toLowerCase() ?? "";
      if (!["png", "jpg", "jpeg", "gif", "svg"].includes(ext)) throw ipcError("invalid", "only images can be added as assets");
      const sha = hashOf(path_abs).padEnd(16, "0");
      const asset = { path: `assets/${sha}.${ext}`, sha256: sha, width: 800, height: 600 };
      d.assets.push(asset);
      return structuredClone(asset);
    },

    exports_list: ({ session, deck }) => structuredClone(deckOf(session, deck).exports),

    export_run: ({ session, deck, format }) => {
      const d = deckOf(session, deck);
      return task(session, deck, `Exporting ${format}`, () => {
        if (format === "pdf" || format === "png") throw ipcError("engine_unavailable", "no rendering engine in the mock");
        const ext = format === "notes-docx" ? "docx" : format;
        const view: Deck.ExportView = {
          id: `x-${env.nextId()}`,
          format,
          preset: null,
          path: `exports/${deck}.${ext}`,
          checkpoint: checkpoint(session, `export ${format}`),
          created_at: new Date().toISOString(),
          errors: 0,
          warnings: 0,
          engine: null,
        };
        d.exports.unshift(view);
        emitDeck(session, deck, { type: "export_done", export: structuredClone(view) });
        return view.path;
      });
    },

    export_reveal: ({ session, deck, export_id }) => {
      if (!deckOf(session, deck).exports.some((e) => e.id === export_id)) throw ipcError("not_found", "no such export");
      return null;
    },

    export_open: ({ session, deck, export_id }) => {
      if (!deckOf(session, deck).exports.some((e) => e.id === export_id)) throw ipcError("not_found", "no such export");
      return null;
    },

    history_list: ({ session, limit }) => {
      of(session);
      const list = checkpoints.get(session) ?? [];
      return structuredClone(limit ? list.slice(0, Math.max(1, limit)) : list);
    },

    history_restore: ({ session, checkpoint: id }) => {
      of(session);
      if (!(checkpoints.get(session) ?? []).some((c) => c.id === id)) throw ipcError("not_found", `no checkpoint \`${id}\``);
      checkpoint(session, `before restoring ${id}`);
      env.emit(session, { type: "decks_changed", decks: [...of(session).values()].map((x) => structuredClone(x.summary)) });
      return null;
    },

    history_revert_turn: ({ session, turn }) => {
      of(session);
      const cp = (checkpoints.get(session) ?? []).find((c) => c.turn === turn);
      if (!cp) throw ipcError("not_found", `no checkpoint was taken for turn \`${turn}\``);
      checkpoint(session, `before restoring ${cp.id}`);
      return structuredClone(cp);
    },

    artifacts_list: ({ session, run_id, kind, limit }) => {
      of(session);
      const list = env.artifacts(session).filter((a) => (!run_id || a.run_id === run_id) && (!kind || a.kind === kind));
      return structuredClone(limit ? list.slice(0, limit) : list);
    },
  };

  const agent: DeckAgentApi = {
    firstSlide: (session) => {
      const m = decks.get(session);
      if (!m) return null;
      for (const d of m.values()) {
        const id = order(d)[0];
        const entry = id ? d.slides.get(id) : undefined;
        if (entry) return { deck: d.summary.slug, slide: structuredClone(entry.slide) };
      }
      return null;
    },
    applySlide: (session, deck, slide) => {
      const d = deckOf(session, deck);
      d.slides.set(slide.id, { slide: structuredClone(slide), hash: hashOf(slide) });
      changed(session, d, [slide.id], [], "agent");
    },
  };
  return { handlers, agent };
}
