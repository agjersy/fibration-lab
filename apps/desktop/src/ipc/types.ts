// IPC types are generated from crates/protocol by ts-rs (`cargo test -p fibration-protocol`);
// they are the only copy of the contract's types on this side. The command table
// (`commands.ts`) is generated from the Rust one by `fibration_core::commands`'s tests.
export * from "./generated";
export type { TurnSummaryView as TurnSummary } from "./generated";
// Deck types (ts-rs export of fibration-deck-protocol, `cargo test -p fibration-deck-protocol`):
// `Deck.DeckView`, `Deck.SlideView`, `Deck.DeckEvent`, ... Several names (Diagnostic,
// SearchHit, FileContent, IpcError) exist in both sets, so the deck ones live in a namespace.
export type * as Deck from "./generated/deck";
