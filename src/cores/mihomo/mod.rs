//! Official Mihomo adapter (DESIGN.md §10).
//!
//! M1 implements it as a concrete type; the `CoreAdapter`/`CoreLauncher`
//! traits from §6.3/§6.4 are extracted when the service launcher (M7) or a
//! second core arrives, so no speculative indirection exists before then.

pub mod binary;
pub mod check;
pub mod compose;
pub mod controller;
pub mod process;
