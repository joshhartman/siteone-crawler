//! Saved configurations (`<config>/profiles/<slug>.json`) and the autosaved working configuration
//! (`<config>/session.json`). Secrets are never written here: they live in the credential store
//! under the profile's slug (or `session`), see [`super::secrets`].

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::config::CrawlConfig;
use super::paths::AppPaths;
use super::schema::OptionSchema;
use super::secrets::SESSION_SCOPE;
use super::storage::{self, LoadError, Recovered};

pub const SCHEMA_VERSION: u32 = 1;

/// The longest slug.
const MAX_SLUG_CHARS: usize = 60;

/// File names Windows reserves.
const RESERVED: &[&str] = &[
    "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8", "com9", "lpt1", "lpt2",
    "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
];

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Profile {
    pub name: String,
    pub config: CrawlConfig,
}

/// A profile file as listed: its slug, and its name or why it cannot be read.
#[derive(Debug)]
pub struct ProfileEntry {
    pub slug: String,
    pub path: PathBuf,
    pub profile: Result<Profile, LoadError>,
}

/// A file-name and credential-scope safe form of a profile name: lowercase letters and digits
/// (any script), other characters as `-`. Never empty, never a reserved Windows name, never the
/// session scope.
pub fn slugify(name: &str) -> String {
    let mut slug = String::new();
    for c in name.trim().chars().flat_map(char::to_lowercase) {
        let c = if c.is_alphanumeric() { c } else { '-' };
        if !(c == '-' && (slug.is_empty() || slug.ends_with('-'))) {
            slug.push(c);
        }
    }
    let slug: String = slug.trim_end_matches('-').chars().take(MAX_SLUG_CHARS).collect();
    let slug = slug.trim_end_matches('-').to_string();
    if slug.is_empty() {
        "profile".to_string()
    } else if RESERVED.contains(&slug.as_str()) || slug == SESSION_SCOPE {
        format!("{slug}-profile")
    } else {
        slug
    }
}

pub fn profile_path(dir: &Path, slug: &str) -> PathBuf {
    dir.join(format!("{slug}.json"))
}

pub fn exists(dir: &Path, slug: &str) -> bool {
    profile_path(dir, slug).is_file()
}

/// All profiles, sorted by name (unreadable ones by slug, listed with their error).
pub fn list(dir: &Path) -> Vec<ProfileEntry> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut profiles: Vec<ProfileEntry> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_file() && path.extension().is_some_and(|ext| ext == "json"))
        .filter_map(|path| {
            let slug = path.file_stem()?.to_string_lossy().to_string();
            let profile = storage::read_json::<Profile>(&path, SCHEMA_VERSION)
                .and_then(|profile| profile.ok_or(LoadError::Parse("the file disappeared".to_string())));
            Some(ProfileEntry { slug, path, profile })
        })
        .collect();
    profiles.sort_by_key(|entry| match &entry.profile {
        Ok(profile) => profile.name.to_lowercase(),
        Err(_) => entry.slug.clone(),
    });
    profiles
}

/// Loads a profile; values of options that no longer exist are dropped (see
/// [`CrawlConfig::normalized`]) and returned as messages.
pub fn load(dir: &Path, slug: &str, schema: &OptionSchema) -> Result<(Profile, Vec<String>), LoadError> {
    let profile = storage::read_json::<Profile>(&profile_path(dir, slug), SCHEMA_VERSION)?
        .ok_or_else(|| LoadError::Io(io::Error::new(io::ErrorKind::NotFound, "no such profile")))?;
    let (config, dropped) = profile.config.normalized(schema);
    Ok((Profile { config, ..profile }, dropped))
}

/// Saves a profile under the slug of its name and returns the slug. Secret options are never
/// written (they are not part of a normalized configuration).
pub fn save(dir: &Path, profile: &Profile, schema: &OptionSchema) -> io::Result<String> {
    let slug = slugify(&profile.name);
    let (config, _) = profile.config.normalized(schema);
    let clean = Profile {
        name: profile.name.trim().to_string(),
        config,
    };
    storage::write_json(&profile_path(dir, &slug), SCHEMA_VERSION, &clean)?;
    Ok(slug)
}

pub fn delete(dir: &Path, slug: &str) -> io::Result<()> {
    match fs::remove_file(profile_path(dir, slug)) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        other => other,
    }
}

