//! Chat Completions request body construction.
//!
//! Filled in by C1. The signature mirrors
//! [`crate::model::responses::request::response_body_with_options`] so the
//! agent loop can hand the same `(args, cwd, input, skills, context)` tuple to
//! either backend.

use crate::cli::Args;
use crate::context::{Catalog, ProjectContext};
use crate::model::auth::ResolvedProvider;
use serde_json::Value;
use std::path::Path;

/// Build the JSON body for `POST {base_url}/chat/completions`.
///
/// Stub: filled in by C1.
#[allow(dead_code)]
pub(crate) fn build_request_body(
    _args: &Args,
    _resolved: &ResolvedProvider,
    _cwd: &Path,
    _input: &[Value],
    _skills: &Catalog,
    _context: Option<&ProjectContext>,
) -> Value {
    unimplemented!("Chat Completions request body lands in C1")
}
