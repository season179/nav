//! The coding-agent harness: how nav thinks, acts, checks, and learns.
//!
//! This crate should not know about HTTP, SSE, JSON-RPC, Bubble Tea, Electron,
//! or browser APIs. It is the backend's product brain, not its transport.

pub mod agents;
pub mod context;
pub mod events;
pub mod guardrails;
pub mod integrations;
pub mod models;
pub mod observability;
pub mod runtime;
pub mod sessions;
pub mod skills;
pub mod tools;
pub mod verification;
pub mod workspace;

pub use runtime::{BackendInfo, Harness};
