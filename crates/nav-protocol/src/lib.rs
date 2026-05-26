//! Wire-level types shared by backend transports and future frontends.
//!
//! Keep HTTP, SSE, and JSON-RPC shapes here. Agent behavior belongs in
//! `nav-harness`; this crate is only the language spoken at the boundary.

pub mod events;
pub mod rpc;

pub use events::{
    BACKEND_EVENT_TYPES, BackendEvent, EventEnvelope, ProviderEventMetadata, ProviderUsage,
};
pub use rpc::{JsonRpcError, JsonRpcRequest, JsonRpcResponse};
