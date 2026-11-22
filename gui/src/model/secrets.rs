//! Secrets — passwords and API keys — live in the OS credential store (Windows Credential
//! Manager), never in profiles, history, logs or previews.
//!
//! Service `siteone-crawler-gui`, account `<scope>/<option>`, where the scope is a profile slug or
//! `session` (the working configuration), e.g. `session/--http-auth`. A data folder set with
//! `SITEONE_GUI_DATA_DIR` has a service of its own ([`service_name`]).

use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::path::Path;
use std::sync::Mutex;

use super::config::{AI_API_KEY, MASK};

/// The credential-store service name.
pub const SERVICE: &str = "siteone-crawler-gui";

/// The credential-store service of a data folder: [`SERVICE`] for the default folders; for a
/// folder set with `SITEONE_GUI_DATA_DIR`, a name of its own (`siteone-crawler-gui <hash of the
/// folder>`). Profiles and the session live in the data folder, so two data folders must not
/// read, overwrite or delete each other's `session/…` and `<profile>/…` secrets.
pub fn service_name(override_dir: Option<&Path>) -> String {
    let Some(dir) = override_dir else {
        return SERVICE.to_string();
    };
    // FNV-1a over the lower-cased path (Windows paths do not distinguish case): stable across
    // runs and versions, which a std hasher is not.
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in dir.to_string_lossy().to_lowercase().bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{SERVICE} {hash:016x}")
}

/// The scope of the working configuration's secrets.
pub const SESSION_SCOPE: &str = "session";

/// The account a secret is stored under.
pub fn account(scope: &str, option: &str) -> String {
    format!("{scope}/{option}")
}

/// An error of the credential store. Its message never contains a secret value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretError(pub String);

impl fmt::Display for SecretError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "credential store: {}", self.0)
    }
}

impl std::error::Error for SecretError {}

/// A credential store.
pub trait SecretStore: Send + Sync {
    /// `Ok(None)` when no secret is stored under `account`.
    fn get(&self, account: &str) -> Result<Option<String>, SecretError>;
    fn set(&self, account: &str, value: &str) -> Result<(), SecretError>;
    /// Deleting a secret that does not exist is not an error.
    fn delete(&self, account: &str) -> Result<(), SecretError>;
}

/// The OS credential store through `keyring`, under one service name ([`service_name`]).
#[derive(Debug, Clone)]
pub struct KeyringStore {
    service: String,
}

impl KeyringStore {
    pub fn new(service: String) -> Self {
        Self { service }
    }

    fn entry(&self, account: &str) -> Result<keyring::Entry, SecretError> {
        keyring::Entry::new(&self.service, account).map_err(|error| SecretError(error.to_string()))
    }
}

impl SecretStore for KeyringStore {
    fn get(&self, account: &str) -> Result<Option<String>, SecretError> {
        match self.entry(account)?.get_password() {
            Ok(value) => Ok(Some(value)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(error) => Err(SecretError(error.to_string())),
        }
    }

    fn set(&self, account: &str, value: &str) -> Result<(), SecretError> {
        self.entry(account)?
            .set_password(value)
            .map_err(|error| SecretError(error.to_string()))
    }

    fn delete(&self, account: &str) -> Result<(), SecretError> {
        match self.entry(account)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(error) => Err(SecretError(error.to_string())),
        }
    }
}

/// An in-memory store, for tests.
#[derive(Debug, Default)]
pub struct MemoryStore {
    values: Mutex<HashMap<String, String>>,
}

impl SecretStore for MemoryStore {
    fn get(&self, account: &str) -> Result<Option<String>, SecretError> {
        let values = self.values.lock().map_err(|_| SecretError("poisoned".to_string()))?;
        Ok(values.get(account).cloned())
    }

    fn set(&self, account: &str, value: &str) -> Result<(), SecretError> {
        let mut values = self.values.lock().map_err(|_| SecretError("poisoned".to_string()))?;
        values.insert(account.to_string(), value.to_string());
        Ok(())
    }

    fn delete(&self, account: &str) -> Result<(), SecretError> {
        let mut values = self.values.lock().map_err(|_| SecretError("poisoned".to_string()))?;
        values.remove(account);
        Ok(())
    }
}

/// The secret values of one configuration, keyed by long option name (`--http-auth`,
/// `--ai-api-key`, …). Empty values are not kept. `Debug` shows the option names only.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct SecretValues(BTreeMap<String, String>);

impl fmt::Debug for SecretValues {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_set().entries(self.0.keys()).finish()
    }
}

impl SecretValues {
    pub fn get(&self, option: &str) -> Option<&str> {
        self.0.get(option).map(String::as_str)
    }

    /// Sets a secret; an empty value removes it.
    pub fn set(&mut self, option: &str, value: &str) {
        if value.is_empty() {
            self.0.remove(option);
        } else {
            self.0.insert(option.to_string(), value.to_string());
        }
    }

