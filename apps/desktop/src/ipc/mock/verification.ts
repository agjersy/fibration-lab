// The verification ledger of the MockBackend (IPC.md section 10): entries kept per session and
// written to `.analyst/verification.jsonl` in the session's files, effective state computed
// against each item's content now (so editing a file resets what was verified about it).
import type { Commands } from "../commands";
import type { ArtifactEntry, Deck, IpcError, LedgerEntry, SessionEvent, SessionId, SlideUse, VerificationView, VerifyItem } from "../types";
import { hashOf } from "./decks";

type VerificationCommand = "verification_list" | "verification_set" | "deck_verification_gaps";

export type VerificationHandlers = {
  [K in VerificationCommand]: (args: Commands[K]["args"]) => Commands[K]["result"];
};

export interface VerificationMockEnv {
  requireOpen: (session: SessionId) => void;
  files: (session: SessionId) => Map<string, string>;
  artifacts: (session: SessionId) => ArtifactEntry[];
  slides: (session: SessionId) => { deck: string; slide: Deck.Slide }[];
  emit: (session: SessionId, event: SessionEvent) => void;
}

const LEDGER = ".analyst/verification.jsonl";

function ipcError(code: IpcError["code"], message: string): IpcError {
  return { code, message };
}

export function itemKey(item: VerifyItem): string {
  switch (item.kind) {
    case "equation":
      return `equation:${item.path}#${item.qualname}`;
    case "figure":
      return `figure:${item.run_id}/${item.artifact}`;
    case "result":
      return `result:${item.run_id}/${item.name}`;
    case "slide_claim":
      return `slide_claim:${item.deck}/${item.slide_id}/${item.element_id}`;
    case "code_hunk":
      return `code_hunk:${item.proposal_id}`;
  }
}

type Element = { id: string; [k: string]: unknown };

function elementsOf(slide: Deck.Slide): Element[] {
  const out: Element[] = [];
  for (const c of Object.values(slide.placeholders)) if (c && c.type !== "empty") out.push(c as unknown as Element);
  for (const e of slide.elements) out.push({ ...(e.content as unknown as Element), id: e.id });
  return out;
}

function boundRuns(el: Element): Deck.Run[] {
  const text = el.text as Deck.Text | undefined;
  return (text?.paragraphs ?? []).flatMap((p) => p.runs).filter((r) => r.run_cite);
}

function citesAnalysis(el: Element): boolean {
  return Boolean(el.run_cite) || Boolean(el.eq_cite) || boundRuns(el).length > 0;
}

