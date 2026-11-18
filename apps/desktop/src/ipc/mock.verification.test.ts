import { describe, expect, it } from "vitest";
import { queueOrder } from "../ui/panels/VerifyPanel";
import { MockBackend } from "./mock";
import type { SessionEvent, VerificationView } from "./types";

function fast(): MockBackend {
  return new MockBackend({ latency: [0, 0], timeScale: 0, seedSession: false });
}

function waitFor<T>(check: () => T | undefined, timeoutMs = 3000): Promise<T> {
  return new Promise((resolve, reject) => {
    const start = Date.now();
    const tick = () => {
      const v = check();
      if (v !== undefined) resolve(v);
      else if (Date.now() - start > timeoutMs) reject(new Error("timeout waiting for condition"));
      else setTimeout(tick, 2);
    };
    tick();
  });
}

describe("MockBackend verification ledger", () => {
  it("verifies an equation, resets it when the file changes, and lists deck gaps before export", async () => {
    const b = fast();
    const info = await b.invoke("session_create", { repo_abs: "/home/user/projects/decay-analysis", target: "local", init: true });
    const session = info.id;
    const events: SessionEvent[] = [];
    await b.subscribe(session, (e) => events.push(e));
    await waitFor(() => events.find((e) => e.type === "kernel_state" && e.state === "ready"));
    const item = { kind: "equation" as const, path: "analysis/models.py", qualname: "single_exp" };

    const v = await b.invoke("verification_set", { session, item, state: "human_verified", note: "by hand" });
    expect(v).toMatchObject({ key: "equation:analysis/models.py#single_exp", state: "human_verified", stale: false });
    expect(events.some((e) => e.type === "verification_changed")).toBe(true);
    expect((await b.invoke("fs_read", { session, path: ".analyst/verification.jsonl" })).content).toContain("human_verified");

    const file = await b.invoke("fs_read", { session, path: "analysis/models.py" });
    await b.invoke("fs_write", { session, path: "analysis/models.py", content: `${file.content}\n# changed\n`, expected_hash: file.hash });
    const after = (await b.invoke("verification_list", { session })).find((x) => x.key === v.key)!;
    expect(after).toMatchObject({ state: "unverified", stale: true, entry: null });
    await expect(b.invoke("verification_set", { session, item: { ...item, path: "nope.py" }, state: "human_verified" })).rejects.toMatchObject({ code: "not_found" });

    // An equation on a slide: the deck depends on it and on the claim, until both are verified.
    await b.invoke("deck_create", { session, title: "Results", template_abs: "/t/corp.potx" });
    const slide = await b.invoke("slide_create", { session, deck: "results", layout: "Title and Content" });
    await b.invoke("slide_place_equation", { session, deck: "results", slide_id: slide.slide.id, path: "analysis/models.py", function: "single_exp" });
    const gaps = await b.invoke("deck_verification_gaps", { session, deck: "results" });
    expect(gaps.map((g) => g.item.kind).sort()).toEqual(["equation", "slide_claim"]);
    for (const g of gaps) await b.invoke("verification_set", { session, item: g.item, state: "human_verified" });
    expect(await b.invoke("deck_verification_gaps", { session, deck: "results" })).toEqual([]);
  });

  it("orders the queue: used on slides first, grouped by kind, verified items left out", () => {
    const v = (key: string, kind: VerificationView["item"]["kind"], onSlides: boolean, state: VerificationView["state"] = "unverified"): VerificationView => ({
      key,
      item: kind === "equation" ? { kind, path: key, qualname: "f" } : kind === "figure" ? { kind, run_id: "r", artifact: key } : { kind: "code_hunk", proposal_id: key },
      label: key,
      state,
      stale: false,
      hash: "h",
      entry: null,
      on_slides: onSlides ? [{ deck: "d", slide_id: "s-1", element_id: "e-1" }] : [],
    });
    const groups = queueOrder([v("a", "figure", false), v("b", "equation", true), v("c", "figure", true), v("d", "equation", false, "human_verified"), v("e", "code_hunk", false)]);
    expect(groups.map((g) => [g.title, g.items.map((i) => i.key)])).toEqual([
      ["Equations on slides", ["b"]],
      ["Figures on slides", ["c"]],
      ["Figures", ["a"]],
      ["Code changes", ["e"]],
    ]);
  });
});
