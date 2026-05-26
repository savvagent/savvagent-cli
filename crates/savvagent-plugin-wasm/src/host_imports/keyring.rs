//! `keyring-capability` host import for provider plugins.
//!
//! The WIT contract is deliberately narrow: there is exactly one operation,
//! `get(account) -> result<string, keyring-error>`, and the keyring service
//! name is *not* a parameter — it's hardcoded to `"savvagent"`. That keeps
//! every plugin sharing one keyring namespace with the host's own
//! `/connect` flow, and removes "name-collision via crafted service" as
//! an attack vector.
//!
//! ## Allow-list
//!
//! The plugin's `plugin.toml` `[security] keyring-accounts = [...]`
//! declares which account names the plugin is permitted to read. Accounts
//! absent from that list surface as
//! [`wit::KeyringError::Denied`] *before* any keyring backend is
//! consulted; this means a plugin without a manifest-declared account
//! cannot even probe whether an account exists.
//!
//! ## Backend errors
//!
//! `keyring::Error::NoEntry` maps to [`wit::KeyringError::NotFound`].
//! Everything else (the backend isn't available, the secret-service
//! daemon isn't running, etc.) maps to [`wit::KeyringError::Backend`]
//! with the upstream error stringified. The host never panics out of
//! this path.
//!
//! ## Tests
//!
//! Unit tests here exercise only the allow-list path — they don't
//! touch the real OS keyring (CI runners and many dev boxes don't have
//! a backend configured, and even when they do, mutating the real
//! keyring from a unit test would be hostile).

use std::sync::Arc;

use crate::provider_world::savvagent::plugin::keyring_capability as wit;

/// Service identifier under which all savvagent-managed credentials live.
/// **Not** configurable by plugins; see the module docs for why.
pub const SAVVAGENT_KEYRING_SERVICE: &str = "savvagent";

/// Per-store keyring state. Holds the manifest-derived allow-list.
/// Cloning is cheap (internal `Arc`).
#[derive(Clone)]
pub struct KeyringState {
    /// Account names the plugin is permitted to read. Empty list = the
    /// plugin can never call `get` successfully.
    pub allowed_accounts: Arc<Vec<String>>,
}

impl KeyringState {
    /// Construct a fresh state for one plugin's per-call Store.
    pub fn new(allowed_accounts: Vec<String>) -> Self {
        Self {
            allowed_accounts: Arc::new(allowed_accounts),
        }
    }

    /// Look up `account` under the savvagent service.
    ///
    /// Returns:
    /// - `Ok(secret)` when the account is in the allow-list and the
    ///   backend has a stored entry;
    /// - [`wit::KeyringError::Denied`] when the account isn't in the
    ///   allow-list (checked *before* touching the backend);
    /// - [`wit::KeyringError::NotFound`] when the backend has no entry;
    /// - [`wit::KeyringError::Backend`] for any other backend failure.
    pub fn get(&self, account: &str) -> Result<String, wit::KeyringError> {
        if !self.allowed_accounts.iter().any(|a| a == account) {
            return Err(wit::KeyringError::Denied(account.to_string()));
        }
        let entry = keyring::Entry::new(SAVVAGENT_KEYRING_SERVICE, account)
            .map_err(|e| wit::KeyringError::Backend(e.to_string()))?;
        match entry.get_password() {
            Ok(s) => Ok(s),
            Err(keyring::Error::NoEntry) => Err(wit::KeyringError::NotFound),
            Err(e) => Err(wit::KeyringError::Backend(e.to_string())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_allow_list_denies_every_account() {
        let s = KeyringState::new(Vec::new());
        let err = s.get("anthropic").unwrap_err();
        assert!(matches!(err, wit::KeyringError::Denied(a) if a == "anthropic"));
    }

    #[test]
    fn unlisted_account_denied_even_when_others_are_allowed() {
        let s = KeyringState::new(vec!["openai".into()]);
        let err = s.get("anthropic").unwrap_err();
        assert!(matches!(err, wit::KeyringError::Denied(a) if a == "anthropic"));
    }

    #[test]
    fn savvagent_service_constant_is_stable() {
        // Locked at "savvagent" so the value matches what `/connect`
        // already writes. A change here is a breaking change for every
        // existing user's keyring entries — it should be a deliberate
        // multi-step migration, not a typo.
        assert_eq!(SAVVAGENT_KEYRING_SERVICE, "savvagent");
    }
}