    pub fn remove(&mut self, option: &str) {
        self.0.remove(option);
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn options(&self) -> impl Iterator<Item = &str> {
        self.0.keys().map(String::as_str)
    }

    /// These values in place of `current` (an import, *Run again*): every password of `current`
    /// that these lack is gone, so it never goes to another site. The API key belongs to the AI
    /// provider, not to a site: `current`'s stays when these have none.
    pub fn replacing(mut self, current: &SecretValues) -> SecretValues {
        if let Some(key) = current.get(AI_API_KEY)
            && self.get(AI_API_KEY).is_none()
        {
            self.set(AI_API_KEY, key);
        }
        self
    }

    /// The same options with every value replaced by [`MASK`].
    pub fn masked(&self) -> SecretValues {
        SecretValues(self.0.keys().map(|option| (option.clone(), MASK.to_string())).collect())
    }

    /// Loads the secrets of `scope` for `options` from `store`. A failing lookup is reported and
    /// that secret is left out.
    pub fn load<'a>(
        store: &dyn SecretStore,
        scope: &str,
        options: impl IntoIterator<Item = &'a str>,
    ) -> (SecretValues, Vec<SecretError>) {
        let mut values = SecretValues::default();
        let mut errors = Vec::new();
        for option in options {
            match store.get(&account(scope, option)) {
                Ok(Some(value)) => values.set(option, &value),
                Ok(None) => {}
                Err(error) => errors.push(error),
            }
        }
        (values, errors)
    }

    /// Stores these secrets under `scope`: sets the present ones and deletes the absent ones of
    /// `options`. Stops at the first error.
    pub fn save<'a>(
        &self,
        store: &dyn SecretStore,
        scope: &str,
        options: impl IntoIterator<Item = &'a str>,
    ) -> Result<(), SecretError> {
        for option in options {
            let account = account(scope, option);
            match self.get(option) {
                Some(value) => store.set(&account, value)?,
                None => store.delete(&account)?,
            }
        }
        Ok(())
    }

    /// Deletes every secret of `scope` for `options` (e.g. when a profile is deleted).
    pub fn delete_scope<'a>(
        store: &dyn SecretStore,
        scope: &str,
        options: impl IntoIterator<Item = &'a str>,
    ) -> Result<(), SecretError> {
        for option in options {
            store.delete(&account(scope, option))?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const OPTIONS: [&str; 2] = ["--http-auth", "--ai-api-key"];

    #[test]
    fn secrets_round_trip_through_a_store_per_scope() {
        let store = MemoryStore::default();
        let mut values = SecretValues::default();
        values.set("--http-auth", "user:pass");
        values.save(&store, "work", OPTIONS).expect("saved");
        assert_eq!(
            store.get("work/--http-auth").expect("get").as_deref(),
            Some("user:pass")
        );

        let (loaded, errors) = SecretValues::load(&store, "work", OPTIONS);
        assert!(errors.is_empty());
        assert_eq!(loaded, values);
        let (other, _) = SecretValues::load(&store, SESSION_SCOPE, OPTIONS);
        assert!(other.is_empty());

        values.remove("--http-auth");
        values.save(&store, "work", OPTIONS).expect("saved");
        assert_eq!(store.get("work/--http-auth").expect("get"), None);
    }

    #[test]
    fn secret_values_never_show_a_value() {
        let mut values = SecretValues::default();
        values.set("--ai-api-key", "sk-very-secret");
        values.set("--http-auth", "");
        assert_eq!(format!("{values:?}"), r#"{"--ai-api-key"}"#);
        assert_eq!(values.masked().get("--ai-api-key"), Some(MASK));
        assert_eq!(values.get("--http-auth"), None, "empty values are not kept");
    }

    #[test]
    fn replacing_drops_absent_passwords_and_keeps_the_api_key() {
        let mut current = SecretValues::default();
        current.set("--http-auth", "a:old");
        current.set("--upload-password", "old-upload");
        current.set(AI_API_KEY, "sk-old");
        let replaced = SecretValues::default().replacing(&current);
        assert_eq!(replaced.get("--http-auth"), None);
        assert_eq!(replaced.get("--upload-password"), None);
        assert_eq!(replaced.get(AI_API_KEY), Some("sk-old"));
        let mut new = SecretValues::default();
        new.set("--http-auth", "b:new");
        new.set(AI_API_KEY, "sk-new");
        let replaced = new.replacing(&current);
        assert_eq!(replaced.get("--http-auth"), Some("b:new"));
        assert_eq!(replaced.get(AI_API_KEY), Some("sk-new"));
    }

    #[test]
    fn a_data_folder_set_by_override_has_its_own_service() {
        assert_eq!(service_name(None), SERVICE);
        let a = service_name(Some(Path::new(r"D:\Portable\crawler")));
        let b = service_name(Some(Path::new(r"D:\Portable\other")));
        assert!(
            a.starts_with("siteone-crawler-gui ") && a.len() == SERVICE.len() + 17,
            "{a}"
        );
        assert_ne!(a, b);
        assert_ne!(a, SERVICE);
        // The same folder, whatever its case, is the same service on every run.
        assert_eq!(a, service_name(Some(Path::new(r"d:\portable\CRAWLER"))));
        assert_eq!(
            service_name(Some(Path::new("x"))),
            "siteone-crawler-gui af63f54c86021707"
        );
    }

    #[test]
    fn accounts_name_scope_and_option() {
        assert_eq!(account(SESSION_SCOPE, "--http-auth"), "session/--http-auth");
    }
}
