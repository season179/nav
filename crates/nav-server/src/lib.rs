//! Transport layer for nav frontends.
//!
//! This crate translates frontend protocols into harness calls. It owns local
//! HTTP, JSON-RPC routing, SSE streaming, auth, and the temporary stdio bridge.

pub mod bootstrap;
pub mod http;
pub mod stdio;
