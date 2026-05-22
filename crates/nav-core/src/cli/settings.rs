//! Merge project/user settings with parsed CLI flags.
//!
//! The important rule is simple: a flag the user typed wins over settings, and
//! settings only fill in values that came from clap defaults.

use clap::{CommandFactory, FromArgMatches, parser::ValueSource};
use std::collections::HashSet;

use crate::context::Settings;

use super::Args;

/// Set of argument IDs whose value came from an explicit user-supplied flag
/// rather than clap's default. [`Args::apply_settings`] uses it to skip
/// fields the user already provided on the command line, so the precedence
/// chain is: explicit CLI > project settings > user settings > clap default.
#[derive(Debug, Clone, Default)]
pub struct ProvidedArgs(HashSet<String>);

impl ProvidedArgs {
    pub fn was_provided(&self, name: &str) -> bool {
        self.0.contains(name)
    }
}

impl Args {
    /// Like `Args::parse`, but also returns the set of argument IDs the user
    /// supplied explicitly. Pair with [`Args::apply_settings`] to merge a
    /// `.nav/settings.json` without clobbering flags the user actually typed.
    pub fn parse_with_sources() -> (Self, ProvidedArgs) {
        let matches = Args::command().get_matches();
        Self::from_matches_with_sources(matches)
            .expect("clap matches must round-trip through FromArgMatches")
    }

    pub(super) fn from_matches_with_sources(
        matches: clap::ArgMatches,
    ) -> Result<(Self, ProvidedArgs), clap::Error> {
        let mut provided: HashSet<String> = HashSet::new();
        for id in matches.ids() {
            if matches.value_source(id.as_str()) == Some(ValueSource::CommandLine) {
                provided.insert(id.as_str().to_string());
            }
        }
        let args = Args::from_arg_matches(&matches)?;
        Ok((args, ProvidedArgs(provided)))
    }

    /// Fills in `Args` fields that clap defaulted from `settings`. Any field
    /// the user passed on the CLI (tracked via `provided`) is left untouched.
    pub fn apply_settings(&mut self, settings: &Settings, provided: &ProvidedArgs) {
        if let Some(model) = settings.model.as_deref()
            && !provided.was_provided("model")
        {
            self.model = model.to_string();
        }
        if let Some(auth) = settings.auth
            && !provided.was_provided("auth")
        {
            self.auth = auth;
        }
        if let Some(transport) = settings.transport
            && !provided.was_provided("transport")
        {
            self.transport = transport;
        }
        if let Some(max_turns) = settings.max_turns
            && !provided.was_provided("max_turns")
        {
            self.max_turns = max_turns;
        }
        if let Some(budget) = settings.tool_call_soft_budget
            && !provided.was_provided("tool_call_soft_budget")
        {
            self.tool_call_soft_budget = budget;
        }
        if let Some(secs) = settings.bash_timeout_secs
            && !provided.was_provided("bash_timeout_secs")
        {
            self.bash_timeout_secs = secs;
        }
        if let Some(limit) = settings.auto_compact_token_limit
            && !provided.was_provided("auto_compact_token_limit")
        {
            self.auto_compact_token_limit = limit;
        }
        if let Some(fraction) = settings.auto_compact_fraction
            && !provided.was_provided("auto_compact_fraction")
        {
            self.auto_compact_fraction = fraction;
        }
        if let Some(budget) = settings.ambient_context_token_budget
            && !provided.was_provided("ambient_context_token_budget")
        {
            self.ambient_context_token_budget = budget;
        }
        if provided.was_provided("no_git_checkpoints") {
            self.git_checkpoints = false;
        } else if let Some(enabled) = settings.git_checkpoints
            && !provided.was_provided("git_checkpoints")
        {
            self.git_checkpoints = enabled;
        }
        if let Some(effort) = settings.reasoning_effort
            && !provided.was_provided("reasoning_effort")
        {
            self.reasoning_effort = Some(effort);
        }
    }
}
