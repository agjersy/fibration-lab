//! The deck workbench as a library of the one core (`fibration-core`): the deck store, the
//! reference index, the deck kernel client, the deck tools the agent loop registers, and the
//! deck service the UI commands call. It has no agent loop, no home directory, and no config
//! of its own: the session that owns a deck supplies them.

pub mod deck_config;
pub mod deck_store;
pub mod index;
pub mod kernel;
pub mod project;
pub mod refs;
pub mod service;
pub mod tools;
pub mod util;
pub mod workspace;