export function makeVerificationMock(env: VerificationMockEnv): VerificationHandlers {
  const ledgers = new Map<SessionId, LedgerEntry[]>();
  const ledger = (session: SessionId) => {
    let l = ledgers.get(session);
    if (!l) {
      l = [];
      ledgers.set(session, l);
    }
    return l;
  };

  const hash = (session: SessionId, item: VerifyItem): string | null => {
    const files = env.files(session);
    switch (item.kind) {
      case "equation": {
        const content = files.get(item.path);
        return content === undefined ? null : hashOf(content);
      }
      case "figure":
        return env.artifacts(session).some((a) => a.run_id === item.run_id && a.path === item.artifact) ? hashOf(`${item.run_id}/${item.artifact}`) : null;
      case "result": {
        const a = env.artifacts(session).find((x) => x.run_id === item.run_id && x.kind === "results");
        const r = a?.results?.find((x) => x.name === item.name);
        return r ? hashOf(r) : null;
      }
      case "slide_claim": {
        const s = env.slides(session).find((x) => x.deck === item.deck && x.slide.id === item.slide_id);
        const el = s ? elementsOf(s.slide).find((e) => e.id === item.element_id) : undefined;
        return el ? hashOf(el) : null;
      }
      case "code_hunk":
        return hashOf(item.proposal_id);
    }
  };

  const label = (session: SessionId, item: VerifyItem): string => {
    switch (item.kind) {
      case "equation":
        return `${item.qualname} in ${item.path}`;
      case "figure":
        return `${item.artifact.split("/").pop() ?? item.artifact} of run ${item.run_id}`;
      case "result": {
        const a = env.artifacts(session).find((x) => x.run_id === item.run_id && x.kind === "results");
        const r = a?.results?.find((x) => x.name === item.name);
        return r ? `${item.name} = ${r.value}${r.unit ? ` ${r.unit}` : ""} (run ${item.run_id})` : `${item.name} of run ${item.run_id}`;
      }
      case "slide_claim":
        return `element ${item.element_id} on ${item.deck}/${item.slide_id}`;
      case "code_hunk":
        return `change ${item.proposal_id}`;
    }
  };

  const view = (session: SessionId, item: VerifyItem, onSlides: SlideUse[]): VerificationView => {
    const key = itemKey(item);
    const h = hash(session, item);
    const mine = ledger(session).filter((e) => e.key === key);
    const entry = h === null ? undefined : [...mine].reverse().find((e) => e.hash === h);
    return { key, item, label: label(session, item), state: entry ? entry.state : "unverified", stale: !entry && mine.length > 0, hash: h, entry: entry ? structuredClone(entry) : null, on_slides: onSlides };
  };

  const list = (session: SessionId): VerificationView[] => {
    const items = new Map<string, { item: VerifyItem; used: SlideUse[] }>();
    const add = (item: VerifyItem, used?: SlideUse) => {
      const key = itemKey(item);
      const cur = items.get(key) ?? { item, used: [] };
      if (used && !cur.used.some((u) => u.deck === used.deck && u.slide_id === used.slide_id && u.element_id === used.element_id)) cur.used.push(used);
      items.set(key, cur);
    };
    for (const { deck, slide } of env.slides(session)) {
      for (const el of elementsOf(slide)) {
        if (!citesAnalysis(el)) continue;
        const used = { deck, slide_id: slide.id, element_id: el.id };
        add({ kind: "slide_claim", ...used }, used);
        const rc = el.run_cite as Deck.RunCite | undefined;
        if (rc) add({ kind: "figure", run_id: rc.run_id, artifact: rc.artifact }, used);
        const eq = el.eq_cite as Deck.EqCite | undefined;
        if (eq) add({ kind: "equation", path: eq.path, qualname: eq.qualname }, used);
        for (const r of boundRuns(el)) if (r.run_cite) add({ kind: "result", run_id: r.run_cite.run_id, name: r.run_cite.result_name }, used);
      }
    }
    for (const a of env.artifacts(session)) {
      if (a.kind === "figure") add({ kind: "figure", run_id: a.run_id, artifact: a.path });
      if (a.kind === "results") for (const r of a.results ?? []) add({ kind: "result", run_id: a.run_id, name: r.name });
    }
    for (const e of ledger(session)) add(e.item);
    const rank: Record<VerifyItem["kind"], number> = { slide_claim: 0, equation: 1, figure: 2, result: 3, code_hunk: 4 };
    return [...items.values()]
      .map(({ item, used }) => view(session, item, used))
      .sort((a, b) => Number(a.on_slides.length === 0) - Number(b.on_slides.length === 0) || rank[a.item.kind] - rank[b.item.kind] || a.label.localeCompare(b.label));
  };

  return {
    verification_list: ({ session }) => {
      env.requireOpen(session);
      return list(session);
    },
    verification_set: ({ session, item, state, note }) => {
      env.requireOpen(session);
      const h = hash(session, item);
      if (h === null) throw ipcError("not_found", `\`${itemKey(item)}\` cannot be read now, so there is nothing to verify`);
      const entry: LedgerEntry = { item, key: itemKey(item), hash: h, state, by: "user", at: new Date().toISOString().replace(/\.\d+Z$/, "Z"), ...(note?.trim() ? { note: note.trim() } : {}) };
      ledger(session).push(entry);
      const files = env.files(session);
      files.set(LEDGER, `${files.get(LEDGER) ?? ""}${JSON.stringify(entry)}\n`);
      env.emit(session, { type: "verification_changed", key: entry.key });
      return view(session, item, []);
    },
    deck_verification_gaps: ({ session, deck }) => {
      env.requireOpen(session);
      return list(session).filter((v) => v.on_slides.some((u) => u.deck === deck) && v.state !== "human_verified");
    },
  };
}