/// Renames a profile: saves it under the new name and removes the old file. Returns the new slug.
/// The caller moves the profile's secrets to the new scope.
pub fn rename(dir: &Path, slug: &str, new_name: &str, schema: &OptionSchema) -> Result<String, LoadError> {
    let (profile, _) = load(dir, slug, schema)?;
    let renamed = Profile {
        name: new_name.trim().to_string(),
        ..profile
    };
    let new_slug = save(dir, &renamed, schema).map_err(LoadError::Io)?;
    if new_slug != slug {
        delete(dir, slug).map_err(LoadError::Io)?;
    }
    Ok(new_slug)
}

/// The working configuration, autosaved so a restart restores the form.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Session {
    /// The slug of the profile the configuration was loaded from.
    pub profile: Option<String>,
    pub config: CrawlConfig,
}

impl Session {
    /// Loads the session; a damaged file is set aside (notice), dropped values are reported.
    pub fn load(paths: &AppPaths, schema: &OptionSchema) -> (Recovered<Session>, Vec<String>) {
        let mut loaded: Recovered<Session> = storage::load_or_recover(&paths.session_file(), SCHEMA_VERSION);
        let (config, dropped) = loaded.value.config.normalized(schema);
        loaded.value.config = config;
        (loaded, dropped)
    }

    pub fn save(&self, paths: &AppPaths, schema: &OptionSchema) -> io::Result<()> {
        let (config, _) = self.config.normalized(schema);
        let clean = Session {
            profile: self.profile.clone(),
            config,
        };
        storage::write_json(&paths.session_file(), SCHEMA_VERSION, &clean)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::config::Value;

    #[test]
    fn slugs_are_safe_names() {
        assert_eq!(slugify("  My Site: Prod / v2 "), "my-site-prod-v2");
        assert_eq!(slugify("Černý kůň"), "černý-kůň");
        assert_eq!(slugify("***"), "profile");
        assert_eq!(slugify("CON"), "con-profile");
        assert_eq!(slugify("Session"), "session-profile");
        assert_eq!(slugify(&"x".repeat(100)).chars().count(), MAX_SLUG_CHARS);
    }

    #[test]
    fn profiles_save_list_load_rename_and_delete() {
        let schema = OptionSchema::load();
        let tmp = tempfile::tempdir().expect("temp dir");
        let dir = tmp.path().join("profiles");
        let mut config = CrawlConfig::new("https://example.com/");
        config
            .set(&schema, "--workers", Value::Text("6".to_string()))
            .expect("set");
        // A secret that slipped into the values is never written.
        config
            .values
            .insert("--http-auth".to_string(), Value::Text("u:p".to_string()));
        let profile = Profile {
            name: "Prod site".to_string(),
            config,
        };
        let slug = save(&dir, &profile, &schema).expect("saved");
        assert_eq!(slug, "prod-site");
        let text = fs::read_to_string(profile_path(&dir, &slug)).expect("text");
        assert!(!text.contains("u:p"), "{text}");

        fs::write(dir.join("broken.json"), "{").expect("written");
        let listed = list(&dir);
        assert_eq!(listed.len(), 2);
        assert!(
            listed
                .iter()
                .any(|entry| entry.slug == "broken" && entry.profile.is_err())
        );

        let (loaded, dropped) = load(&dir, &slug, &schema).expect("loads");
        assert!(dropped.is_empty());
        assert_eq!(
            loaded.config.values.get("--workers"),
            Some(&Value::Text("6".to_string()))
        );
        assert!(!loaded.config.values.contains_key("--http-auth"));

        let renamed = rename(&dir, &slug, "Staging", &schema).expect("renamed");
        assert_eq!(renamed, "staging");
        assert!(!exists(&dir, &slug) && exists(&dir, "staging"));
        delete(&dir, "staging").expect("deleted");
        delete(&dir, "staging").expect("deleting twice is fine");
    }

    #[test]
    fn the_session_round_trips() {
        let schema = OptionSchema::load();
        let tmp = tempfile::tempdir().expect("temp dir");
        let paths = AppPaths::at(tmp.path());
        let mut session = Session {
            profile: Some("prod".to_string()),
            config: CrawlConfig::new("https://example.com/"),
        };
        session.config.ai_enabled = true;
        session.save(&paths, &schema).expect("saved");
        let (loaded, dropped) = Session::load(&paths, &schema);
        assert_eq!(loaded.value, session);
        assert!(loaded.notice.is_none() && dropped.is_empty());
    }
}
