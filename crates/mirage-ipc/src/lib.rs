//! Protocol between the Mirage UI process and the TUN helper service
//! (DESIGN.md §11.7), plus the configuration policy both sides must apply.
//!
//! The service runs as SYSTEM, so it never trusts the caller: the app's
//! composer uses [`policy::apply`] to build the run configuration, and the
//! service runs the same function again on the YAML it receives over the pipe.
//! One implementation, two callers — the two can never drift apart.

pub mod policy;
pub mod protocol;

pub use policy::{ManagedOverlay, OVERLAY_VERSION, TunOverlay, TunStack};
pub use protocol::{
    MAX_FRAME_BYTES, PIPE_NAME, PROTOCOL_VERSION, Request, Response, ServiceStatus,
};

#[cfg(windows)]
pub mod atomic;
pub mod binary;
#[cfg(windows)]
pub mod known_folder;
pub mod rolling;
pub mod service;
