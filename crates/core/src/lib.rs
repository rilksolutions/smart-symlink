//! Shared core for DirLinker: used by both the GUI (Tauri) and the terminal UI.
//!
//! - [`fsinfo`]  drives, volume / filesystem detection, capability probing
//! - [`listing`] directory listing with link detection
//! - [`size`]    recursive (parallel, cancellable, cached) folder sizes
//! - [`links`]   which link types are possible for a given selection, with explanations
//! - [`ops`]     the actual move + link operations (copy, verify, swap, rollback)

pub mod fsinfo;
pub mod links;
pub mod listing;
pub mod ops;
pub mod registry;
pub mod size;
pub mod util;

pub use links::{LinkKind, Mode};
