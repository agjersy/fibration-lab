// IPC types are generated from crates/protocol by ts-rs (`cargo test -p analyst-protocol`).
// docs/IPC.md is the contract; types.manual.ts mirrors it and types.compat.test.ts checks
// that the generated export matches it in both directions.
export * from "./generated";
export type { TurnSummaryView as TurnSummary } from "./generated";
// Deck types (ts-rs export of analyst-deck-protocol, `cargo test -p analyst-deck-protocol`):
// `Deck.DeckView`, `Deck.SlideView`, `Deck.DeckEvent`, ... Several names (Diagnostic,
// SearchHit, FileContent, IpcError) exist in both sets, so the deck ones live in a namespace.
export type * as Deck from "./generated/deck";
