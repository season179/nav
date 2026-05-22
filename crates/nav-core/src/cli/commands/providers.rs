//! `nav providers` subcommands.
//!
//! `list_providers` walks the merged catalog and reports one line per
//! provider — id, display name, base URL, and whether the configured
//! credential resolves locally.

use clap::Subcommand;
use serde::Serialize;

use crate::context::ProviderCatalog;
use crate::model::resolve_value::resolve_value;

/// Actions under `nav providers`.
#[derive(Subcommand, Debug, Clone, PartialEq, Eq)]
pub enum ProvidersAction {
    /// One line per provider with its id, display name, base URL, and
    /// credential resolvability.
    List {
        /// Emit a JSON array instead of the line-per-provider text output.
        #[arg(long)]
        json: bool,
    },
}

/// One row of `nav providers list` output. Stable shape suitable for both
/// the text renderer and `--json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProviderLine {
    /// Provider id (the catalog map key).
    pub id: String,
    /// Provider display name, or the id when `name` is unset.
    pub display_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    /// `true` when the configured `api_key` resolves to a non-empty value
    /// (literal, env, or `!command`). `false` when an `api_key` is set but
    /// cannot resolve (env unset and not a literal, command exited non-zero,
    /// or resolved value is empty). Providers without an `api_key` report
    /// `true` — local providers like Ollama don't need a credential.
    pub credential_resolvable: bool,
    /// `true` when an `api_key` is configured for this provider. The
    /// renderer uses this to distinguish `credential resolvable: n/a`
    /// (no key configured, e.g. local Ollama) from `yes`/`no`.
    pub credential_configured: bool,
}

/// Flatten the merged providers catalog into one row per provider, ordered
/// by id (the `BTreeMap` already iterates sorted).
///
/// Credential resolvability is checked via [`resolve_value`], which can
/// shell out for `!command` entries. The shell-command cache is
/// process-local, so each fresh `nav providers list` invocation re-runs
/// every `!command` from cold; the cache only amortizes within a single
/// process. Errors from `resolve_value` are surfaced on stderr so users
/// can see *why* a credential failed to resolve.
pub fn list_providers(catalog: Option<&ProviderCatalog>) -> Vec<ProviderLine> {
    let Some(catalog) = catalog else {
        return Vec::new();
    };
    catalog
        .iter()
        .map(|(id, provider)| {
            let display_name = provider.name.clone().unwrap_or_else(|| id.clone());
            let (credential_configured, credential_resolvable) = match provider.api_key.as_deref() {
                None => (false, true),
                Some(raw) => (true, credential_resolves(id, raw)),
            };
            ProviderLine {
                id: id.clone(),
                display_name,
                base_url: provider.base_url.clone(),
                credential_resolvable,
                credential_configured,
            }
        })
        .collect()
}

/// Resolve the raw `api_key` string and report whether it produced a
/// non-empty value. A failed `resolve_value` (shell-command error,
/// timeout) is reported on stderr so `nav providers list` doubles as a
/// diagnostic — `credential=no` alone would leave operators guessing.
fn credential_resolves(provider_id: &str, raw: &str) -> bool {
    match resolve_value(raw) {
        Ok(Some(value)) => !value.is_empty(),
        Ok(None) => false,
        Err(err) => {
            eprintln!("nav: provider `{provider_id}` credential check failed: {err}");
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::ProviderConfig;
    use std::collections::BTreeMap;

    /// RAII guard that sets an env var on creation and removes it on drop.
    struct EnvVarGuard {
        key: String,
    }

    impl EnvVarGuard {
        fn new(key: impl Into<String>, value: &str) -> Self {
            let key = key.into();
            // SAFETY: test-only, unique key; no other thread reads this var.
            unsafe { std::env::set_var(&key, value) };
            Self { key }
        }

        fn key(&self) -> &str {
            &self.key
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            unsafe { std::env::remove_var(&self.key) };
        }
    }

    fn provider(
        name: Option<&str>,
        base_url: Option<&str>,
        api_key: Option<&str>,
    ) -> ProviderConfig {
        ProviderConfig {
            name: name.map(str::to_owned),
            base_url: base_url.map(str::to_owned),
            api_key: api_key.map(str::to_owned),
            headers: None,
            models: BTreeMap::new(),
        }
    }

    #[test]
    fn list_providers_returns_empty_when_catalog_missing() {
        assert!(list_providers(None).is_empty());
    }

    #[test]
    fn ordered_by_provider_id() {
        let mut catalog = ProviderCatalog::new();
        catalog.insert(
            "z.ai".into(),
            provider(Some("Z.AI"), Some("https://z"), None),
        );
        catalog.insert("ollama".into(), provider(None, Some("http://local"), None));
        let lines = list_providers(Some(&catalog));
        assert_eq!(lines[0].id, "ollama");
        assert_eq!(lines[1].id, "z.ai");
    }

    #[test]
    fn display_name_falls_back_to_id() {
        let mut catalog = ProviderCatalog::new();
        catalog.insert("ollama".into(), provider(None, Some("http://local"), None));
        let lines = list_providers(Some(&catalog));
        assert_eq!(lines[0].display_name, "ollama");
    }

    #[test]
    fn missing_api_key_reports_no_credential_configured() {
        let mut catalog = ProviderCatalog::new();
        catalog.insert("ollama".into(), provider(None, Some("http://local"), None));
        let lines = list_providers(Some(&catalog));
        assert!(!lines[0].credential_configured);
        assert!(lines[0].credential_resolvable);
    }

    #[test]
    fn env_backed_api_key_resolves_when_set() {
        let guard = EnvVarGuard::new(
            format!("NAV_TEST_PROVIDERS_OK_{}", std::process::id()),
            "secret",
        );
        let mut catalog = ProviderCatalog::new();
        catalog.insert(
            "z.ai".into(),
            provider(Some("Z.AI"), Some("https://z"), Some(guard.key())),
        );
        let lines = list_providers(Some(&catalog));
        assert!(lines[0].credential_configured);
        assert!(lines[0].credential_resolvable);
    }

    #[test]
    fn literal_api_key_resolves_as_itself() {
        // A literal like `sk-...` always resolves (env-over-literal falls
        // through to the literal branch when no env var of that name is set).
        let mut catalog = ProviderCatalog::new();
        let unique_literal = format!("sk-literal-{}", std::process::id());
        catalog.insert(
            "openai".into(),
            provider(
                Some("OpenAI"),
                Some("https://api.openai.com/v1"),
                Some(&unique_literal),
            ),
        );
        let lines = list_providers(Some(&catalog));
        assert!(lines[0].credential_configured);
        assert!(lines[0].credential_resolvable);
    }

    #[test]
    fn empty_api_key_is_unresolvable() {
        // An empty-string `api_key` (e.g., from a templating tool that
        // didn't substitute) round-trips through resolve_value as
        // Ok(Some("")). The list command should flag that as unresolvable
        // rather than report a green credential for an empty key.
        let mut catalog = ProviderCatalog::new();
        catalog.insert("openai".into(), provider(None, Some("https://x"), Some("")));
        let lines = list_providers(Some(&catalog));
        assert!(lines[0].credential_configured);
        assert!(!lines[0].credential_resolvable);
    }
}
