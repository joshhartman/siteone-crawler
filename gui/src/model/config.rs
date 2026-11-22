//! A crawl configuration: the URL, the AI master switch and the options that differ from the CLI
//! defaults — and its translations to and from the CLI: argv, config file, shell command line.
//!
//! The argv round-trips through the crawler's own parser (`core_options::parse_argv`): every
//! configured value comes back exactly (see the tests). The rules that make this work:
//!
//! - scalar options are written as one element `--name=value`. The parser removes one pair of
//!   matching quotes (`"…"`, `'…'`, `` `…` ``) around a value, so a value that is itself enclosed
//!   in such a pair gets one more pair of `'` around it;
//! - every item of a list option is one element in the literal-array form `--name:=item`, which
//!   the parser takes verbatim (no quote removal, no comma split). An empty list that replaces a
//!   non-empty default is `--name:=`;
//! - booleans are `--name=true` / `--name=false`, the form the parser accepts for both values.
//!
//! Secrets (`--http-auth`, …) are never stored in a [`CrawlConfig`]; they come from a
//! [`SecretValues`] at argv time and are masked in previews unless the caller asks otherwise.

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;

use serde::{Deserialize, Serialize};
use siteone_crawler::error::CrawlerError;
use siteone_crawler::options::core_options;
use siteone_crawler::options::option_type::OptionType;

use super::schema::{OptionName, OptionSchema, OptionSpec, PathRole, Visibility};
use super::secrets::SecretValues;

/// The program name put in front of an argv for the CLI's parser and in command lines.
pub const PROGRAM_NAME: &str = "siteone-crawler";

/// What a secret looks like in previews and exports.
pub const MASK: &str = "••••";

/// The API key option. It has a dedicated field; a run passes the key through the environment.
pub const AI_API_KEY: &str = "--ai-api-key";

/// Options whose mere presence changes what the CLI does, even with the default value: an explicit
/// `--ai-actions` keeps the action set next to `--ai-report`/`--ai-elaborate`/…, an explicit
/// `--progress-interval` survives `--ci`. They stay overrides when set to their default.
const EXPLICIT_MATTERS: &[&str] = &["--ai-actions", "--progress-interval"];

/// Managed options without a path, added for validation of a configuration (GUI-SPEC §5.2). The
/// run plan adds the full set, paths included, and validates again before spawning.
const VALIDATION_MANAGED: &[&str] = &[
    "--control-stdin=true",
    "--output=text",
    "--force-color=true",
    "--console-width=160",
    "--hide-progress-bar=true",
];

/// The skip patterns of `--ignore-regex`.
pub const IGNORE_REGEX: &str = "--ignore-regex";

/// What a run adds to `--ignore-regex` while [`CrawlConfig::skip_action_links`] is on: links whose
/// request changes the site (a GET that adds to a cart or logs out) and adds nothing to a crawl.
/// The crawler matches each pattern against the full URL of a found link, entities decoded, and
/// neither requests nor reports a URL that matches (`Crawler::is_url_allowed_by_regexes`; the
/// `/…/` delimiters are the PCRE form of `utils::extract_pcre_regex_pattern`).
pub const ACTION_LINK_PATTERNS: &[&str] = &[
    // WooCommerce "Add to cart" on shop and product pages: `?add-to-cart=<product id>`.
    "/[?&]add-to-cart=/",
    // WooCommerce: remove an item from the cart, `?remove_item=<cart item key>&_wpnonce=…`.
    "/[?&]remove_item=/",
    // WooCommerce: put a removed item back into the cart, `?undo_item=<cart item key>&_wpnonce=…`.
    "/[?&]undo_item=/",
    // WordPress log-out link, `wp-login.php?action=logout&_wpnonce=…`: it ends the session a crawl
    // with a login cookie (`--header`) runs in.
    r"/wp-login\.php\?(.*&)?action=logout/",
];

/// A configured option value. Numbers are kept as text and validated by the CLI's parser.
/// Serialized untagged: `true`, `"text"`, `["a", "b"]`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Value {
    Bool(bool),
    Text(String),
    List(Vec<String>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigError {
    UnknownOption(String),
    /// Managed or hidden: the GUI sets it itself or has a dedicated field for it.
    NotEditable(String),
    /// Kept in the credential store ([`SecretValues`]), not in the configuration.
    Secret(String),
    TypeMismatch {
        option: String,
        expected: &'static str,
    },
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConfigError::UnknownOption(name) => write!(f, "unknown option {name}"),
            ConfigError::NotEditable(name) => write!(f, "{name} is set by the GUI"),
            ConfigError::Secret(name) => write!(f, "{name} is a secret and kept in the credential store"),
            ConfigError::TypeMismatch { option, expected } => write!(f, "{option} takes {expected}"),
        }
    }
}

impl std::error::Error for ConfigError {}

/// A crawl configuration. `values` holds only what differs from the CLI default ("overrides"),
/// keyed by long option name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct CrawlConfig {
    pub url: String,
    /// With `false`, no option of the AI group is emitted (the overrides are kept).
    pub ai_enabled: bool,
    /// The app checks the run's external links itself after the crawl (GUI-SPEC §4.7). Not a CLI
    /// option: nothing is emitted for it. Written only when on.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub check_external_links: bool,
    /// The run adds [`ACTION_LINK_PATTERNS`] to `--ignore-regex`, after the user's own values. On
    /// by default, also for a file written before it existed; written only when off.
    #[serde(skip_serializing_if = "is_true")]
    pub skip_action_links: bool,
    pub values: BTreeMap<OptionName, Value>,
}

impl Default for CrawlConfig {
    fn default() -> Self {
        Self {
            url: String::new(),
            ai_enabled: false,
            check_external_links: false,
            skip_action_links: true,
            values: BTreeMap::new(),
        }
    }
}

fn is_true(value: &bool) -> bool {
    *value
}

impl CrawlConfig {
    pub fn new(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            ..Self::default()
        }
    }

    /// The effective value of an option: its override, or the default.
    pub fn get(&self, schema: &OptionSchema, name: &str) -> Option<Value> {
        let spec = schema.get(name)?;
        Some(
            self.values
                .get(&spec.name)
                .cloned()
                .unwrap_or_else(|| spec.default_value.clone()),
        )
    }

    pub fn is_changed(&self, schema: &OptionSchema, name: &str) -> bool {
        schema
            .get(name)
            .is_some_and(|spec| self.values.contains_key(&spec.name))
    }

    /// Sets an option. A value equal to the default removes the override; blank list items are
    /// dropped (the CLI drops them too); `Int`/`Float` text is trimmed.
    pub fn set(&mut self, schema: &OptionSchema, name: &str, value: Value) -> Result<(), ConfigError> {
        let spec = schema
            .get(name)
            .ok_or_else(|| ConfigError::UnknownOption(name.to_string()))?;
        if spec.secret {
            return Err(ConfigError::Secret(spec.name.clone()));
        }
        if spec.visibility != Visibility::Editable {
            return Err(ConfigError::NotEditable(spec.name.clone()));
        }
        let value = normalize(spec, value)?;
        if value == spec.default_value && !EXPLICIT_MATTERS.contains(&spec.name.as_str()) {
            self.values.remove(&spec.name);
        } else {
            self.values.insert(spec.name.clone(), value);
        }
        Ok(())
    }

    /// Removes the override of an option.
    pub fn reset(&mut self, schema: &OptionSchema, name: &str) {
        if let Some(spec) = schema.get(name) {
            self.values.remove(&spec.name);
        }
    }

    /// Removes all overrides; the URL and the AI switch stay.
    pub fn reset_all(&mut self) {
        self.values.clear();
    }

    /// Number of changed options, secrets included, counting the AI group only with AI on.
    pub fn changed_count(&self, schema: &OptionSchema, secrets: &SecretValues) -> usize {
        schema
            .options()
            .iter()
            .filter(|spec| spec.is_editable() && (self.ai_enabled || !spec.is_ai()))
            .filter(|spec| {
                if spec.secret {
                    secrets.get(&spec.name).is_some()
                } else {
                    self.values.contains_key(&spec.name)
                }
            })
            .count()
    }

    /// Re-applies every stored value through [`Self::set`], e.g. after loading a file written by
    /// another version. Returns the configuration and a message per value that was dropped.
    pub fn normalized(&self, schema: &OptionSchema) -> (CrawlConfig, Vec<String>) {
        let mut clean = CrawlConfig {
            url: self.url.clone(),
            ai_enabled: self.ai_enabled,
            check_external_links: self.check_external_links,
            skip_action_links: self.skip_action_links,
            values: BTreeMap::new(),
        };
        let mut dropped = Vec::new();
        for (name, value) in &self.values {
            if let Err(error) = clean.set(schema, name, value.clone()) {
                dropped.push(error.to_string());
            }
        }
        (clean, dropped)
    }

    /// The child's argv (without the program name): `--url`, then the overrides and secrets in
    /// CLI order. The API key is not included — a run passes it through the environment.
    pub fn to_argv(&self, schema: &OptionSchema, secrets: &SecretValues) -> Vec<String> {
        self.build_argv(schema, secrets, false)
    }

    /// The argv for the command preview and exports: like [`Self::to_argv`], plus
    /// `--ai-api-key` where a run would pass a stored key through the environment, so the command
    /// does what the run does. Secrets are masked unless `include_secrets`.
    pub fn to_preview_argv(&self, schema: &OptionSchema, secrets: &SecretValues, include_secrets: bool) -> Vec<String> {
        if include_secrets {
            self.build_argv(schema, secrets, true)
        } else {
            self.build_argv(schema, &secrets.masked(), true)
        }
    }

    fn build_argv(&self, schema: &OptionSchema, secrets: &SecretValues, inline_api_key: bool) -> Vec<String> {
        let mut argv = Vec::new();
        let url = self.url.trim();
        if !url.is_empty() {
            argv.push(format!("--url={}", protect_scalar(url)));
        }
        for spec in schema.options() {
            if spec.is_ai() && !self.ai_enabled {
                continue;
            }
            if spec.name == AI_API_KEY {
                if inline_api_key
                    && self.passes_api_key_by_env(secrets)
                    && let Some(key) = secrets.get(AI_API_KEY)
                {
                    push_option(&mut argv, spec, &Value::Text(key.to_string()));
                }
                continue;
            }
            if spec.visibility != Visibility::Editable {
                continue;
            }
            let value = if spec.secret {
                secrets.get(&spec.name).map(|value| Value::Text(value.to_string()))
            } else {
                self.values.get(&spec.name).cloned()
            };
            if spec.name == IGNORE_REGEX && self.skip_action_links {
                // The user's patterns first; a pattern the user has already is not repeated.
                let mut items = match value {
                    Some(Value::List(items)) => items,
                    _ => Vec::new(),
                };
                for pattern in ACTION_LINK_PATTERNS {
                    if !items.iter().any(|item| item == pattern) {
                        items.push(pattern.to_string());
                    }
                }
                push_option(&mut argv, spec, &Value::List(items));
                continue;
            }
            // The CLI turns AI on only for an explicitly given AI option.
            let value = match value {
                Some(value) => value,
                None if self.ai_enabled && spec.name == AI_PROVIDER => spec.default_value.clone(),
                // The app's default is not the CLI's: it has to be said.
                None if spec.app_default => spec.default_value.clone(),
                None => continue,
            };
            push_option(&mut argv, spec, &value);
        }
        argv
    }

    /// This configuration with every relative input path — the value of an option with the
    /// [`PathRole::InputFile`] role (`--url-list`, `--ai-prompt-file`, …) — made absolute against
    /// `base` (GUI-SPEC §5.2). A crawler runs in its run folder: a relative path it reads would name
    /// another file than the one the app checked. A program name without a directory part in
    /// [`PROGRAM_PATHS`] is looked up on `PATH` and stays as it is; output paths stay relative.
    pub fn with_absolute_inputs(&self, schema: &OptionSchema, base: &Path) -> CrawlConfig {
        let mut config = self.clone();
        for (name, value) in &mut config.values {
            let is_input = schema
                .get(name)
                .is_some_and(|spec| spec.path_role == Some(PathRole::InputFile));
            if let (true, Value::Text(text)) = (is_input, value)
                && let Some(absolute) = absolute_input(name, text, base)
            {
                *text = absolute;
            }
        }
        config
    }

    /// Whether a run passes the stored API key through the environment: AI is on, a key is
    /// stored, and neither `--ai-api-key-env` nor `--ai-api-key-file` is set (GUI-SPEC §4.2).
    pub fn passes_api_key_by_env(&self, secrets: &SecretValues) -> bool {
        self.ai_enabled
            && secrets.get(AI_API_KEY).is_some()
            && !self.values.contains_key(AI_API_KEY_ENV)
            && !self.values.contains_key(AI_API_KEY_FILE)
    }

    /// Validates the configuration (GUI-SPEC §5.2): the URL's shape, then the CLI's parser on the
    /// argv including the managed options and `--config-file=<config_file>` (the app's empty config
    /// file, [`super::paths::AppPaths::empty_config_file`]). Returns the CLI's message on failure.
    pub fn validate(&self, schema: &OptionSchema, secrets: &SecretValues, config_file: &Path) -> Result<(), String> {
        let url = self.url.trim();
        if url.is_empty() {
            if !self.values.contains_key(URL_LIST) {
                return Err("Enter the URL of the website to crawl.".to_string());
            }
        } else {
            check_url(url)?;
        }
        let mut argv = self.to_argv(schema, secrets);
        argv.extend(VALIDATION_MANAGED.iter().map(|arg| arg.to_string()));
        argv.push(config_file_arg(config_file));
        check_argv(&argv)
    }
}

/// Input-path options whose value may be a program name that the crawler looks up on `PATH`.
pub const PROGRAM_PATHS: &[&str] = &["--ffmpeg-path"];

/// `text` made absolute against `base`, when it is a relative path (see
/// [`CrawlConfig::with_absolute_inputs`]).
fn absolute_input(option: &str, text: &str, base: &Path) -> Option<String> {
    let path = Path::new(text);
    if text.trim().is_empty() || path.is_absolute() {
        return None;
    }
    if PROGRAM_PATHS.contains(&option) && !text.contains(['/', '\\']) {
        return None;
    }
    Some(base.join(path).display().to_string())
}

const AI_PROVIDER: &str = "--ai-provider";
const AI_API_KEY_ENV: &str = "--ai-api-key-env";
const AI_API_KEY_FILE: &str = "--ai-api-key-file";
const URL_LIST: &str = "--url-list";

fn normalize(spec: &OptionSpec, value: Value) -> Result<Value, ConfigError> {
    let mismatch = |expected| ConfigError::TypeMismatch {
        option: spec.name.clone(),
        expected,
    };
    match value {
        Value::Bool(_) if spec.kind == OptionType::Bool && !spec.is_list => Ok(value),
        Value::List(items) if spec.is_list => Ok(Value::List(
            items.into_iter().filter(|item| !item.trim().is_empty()).collect(),
        )),
        Value::Text(text) if !spec.is_list && spec.kind != OptionType::Bool => Ok(Value::Text(match spec.kind {
            OptionType::Int | OptionType::Float => text.trim().to_string(),
            _ => text,
        })),
        _ if spec.is_list => Err(mismatch("a list")),
        _ if spec.kind == OptionType::Bool => Err(mismatch("true or false")),
        _ => Err(mismatch("text")),
    }
}

fn push_option(argv: &mut Vec<String>, spec: &OptionSpec, value: &Value) {
    match value {
        Value::Bool(value) => argv.push(format!("{}={}", spec.name, value)),
        Value::Text(text) => argv.push(format!("{}={}", spec.name, protect_scalar(text))),
        Value::List(items) if items.is_empty() => argv.push(format!("{}:=", spec.name)),
        Value::List(items) => argv.extend(items.iter().map(|item| format!("{}:={}", spec.name, item))),
    }
}

/// The parser removes one pair of matching quotes around a scalar value: a value that is itself
/// enclosed in such a pair gets one more.
fn protect_scalar(value: &str) -> Cow<'_, str> {
    if is_enclosed_in_quotes(value) {
        Cow::Owned(format!("'{value}'"))
    } else {
        Cow::Borrowed(value)
    }
}

fn is_enclosed_in_quotes(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() >= 2 && bytes[0] == bytes[bytes.len() - 1] && matches!(bytes[0], b'"' | b'\'' | b'`')
}

/// Removes one pair of matching quotes, exactly as the CLI's parser does.
fn unquote(value: &str) -> &str {
    if is_enclosed_in_quotes(value) {
        &value[1..value.len() - 1]
    } else {
        value
    }
}

/// `--config-file=<file>`. The CLI reads the first one and then no `~/.siteone-crawler.conf` and no
/// system config file: with the app's empty file, a crawler the app starts takes only its argv.
pub fn config_file_arg(file: &Path) -> String {
    format!("{CONFIG_FILE}={}", file.display())
}

/// Runs the CLI's parser on `argv` (without the program name). Returns the CLI's message.
///
/// Like the crawler itself, the parser also merges `$HOME/.siteone-crawler.conf` when the argv
/// names no `--config-file` (pass [`config_file_arg`]), and it creates the directories of `Dir`
/// options that do not exist.
pub fn check_argv(argv: &[String]) -> Result<(), String> {
    let mut full = Vec::with_capacity(argv.len() + 1);
    full.push(PROGRAM_NAME.to_string());
    full.extend_from_slice(argv);
    core_options::parse_argv(&full)
        .map(|_| ())
        .map_err(|error| match error {
            CrawlerError::Config(message) => message,
            other => other.to_string(),
        })
}

/// Completes a URL typed without a scheme with `https://` (on blur). Other input is returned
/// trimmed and otherwise unchanged.
pub fn complete_url(input: &str) -> String {
    let url = input.trim();
    if url.is_empty() || url.contains("://") {
        url.to_string()
    } else {
        format!("https://{url}")
    }
}

/// The URL shape check: `http://` or `https://` with a host.
pub fn check_url(url: &str) -> Result<(), String> {
    let lower = url.trim().to_ascii_lowercase();
    if !lower.starts_with("http://") && !lower.starts_with("https://") {
        return Err("The URL must start with http:// or https://.".to_string());
    }
    match url::Url::parse(url.trim()) {
        Ok(parsed) if parsed.host_str().is_some_and(|host| !host.is_empty()) => Ok(()),
        _ => Err("This is not a valid URL.".to_string()),
    }
}

// ---------------------------------------------------------------------------------------------
// Import
// ---------------------------------------------------------------------------------------------

/// The result of an import. Nothing is dropped silently: what could not become part of the
/// configuration is listed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Imported {
    pub config: CrawlConfig,
    /// Secret options found in the input, for the credential store.
    pub secrets: SecretValues,
    /// Unknown options (only the part before `=`) and stray values.
    pub unknown: Vec<String>,
    /// Known options the GUI does not take from an import: managed, hidden, `--config-file`.
    pub ignored: Vec<String>,
    /// Values the CLI would reject in a way that prevents storing them (e.g. a non-boolean).
    pub invalid: Vec<String>,
}

/// Lines of a CLI config file as arguments: one argument per line, blank lines and `#` comments
/// ignored, a leading BOM removed — the CLI's own rules.
pub fn parse_config_file(text: &str) -> Vec<String> {
    text.strip_prefix('\u{feff}')
        .unwrap_or(text)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(str::to_string)
        .collect()
}

/// Splits a pasted command line into arguments with the quoting rules of `shell` — the shells the
/// app exports for ([`command_line`]). Nothing is evaluated.
///
/// - [`Shell::Posix`]: Bash's rules for the arguments of a command ([`split_bash`]).
/// - [`Shell::PowerShell`]: PowerShell's rules for the arguments of a program ([`split_powershell`]).
pub fn split_command_line(text: &str, shell: Shell) -> Result<Vec<String>, String> {
    let text = text.replace("\r\n", "\n");
    match shell {
        Shell::Posix => split_bash(&text),
        Shell::PowerShell => split_powershell(&text),
    }
}

/// How a pasted command line reads ([`read_command_line`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandLineReading {
    /// PowerShell and Bash read the same arguments.
    Same(Vec<String>),
    /// Only `shell` can read it; the other shell's reason is `other_error`.
    Only {
        shell: Shell,
        argv: Vec<String>,
        other_error: String,
    },
    /// Both read it, differently, and the text shows it is written for `shell` (`hint`); the other
    /// shell would read `other`.
    Detected {
        shell: Shell,
        argv: Vec<String>,
        hint: &'static str,
        other: Vec<String>,
    },
    /// Both read it, differently, and the text does not show which shell it is for: no hint, or
    /// hints of both (`hints`).
    Ambiguous {
        powershell: Vec<String>,
        bash: Vec<String>,
        hints: Vec<ShellHint>,
    },
    /// Neither reads it: the error of the more plausible shell (by the hints; PowerShell without
    /// one), then the other's.
    Invalid {
        shell: Shell,
        error: String,
        other_error: String,
    },
}

/// A sign in a command line of the shell it is written for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShellHint {
    pub shell: Shell,
    pub what: &'static str,
}

/// Reads a pasted command line with both shells' rules ([`split_command_line`]) and tells which
/// reading applies. It never picks one of two different readings without a hint in the text, and
/// a hint never outweighs a hint of the other shell.
pub fn read_command_line(text: &str) -> CommandLineReading {
    let powershell = split_command_line(text, Shell::PowerShell);
    let bash = split_command_line(text, Shell::Posix);
    let hints = shell_hints(&text.replace("\r\n", "\n"));
    let hint_of = |shell: Shell| hints.iter().find(|hint| hint.shell == shell).map(|hint| hint.what);
    let pointed = match (hint_of(Shell::PowerShell), hint_of(Shell::Posix)) {
        (Some(hint), None) => Some((Shell::PowerShell, hint)),
        (None, Some(hint)) => Some((Shell::Posix, hint)),
        _ => None,
    };
    match (powershell, bash) {
        (Ok(powershell), Ok(bash)) if powershell == bash => CommandLineReading::Same(powershell),
        (Ok(powershell), Ok(bash)) => match pointed {
            Some((Shell::PowerShell, hint)) => CommandLineReading::Detected {
                shell: Shell::PowerShell,
                argv: powershell,
                hint,
                other: bash,
            },
            Some((Shell::Posix, hint)) => CommandLineReading::Detected {
                shell: Shell::Posix,
                argv: bash,
                hint,
                other: powershell,
            },
            None => CommandLineReading::Ambiguous {
                powershell,
                bash,
                hints,
            },
        },
        (Ok(argv), Err(other_error)) => CommandLineReading::Only {
            shell: Shell::PowerShell,
            argv,
            other_error,
        },
        (Err(other_error), Ok(argv)) => CommandLineReading::Only {
            shell: Shell::Posix,
            argv,
            other_error,
        },
        (Err(powershell), Err(bash)) => match pointed {
            Some((Shell::Posix, _)) => CommandLineReading::Invalid {
                shell: Shell::Posix,
                error: bash,
                other_error: powershell,
            },
            _ => CommandLineReading::Invalid {
                shell: Shell::PowerShell,
                error: powershell,
                other_error: bash,
            },
        },
    }
}

/// The signs in `text` (line breaks as `\n`) of the shell it is written for, each once: constructs
/// one shell writes and the other reads differently.
///
/// PowerShell: a backslash before a letter, digit, `.`, `_` or `-` outside quotes (a path, which
/// Bash would take as an escape), a doubled single quote inside single quotes, a backtick before a
/// line break or inside double quotes, the call operator `&` before the program. Bash: a backslash
/// before a line break, `'\''`, `\"` `\$` `\\` inside double quotes, `$'…'`, an argument that goes
/// on after the quoted string it starts with (PowerShell would make two of it).
fn shell_hints(text: &str) -> Vec<ShellHint> {
    const PS_PATH: &str = "a backslash in a path, as in C:\\…";
    const PS_DOUBLED: &str = "a doubled apostrophe in single quotes, as in 'it''s'";
    const PS_CONTINUATION: &str = "a line ending in a backtick";
    const PS_ESCAPE: &str = "a backtick escape in double quotes";
    const PS_CALL: &str = "the call operator & before the program";
    const BASH_CONTINUATION: &str = "a line ending in a backslash";
    const BASH_APOSTROPHE: &str = "an apostrophe written as '\\''";
    const BASH_ESCAPE: &str = "a backslash escape in double quotes, as in \\\"";
    const BASH_ANSI_C: &str = "a $'…' string";
    const BASH_ADJACENT: &str = "an argument that goes on after its closing quote";
    let mut hints: Vec<ShellHint> = Vec::new();
    let mut hint = |shell: Shell, what: &'static str| {
        if !hints.iter().any(|hint| hint.what == what) {
            hints.push(ShellHint { shell, what });
        }
    };
    let chars: Vec<char> = text.chars().collect();
    let first = text.trim_start();
    if first.starts_with('&') && first[1..].starts_with(char::is_whitespace) {
        hint(Shell::PowerShell, PS_CALL);
    }
    if text.contains(r"'\''") {
        hint(Shell::Posix, BASH_APOSTROPHE);
    }

    // As Bash reads the quotes.
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        i += 1;
        match c {
            '\\' => {
                match chars.get(i) {
                    Some('\n') => hint(Shell::Posix, BASH_CONTINUATION),
                    Some(&next) if next.is_alphanumeric() || matches!(next, '.' | '_' | '-') => {
                        hint(Shell::PowerShell, PS_PATH)
                    }
                    _ => {}
                }
                i += 1;
            }
            '$' if chars.get(i) == Some(&'\'') => {
                hint(Shell::Posix, BASH_ANSI_C);
                i += 1;
                while i < chars.len() {
                    i += 1;
                    match chars[i - 1] {
                        '\\' => i += 1,
                        '\'' => break,
                        _ => {}
                    }
                }
            }
            '\'' => {
                while i < chars.len() && chars[i] != '\'' {
                    i += 1;
                }
                i += 1;
            }
            '"' => {
                while i < chars.len() {
                    i += 1;
                    match chars[i - 1] {
                        '\\' => {
                            if chars.get(i).is_some_and(|next| matches!(next, '"' | '$' | '\\')) {
                                hint(Shell::Posix, BASH_ESCAPE);
                            }
                            i += 1;
                        }
                        '"' => break,
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }

    // As PowerShell reads the quotes.
    let mut i = 0;
    let mut at_start = true;
    while i < chars.len() {
        let c = chars[i];
        i += 1;
        let starts_arg = at_start;
        at_start = c.is_whitespace();
        if c == '`' {
            if chars.get(i) == Some(&'\n') {
                hint(Shell::PowerShell, PS_CONTINUATION);
            }
            i += 1;
            continue;
        }
        // `\'` is Bash's escaped apostrophe (in `'\''`), not the start of a string: no PowerShell
        // command line has it, and the `''` after it in `'\'''` is no doubled quote.
        if c == '\'' && i >= 2 && chars[i - 2] == '\\' {
            continue;
        }
        let single = is_ps_single_quote(c);
        if !single && !is_ps_double_quote(c) {
            continue;
        }
        let closes = |q: char| {
            if single {
                is_ps_single_quote(q)
            } else {
                is_ps_double_quote(q)
            }
        };
        while i < chars.len() {
            let q = chars[i];
            i += 1;
            if !single && q == '`' {
                if i < chars.len() {
                    hint(Shell::PowerShell, PS_ESCAPE);
                }
                i += 1;
            } else if closes(q) {
                if chars.get(i).copied().is_some_and(closes) {
                    if single {
                        hint(Shell::PowerShell, PS_DOUBLED);
                    }
                    i += 1;
                } else {
                    break;
                }
            }
        }
        if starts_arg && chars.get(i).is_some_and(|next| !next.is_whitespace()) {
            hint(Shell::Posix, BASH_ADJACENT);
        }
    }
    hints
}

/// What follows a `$` that Bash would expand: a variable or special parameter, `${…}`, `$(…)`,
/// `$[…]`, and outside double quotes `$"…"` (a translated string).
fn starts_bash_expansion(next: Option<&char>, in_double_quotes: bool) -> bool {
    next.is_some_and(|&c| c.is_ascii_alphanumeric() || "_{([@*#?-$!".contains(c) || (c == '"' && !in_double_quotes))
}

/// Whether a `!` before `next` starts a history reference in an interactive Bash.
fn starts_bash_history(next: Option<&char>, in_double_quotes: bool) -> bool {
    next.is_some_and(|&c| !(c.is_whitespace() || c == '=' || c == '(' || (c == '"' && in_double_quotes)))
}

/// Splits a command line the way Bash reads the arguments of a command: blanks (space, tab)
/// separate arguments and adjacent parts form one; `'…'` is literal; `"…"` takes `\` only before
/// `$`, `` ` ``, `"`, `\` and a line break; `$'…'` takes the C escapes (`\n`, `\t`, `\'`, `\xHH`,
/// `\u…`, …); outside quotes `\` escapes the next character, and before a line break it continues
/// the line; a line break without one ends the command; `#` at the start of an argument begins a
/// comment.
///
/// Whatever an import cannot read as Bash would pass it is refused with the reason, never guessed:
/// a variable, expansion or command substitution (`$name`, `${…}`, `$(…)`, `` `…` ``), an unquoted
/// `;`, `|`, `&`, `<`, `>`, `(`, `)`, `{`, `}`, a `~` at the start of an argument, a `!` that a
/// terminal's Bash reads as a history reference, a second command, and a `$'…'` that decodes to a
/// NUL or to text that is not UTF-8. Glob characters (`*`, `?`, `[`) are taken as written: a pattern
/// that matches no file is passed so, and an option argument (`--name=…`) names none.
fn split_bash(text: &str) -> Result<Vec<String>, String> {
    const UNCLOSED: &str = "The command line has an unclosed quote.";
    const EXPANSION: &str = "The command line uses a Bash variable, expansion or command substitution ($…, `…`), which an import cannot evaluate. Replace it with its value.";
    const HISTORY: &str = "The command line has a ! that Bash in a terminal reads as a history reference. Put the argument in single quotes.";
    const TILDE: &str = "The command line has an argument that starts with ~, which Bash replaces with a home folder. Write the full path.";
    const NEW_COMMAND: &str = "The command line goes on after a line break without a backslash (\\) at the end of the line. Bash would run that line as a separate command: end the line with a backslash or join the lines.";
    let special = |c: char| {
        format!(
            "The command line has an unquoted {c}, which Bash reads as part of the command, not of an argument. Put the argument in single quotes."
        )
    };
    let blank = |c: char| matches!(c, ' ' | '\t' | '\n');
    let chars: Vec<char> = text.chars().collect();
    let mut args = Vec::new();
    let mut current = String::new();
    let mut in_arg = false;
    // A line break ended the command: only blank lines and comments may follow.
    let mut ended = false;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        i += 1;
        let at_start = !in_arg;
        if ended && !blank(c) && c != '#' {
            return Err(NEW_COMMAND.to_string());
        }
        match c {
            c if blank(c) => {
                if in_arg {
                    args.push(std::mem::take(&mut current));
                    in_arg = false;
                }
                if c == '\n' && !args.is_empty() {
                    ended = true;
                }
            }
            '#' if at_start => {
                while i < chars.len() && chars[i] != '\n' {
                    i += 1;
                }
            }
            '\\' => match chars.get(i) {
                Some('\n') => {
                    i += 1;
                    if chars[i..].iter().all(|&c| blank(c)) {
                        return Err("The command line ends with a line continuation (\\).".to_string());
                    }
                }
                Some(&next) => {
                    current.push(next);
                    in_arg = true;
                    i += 1;
                }
                None => return Err("The command line ends with a backslash.".to_string()),
            },
            '\'' => {
                in_arg = true;
                loop {
                    match chars.get(i) {
                        None => return Err(UNCLOSED.to_string()),
                        Some('\'') => {
                            i += 1;
                            break;
                        }
                        Some(&other) => {
                            current.push(other);
                            i += 1;
                        }
                    }
                }
            }
            '"' => {
                in_arg = true;
                loop {
                    match chars.get(i) {
                        None => return Err(UNCLOSED.to_string()),
                        Some('\\') => match chars.get(i + 1) {
                            Some(&next) if matches!(next, '$' | '`' | '"' | '\\') => {
                                current.push(next);
                                i += 2;
                            }
                            Some('\n') => i += 2,
                            // Kept with its backslash; `\!` is no history reference either.
                            Some(&next) => {
                                current.push('\\');
                                current.push(next);
                                i += 2;
                            }
                            None => return Err(UNCLOSED.to_string()),
                        },
                        Some('"') => {
                            i += 1;
                            break;
                        }
                        Some('$') if starts_bash_expansion(chars.get(i + 1), true) => {
                            return Err(EXPANSION.to_string());
                        }
                        Some('`') => return Err(EXPANSION.to_string()),
                        Some('!') if starts_bash_history(chars.get(i + 1), true) => return Err(HISTORY.to_string()),
                        Some(&other) => {
                            current.push(other);
                            i += 1;
                        }
                    }
                }
            }
            '$' if chars.get(i) == Some(&'\'') => {
                i += 1;
                current.push_str(&bash_ansi_c(&chars, &mut i)?);
                in_arg = true;
            }
            '$' if starts_bash_expansion(chars.get(i), false) => return Err(EXPANSION.to_string()),
            '`' => return Err(EXPANSION.to_string()),
            ';' | '|' | '&' | '<' | '>' | '(' | ')' | '{' | '}' => return Err(special(c)),
            '~' if at_start => return Err(TILDE.to_string()),
            '!' if starts_bash_history(chars.get(i), false) => return Err(HISTORY.to_string()),
            other => {
                current.push(other);
                in_arg = true;
            }
        }
    }
    if in_arg {
        args.push(current);
    }
    Ok(args)
}

/// Decodes the body of a Bash `$'…'` string from `chars[*i..]` (after the opening quote) up to and
/// including its closing quote, as Bash does in a UTF-8 locale.
fn bash_ansi_c(chars: &[char], i: &mut usize) -> Result<String, String> {
    const NUL: &str = "The command line has a $'…' string with a NUL character, which cannot be passed to a program.";
    let digits = |i: &mut usize, radix: u32, max: usize| {
        let mut value = 0u32;
        let mut count = 0;
        while count < max
            && let Some(digit) = chars.get(*i).and_then(|c| c.to_digit(radix))
        {
            value = value * radix + digit;
            *i += 1;
            count += 1;
        }
        (count > 0).then_some(value)
    };
    let mut bytes: Vec<u8> = Vec::new();
    let push_char = |bytes: &mut Vec<u8>, c: char| bytes.extend_from_slice(c.encode_utf8(&mut [0; 4]).as_bytes());
    loop {
        let Some(&c) = chars.get(*i) else {
            return Err("The command line has an unclosed quote.".to_string());
        };
        *i += 1;
        match c {
            '\'' => break,
            '\\' => {
                let Some(&next) = chars.get(*i) else {
                    return Err("The command line has an unclosed quote.".to_string());
                };
                *i += 1;
                let simple = match next {
                    'a' => Some('\u{7}'),
                    'b' => Some('\u{8}'),
                    'e' | 'E' => Some('\u{1b}'),
                    'f' => Some('\u{c}'),
                    'n' => Some('\n'),
                    'r' => Some('\r'),
                    't' => Some('\t'),
                    'v' => Some('\u{b}'),
                    '\\' | '\'' | '"' | '?' => Some(next),
                    _ => None,
                };
                if let Some(simple) = simple {
                    push_char(&mut bytes, simple);
                    continue;
                }
                match next {
                    '0'..='7' => {
                        *i -= 1;
                        let value = digits(i, 8, 3).unwrap_or(0);
                        bytes.push((value & 0xff) as u8);
                    }
                    'x' => match digits(i, 16, 2) {
                        Some(value) => bytes.push(value as u8),
                        None => bytes.extend_from_slice(b"\\x"),
                    },
                    'u' | 'U' => match digits(i, 16, if next == 'u' { 4 } else { 8 }) {
                        Some(value) => match char::from_u32(value) {
                            Some(c) => push_char(&mut bytes, c),
                            None => {
                                return Err(
                                    "The command line has a $'…' string with an invalid \\u escape.".to_string()
                                );
                            }
                        },
                        None => {
                            bytes.push(b'\\');
                            push_char(&mut bytes, next);
                        }
                    },
                    'c' => match chars.get(*i) {
                        Some(&control) if control.is_ascii() => {
                            *i += 1;
                            bytes.push(if control == '?' { 0x7f } else { control as u8 & 0x1f });
                        }
                        _ => bytes.extend_from_slice(b"\\c"),
                    },
                    other => {
                        bytes.push(b'\\');
                        push_char(&mut bytes, other);
                    }
                }
            }
            other => push_char(&mut bytes, other),
        }
    }
    if bytes.contains(&0) {
        return Err(NUL.to_string());
    }
    String::from_utf8(bytes)
        .map_err(|_| "The command line has a $'…' string whose bytes are not UTF-8 text.".to_string())
}

/// PowerShell's single quotes: `'` and the typographic ones it takes as well.
fn is_ps_single_quote(c: char) -> bool {
    matches!(c, '\'' | '\u{2018}' | '\u{2019}' | '\u{201a}' | '\u{201b}')
}

/// PowerShell's double quotes: `"` and the typographic ones it takes as well.
fn is_ps_double_quote(c: char) -> bool {
    matches!(c, '"' | '\u{201c}' | '\u{201d}' | '\u{201e}')
}

/// What follows a `$` that PowerShell would expand: a variable, `${…}`, `$(…)`.
fn starts_ps_expansion(next: Option<&char>) -> bool {
    next.is_some_and(|c| c.is_alphanumeric() || matches!(c, '_' | '{' | '(' | '$' | '?' | '^' | ':'))
}

/// PowerShell's dashes, which begin a parameter token: `-` and the typographic ones it takes as well.
fn is_ps_dash(c: char) -> bool {
    matches!(c, '-' | '\u{2013}' | '\u{2014}' | '\u{2015}')
}

/// Splits a command line the way PowerShell (7.3 or later, which passes each argument to a program
/// as it is) reads the arguments of a program: whitespace separates arguments and adjacent parts
/// form one, but an argument that starts with a quoted string ends with it; `'…'` is literal, a
/// doubled single quote inside it is one; `"…"` takes the backtick escapes (`` `n ``, `` `t ``,
/// `` `" ``, …) and a doubled `"`; outside quotes a backtick escapes the next character the same
/// way, and before a line break it continues the line; a line break without one ends the command;
/// `#` at the start of an argument begins a comment, as does `<#…#>`.
///
/// Whatever an import cannot read as PowerShell would pass it is refused with the reason, never
/// guessed: a variable or expression (`$name`, `$(…)`, `@…`), an unquoted `;`, `|`, `&`, `(`, `)`,
/// `{`, `}`, a redirection, a comma that makes a list, a parameter-like argument PowerShell splits
/// (`-t=5.5` → `-t=5` `.5`), `--%`, a second command, and the escapes a program cannot get (`` `0 ``)
/// or that PowerShell 5.1 and 7 read differently (`` `e ``, `` `u{…} ``). Verified against the real
/// shells by `tests/core_powershell.rs`.
fn split_powershell(text: &str) -> Result<Vec<String>, String> {
    const UNCLOSED: &str = "The command line has an unclosed quote.";
    const EXPANSION: &str = "The command line uses a PowerShell variable or expression ($…), which an import cannot evaluate. Replace it with its value.";
    const NEW_COMMAND: &str = "The command line goes on after a line break without a backtick (`) at the end of the line. PowerShell would run that line as a separate command: end the line with a backtick or join the lines.";
    const COMMA: &str = "The command line has an unquoted comma next to a space, a quote or another comma: PowerShell makes it a list, which Windows PowerShell 5.1 and PowerShell 7 pass differently. Put the argument in single quotes.";
    const NUL: &str = "The command line has a `0 escape (a NUL character), which cannot be passed to a program.";
    const VERSIONED: &str = "The command line has a `e or `u{…} escape, which Windows PowerShell 5.1 and PowerShell 7 read differently. Write the character itself.";
    let special = |c: char| {
        format!(
            "The command line has an unquoted {c}, which PowerShell reads as part of the command, not of an argument. Put the argument in single quotes."
        )
    };
    let split = |start: &str, c: char| {
        format!(
            "PowerShell would split the unquoted argument at “{start}{c}”: in an argument that starts with a single dash, a dot, a bracket or a backtick starts a new argument. Put the argument in single quotes."
        )
    };
    let escape = |c: char| match c {
        '0' => Err(NUL.to_string()),
        'e' | 'u' => Err(VERSIONED.to_string()),
        'a' => Ok('\u{7}'),
        'b' => Ok('\u{8}'),
        'f' => Ok('\u{c}'),
        'n' => Ok('\n'),
        'r' => Ok('\r'),
        't' => Ok('\t'),
        'v' => Ok('\u{b}'),
        other => Ok(other),
    };
    let is_newline = |c: char| matches!(c, '\n' | '\r');
    let is_quote = |c: char| is_ps_single_quote(c) || is_ps_double_quote(c);
    let chars: Vec<char> = text.chars().collect();
    let mut args = Vec::new();
    let mut current = String::new();
    let mut in_arg = false;
    // The argument began with a quoted string: it ends with it.
    let mut quoted_start = false;
    // Inside the name of a parameter-like argument (`-name`, up to a `:`), where PowerShell splits
    // at `.` and `[`.
    let mut param_name = false;
    // The last character of the argument was an unquoted, unescaped one.
    let mut last_plain = false;
    // A line break ended the command: only blank lines and comments may follow.
    let mut ended = false;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        i += 1;
        if quoted_start && !c.is_whitespace() {
            // `'a'b` is two arguments.
            args.push(std::mem::take(&mut current));
            in_arg = false;
        }
        quoted_start = false;
        let at_start = !in_arg;
        if ended && !c.is_whitespace() && c != '#' && !(c == '<' && chars.get(i) == Some(&'#')) {
            return Err(NEW_COMMAND.to_string());
        }
        if at_start {
            param_name = is_ps_dash(c)
                && chars.get(i).is_some_and(|&next| {
                    !(is_ps_dash(next)
                        || next.is_ascii_digit()
                        || next == '.'
                        || next.is_whitespace()
                        || is_quote(next))
                });
            last_plain = false;
        }
        match c {
            c if c.is_whitespace() => {
                if in_arg {
                    args.push(std::mem::take(&mut current));
                    in_arg = false;
                }
                if is_newline(c) && !args.is_empty() {
                    ended = true;
                }
            }
            '#' if at_start => {
                while i < chars.len() && !is_newline(chars[i]) {
                    i += 1;
                }
            }
            '<' if at_start && chars.get(i) == Some(&'#') => {
                let close = (i + 1..chars.len().saturating_sub(1)).find(|&at| chars[at] == '#' && chars[at + 1] == '>');
                match close {
                    Some(at) => i = at + 2,
                    None => return Err("The command line has an unclosed <# comment.".to_string()),
                }
            }
            '`' if param_name => return Err(split(&current, c)),
            // Between arguments, a line continuation; inside one, an escaped line break is part of it.
            '`' => match chars.get(i) {
                Some(&next) if is_newline(next) && !in_arg => {
                    i += 1;
                    if chars[i..].iter().all(|c| c.is_whitespace()) {
                        return Err("The command line ends with a line continuation (`).".to_string());
                    }
                }
                Some(&next) => {
                    current.push(escape(next)?);
                    in_arg = true;
                    last_plain = false;
                    i += 1;
                }
                None => return Err("The command line ends with a backtick.".to_string()),
            },
            '$' if starts_ps_expansion(chars.get(i)) => return Err(EXPANSION.to_string()),
            '@' if at_start => return Err(special(c)),
            ';' | '|' | '(' | ')' | '{' | '}' => return Err(special(c)),
            // The call operator before the program.
            '&' if at_start && args.is_empty() && chars.get(i).is_some_and(|c| c.is_whitespace()) => {
                args.push(c.to_string());
            }
            '&' => return Err(special(c)),
            '>' | '<' if at_start => return Err(special(c)),
            c if at_start && (c.is_ascii_digit() || c == '*') && chars.get(i) == Some(&'>') => {
                return Err(special('>'));
            }
            ',' => {
                let next_plain = chars
                    .get(i)
                    .is_some_and(|&next| !(next.is_whitespace() || is_quote(next) || matches!(next, ',' | '`')));
                if param_name || !last_plain || !next_plain {
                    return Err(COMMA.to_string());
                }
                current.push(c);
            }
            '.' | '[' if param_name => return Err(split(&current, c)),
            c if is_ps_single_quote(c) => {
                quoted_start = at_start;
                param_name = false;
                last_plain = false;
                in_arg = true;
                loop {
                    match chars.get(i) {
                        None => return Err(UNCLOSED.to_string()),
                        Some(&q) if is_ps_single_quote(q) => {
                            if chars.get(i + 1).copied().is_some_and(is_ps_single_quote) {
                                current.push(q);
                                i += 2;
                            } else {
                                i += 1;
                                break;
                            }
                        }
                        Some(&other) => {
                            current.push(other);
                            i += 1;
                        }
                    }
                }
            }
            c if is_ps_double_quote(c) => {
                quoted_start = at_start;
                param_name = false;
                last_plain = false;
                in_arg = true;
                loop {
                    match chars.get(i) {
                        None => return Err(UNCLOSED.to_string()),
                        Some('`') => match chars.get(i + 1) {
                            Some(&next) => {
                                current.push(escape(next)?);
                                i += 2;
                            }
                            None => return Err(UNCLOSED.to_string()),
                        },
                        Some('$') if starts_ps_expansion(chars.get(i + 1)) => return Err(EXPANSION.to_string()),
                        Some(&q) if is_ps_double_quote(q) => {
                            if chars.get(i + 1).copied().is_some_and(is_ps_double_quote) {
                                current.push(q);
                                i += 2;
                            } else {
                                i += 1;
                                break;
                            }
                        }
                        Some(&other) => {
                            current.push(other);
                            i += 1;
                        }
                    }
                }
            }
            other => {
                if other == ':' {
                    param_name = false;
                }
                current.push(other);
                in_arg = true;
                last_plain = true;
            }
        }
    }
    if in_arg {
        args.push(current);
    }
    if args.iter().any(|arg| arg == "--%") {
        return Err("The command line uses --% (stop parsing), which an import does not read.".to_string());
    }
    Ok(args)
}

/// One occurrence of an option in the input.
struct Assignment {
    raw: String,
    literal: bool,
}

/// Imports CLI arguments (with or without a leading program name) into a configuration. The
/// arguments are read exactly as the CLI's parser reads them (`options/option.rs`): long and
/// alternative names, `--name=value`, `--name value`, bare booleans, the literal-array form
/// `--name:=value`, quote removal, comma splitting of list values, the last scalar value winning.
pub fn import_args(schema: &OptionSchema, args: &[String]) -> Imported {
    let mut imported = Imported::default();
    let mut assignments: Vec<(usize, Assignment)> = Vec::new();
    let options = schema.options();
    let index_of = |name: &str| {
        let spec = schema.get(name)?;
        options.iter().position(|candidate| candidate.name == spec.name)
    };

    let mut seen_option = false;
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        i += 1;
        if arg.trim().is_empty() || arg.starts_with('#') {
            continue;
        }
        if !arg.starts_with('-') {
            // Before the first option: the program name (or PowerShell's `&`).
            if seen_option {
                imported.unknown.push(arg.clone());
            }
            continue;
        }
        seen_option = true;
        if let Some((name_part, value)) = arg.split_once('=') {
            let literal = name_part.ends_with(':');
            let name = name_part.strip_suffix(':').unwrap_or(name_part);
            if name == CONFIG_FILE {
                imported.ignored.push(name.to_string());
                continue;
            }
            match index_of(name) {
                Some(at) if !literal || options[at].is_list => assignments.push((
                    at,
                    Assignment {
                        raw: value.to_string(),
                        literal,
                    },
                )),
                _ => imported.unknown.push(name_part.to_string()),
            }
        } else {
            let Some(at) = index_of(arg) else {
                imported.unknown.push(arg.clone());
                continue;
            };
            let raw = if options[at].kind == OptionType::Bool {
                "true".to_string()
            } else if let Some(next) = args.get(i).filter(|next| !next.starts_with('-')) {
                i += 1;
                next.clone()
            } else {
                String::new()
            };
            assignments.push((at, Assignment { raw, literal: false }));
        }
    }

    // Values per option, in CLI order.
    let mut values: BTreeMap<usize, Value> = BTreeMap::new();
    for (at, assignment) in assignments {
        let spec = &options[at];
        let text = if assignment.literal {
            assignment.raw.as_str()
        } else {
            unquote(&assignment.raw)
        };
        if spec.is_list {
            let items = match values.entry(at).or_insert_with(|| Value::List(Vec::new())) {
                Value::List(items) => items,
                _ => continue,
            };
            if assignment.literal || spec.kind == OptionType::HttpHeader || !text.contains(',') {
                items.push(text.to_string());
            } else {
                items.extend(
                    text.split(',')
                        .map(str::trim)
                        .filter(|part| !part.is_empty())
                        .map(|part| unquote(part).to_string()),
                );
            }
        } else if spec.kind == OptionType::Bool {
            match text {
                "1" | "yes" | "true" => values.insert(at, Value::Bool(true)),
                "0" | "no" | "false" => values.insert(at, Value::Bool(false)),
                _ => {
                    imported
                        .invalid
                        .push(format!("{} must be boolean (1/0, yes/no, true/false)", spec.name));
                    values.remove(&at)
                }
            };
        } else {
            values.insert(at, Value::Text(text.to_string()));
        }
    }

    for (at, value) in values {
        let spec = &options[at];
        if spec.name == URL {
            if let Value::Text(url) = value {
                imported.config.url = url;
            }
            continue;
        }
        if spec.secret {
            if spec.is_ai() {
                imported.config.ai_enabled = true;
            }
            if let Value::Text(secret) = value {
                imported.secrets.set(&spec.name, &secret);
            }
            continue;
        }
        if spec.visibility != Visibility::Editable {
            imported.ignored.push(spec.name.clone());
            continue;
        }
        if spec.is_ai() {
            imported.config.ai_enabled = true;
        }
        if let Err(error) = imported.config.set(schema, &spec.name, value) {
            imported.invalid.push(error.to_string());
        }
    }
    // A command with every action-link pattern (one the app wrote) gets the switch, which is on,
    // instead of the patterns, so that a run does not pass them twice.
    if let Some(Value::List(items)) = imported.config.values.get(IGNORE_REGEX)
        && ACTION_LINK_PATTERNS
            .iter()
            .all(|pattern| items.iter().any(|item| item == pattern))
    {
        let own: Vec<String> = items
            .iter()
            .filter(|item| !ACTION_LINK_PATTERNS.contains(&item.as_str()))
            .cloned()
            .collect();
        // An empty list is the default: the override goes.
        let _ = imported.config.set(schema, IGNORE_REGEX, Value::List(own));
    }
    imported
}

const URL: &str = "--url";
const CONFIG_FILE: &str = "--config-file";

// ---------------------------------------------------------------------------------------------
// Export
// ---------------------------------------------------------------------------------------------

/// The shell a command line is quoted for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shell {
    PowerShell,
    Posix,
}

impl Shell {
    /// The name the app shows: "PowerShell" or "Bash".
    pub fn label(self) -> &'static str {
        match self {
            Shell::PowerShell => "PowerShell",
            Shell::Posix => "Bash",
        }
    }

    /// The other shell.
    pub fn other(self) -> Shell {
        match self {
            Shell::PowerShell => Shell::Posix,
            Shell::Posix => Shell::PowerShell,
        }
    }
}

/// A command line for `shell`: `program` and `argv`, each quoted where needed. A quoted program
/// is called with `&` in PowerShell.
pub fn command_line(program: &str, argv: &[String], shell: Shell) -> String {
    let quote = |arg: &str| match shell {
        Shell::PowerShell => quote_powershell(arg).into_owned(),
        Shell::Posix => quote_posix(arg).into_owned(),
    };
    let program_quoted = quote(program);
    let mut line = if shell == Shell::PowerShell && program_quoted != program {
        format!("& {program_quoted}")
    } else {
        program_quoted
    };
    for arg in argv {
        line.push(' ');
        line.push_str(&quote(arg));
    }
    line
}

/// Quotes an argument for POSIX shells: unchanged when it holds only safe characters, else in
/// single quotes (a `'` becomes `'\''`).
pub fn quote_posix(arg: &str) -> Cow<'_, str> {
    let safe = |c: char| c.is_ascii_alphanumeric() || "-_./:=@%+,".contains(c);
    if !arg.is_empty() && arg.chars().all(safe) {
        Cow::Borrowed(arg)
    } else {
        Cow::Owned(format!("'{}'", arg.replace('\'', r"'\''")))
    }
}

/// Quotes an argument for PowerShell: unchanged when it holds only safe characters, else in single
/// quotes, where every single quote — PowerShell also takes the typographic ones — is doubled. An
/// argument that starts with a single `-` is a parameter token PowerShell would split
/// (`-t=5.5` → `-t=5` `.5`): it is quoted too.
///
/// The command is for PowerShell 7.3 or later, which passes each argument to a program as it is
/// (verified by `tests/core_powershell.rs`). Windows PowerShell 5.1 passes some values differently:
/// [`powershell_legacy_hazards`]. No quoting passes the argument `--%` (PowerShell drops it); the
/// app's argv never holds it.
pub fn quote_powershell(arg: &str) -> Cow<'_, str> {
    let safe = |c: char| c.is_ascii_alphanumeric() || "-_./:=\\".contains(c);
    let parameter = arg.starts_with('-') && !arg.starts_with("--");
    if !arg.is_empty() && !parameter && arg.chars().all(safe) {
        return Cow::Borrowed(arg);
    }
    let mut quoted = String::with_capacity(arg.len() + 2);
    quoted.push('\'');
    for c in arg.chars() {
        if matches!(c, '\'' | '\u{2018}' | '\u{2019}' | '\u{201a}' | '\u{201b}') {
            quoted.push(c);
        }
        quoted.push(c);
    }
    quoted.push('\'');
    Cow::Owned(quoted)
}

/// The arguments of `argv` (the option name, or the whole argument) that the legacy argument passing
/// of Windows PowerShell 5.1 (and of PowerShell 7 with `$PSNativeCommandArgumentPassing = 'Legacy'`)
/// would pass to a program differently from the exported command line: an empty argument (dropped),
/// one with a `"` (taken as quoting), and — 5.1 — one with whitespace that ends with `\` (which then
/// escapes the closing quote). No quoting is right for both kinds of passing; the export is for
/// PowerShell 7.3 or later, and the app says so when this is not empty. Verified against the real
/// shells by `tests/core_powershell.rs`.
pub fn powershell_legacy_hazards(argv: &[String]) -> Vec<String> {
    let mut names: Vec<String> = argv
        .iter()
        .filter(|arg| arg.is_empty() || arg.contains('"') || (arg.contains(char::is_whitespace) && arg.ends_with('\\')))
        .map(|arg| match arg.split_once('=') {
            Some((name, _)) if name.starts_with("--") => name.trim_end_matches(':').to_string(),
            _ => arg.to_string(),
        })
        .collect();
    names.dedup();
    names
}

/// A config file for the CLI's `--config-file`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigFileExport {
    pub text: String,
    /// Arguments a config file cannot hold exactly (a line break in a value, or trailing
    /// whitespace in a list item, which the CLI trims from each line). They are left out.
    pub inexact: Vec<String>,
}

/// Writes an argv (as from [`CrawlConfig::to_preview_argv`]) as config-file text.
pub fn config_file_text(argv: &[String]) -> ConfigFileExport {
    let mut text = String::from("# SiteOne Crawler configuration (one argument per line)\n");
    let mut inexact = Vec::new();
    for arg in argv {
        let name = arg.split('=').next().unwrap_or(arg);
        if arg.contains(['\n', '\r']) {
            inexact.push(name.to_string());
            continue;
        }
        if arg.ends_with(char::is_whitespace) {
            match arg.split_once('=') {
                // A scalar keeps its trailing whitespace inside one more pair of quotes.
                Some((name, value)) if !name.ends_with(':') => {
                    text.push_str(&format!("{name}='{value}'\n"));
                }
                _ => inexact.push(name.to_string()),
            }
            continue;
        }
        text.push_str(arg);
        text.push('\n');
    }
    ConfigFileExport { text, inexact }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use siteone_crawler::options::option::OptionValue;

    use super::*;
    use crate::model::test_support::{crawler_argv, empty_config_file};

    fn schema() -> OptionSchema {
        OptionSchema::load()
    }

    fn texts(items: &[&str]) -> Value {
        Value::List(items.iter().map(|item| item.to_string()).collect())
    }

    /// Every option's parsed value, the way `parse_argv` parses each option.
    fn parse_each(argv: &[String]) -> Result<BTreeMap<String, OptionValue>, String> {
        let mut full = vec![PROGRAM_NAME.to_string()];
        full.extend_from_slice(argv);
        let mut options = core_options::get_options();
        let mut parsed = BTreeMap::new();
        for group in options.get_groups_mut().values_mut() {
            for option in group.options.values_mut() {
                option
                    .set_value_from_argv(&full)
                    .map_err(|error| format!("{}: {error}", option.name))?;
                let value = option.get_value().map_err(|error| error.to_string())?.clone();
                parsed.insert(option.name.clone(), value);
            }
        }
        Ok(parsed)
    }

    fn assert_same(spec: &OptionSpec, expected: &Value, actual: &OptionValue) {
        let ok = match (expected, actual) {
            (Value::Bool(expected), OptionValue::Bool(actual)) => expected == actual,
            (Value::Text(expected), OptionValue::Int(actual)) => expected.parse::<i64>().ok() == Some(*actual),
            (Value::Text(expected), OptionValue::Float(actual)) => expected.parse::<f64>().ok() == Some(*actual),
            (Value::Text(expected), OptionValue::Str(actual)) => expected == actual,
            (Value::Text(expected), OptionValue::None) => expected.is_empty(),
            (Value::List(expected), OptionValue::Array(actual)) => expected == actual,
            _ => false,
        };
        assert!(ok, "{}: configured {expected:?}, parsed {actual:?}", spec.name);
    }

    /// Adversarial texts: spaces, commas, quotes, `=`, `:=`, non-ASCII, Windows paths, values that
    /// are themselves quoted, a leading dash, trailing whitespace.
    const NASTY: &[&str] = &[
        "a b, c",
        "say \"hi\" it's",
        "k=v",
        "x:=y",
        "--not-an-option=1",
        "-dash",
        "žluťoučký kůň 日本",
        r"C:\Program Files\Site One\x.txt",
        "'single'",
        "\"double\"",
        "`back`",
        "'",
        "trailing ",
        " leading",
        "a,b,,c",
        "",
    ];

    /// Test values per option: (value, label). Types with a format get valid values of that format.
    fn values_for(spec: &OptionSpec, dir: &Path) -> Vec<Value> {
        let file = dir.join("out dir ž").join("file name ž,1.txt");
        let file = file.to_string_lossy().to_string();
        let sub_dir = dir.join("dir with spaces ž, 'q'").to_string_lossy().to_string();
        let number = |fallback: &str| match spec.range {
            Some((min, max)) => {
                let mid = ((min + max) / 2.0).floor();
                if spec.kind == OptionType::Int {
                    format!("{}", mid as i64)
                } else {
                    format!("{mid}")
                }
            }
            None => fallback.to_string(),
        };
        let scalars: Vec<String> = match spec.kind {
            OptionType::Bool => {
                let default = spec.default_value == Value::Bool(true);
                return vec![Value::Bool(!default)];
            }
            OptionType::Int => vec![number("7")],
            OptionType::Float => vec![number("0.75")],
            OptionType::SizeMG => vec!["512M".to_string(), "1.5G".to_string()],
            OptionType::Email => vec!["a.b@example.com".to_string(), "'q@example.cz'".to_string()],
            OptionType::Url => vec!["https://example.com/a,b?c='d'&e=f".to_string()],
            OptionType::Regex => vec![
                r"^/a,b(?:x|y)=1$".to_string(),
                "'q'".to_string(),
                "x:=y".to_string(),
                "ž+".to_string(),
            ],
            OptionType::File => vec![file],
            OptionType::Dir => vec![sub_dir],
            OptionType::HostAndPort => vec!["proxy.local:8080".to_string()],
            OptionType::ReplaceContent => vec![
                "foo, bar -> baz".to_string(),
                "'x' -> 'y'".to_string(),
                "/a=b/i -> c:=d".to_string(),
            ],
            OptionType::Resolve => vec!["example.com:443:127.0.0.1".to_string(), "a.b:80:::1".to_string()],
            OptionType::HttpHeader => vec![
                "X-Test: a, b=c".to_string(),
                "Cookie: s='1'; t=\"2\"".to_string(),
                "X-Uni: žluť".to_string(),
            ],
            OptionType::String => NASTY.iter().map(|value| value.to_string()).collect(),
        };
        if spec.is_list {
            // Blank items are not values (the CLI drops them), so they are no test values.
            let items: Vec<String> = scalars.into_iter().filter(|item| !item.trim().is_empty()).collect();
            let mut lists = vec![Value::List(items.clone())];
            lists.extend(items.into_iter().map(|item| Value::List(vec![item])));
            if !matches!(&spec.default_value, Value::List(default) if default.is_empty()) {
                lists.push(Value::List(Vec::new()));
            }
            lists
        } else {
            scalars
                .into_iter()
                // An empty text equals "no value", which is only a value for nullable options.
                .filter(|value| !value.is_empty() || spec.nullable)
                .map(Value::Text)
                .collect()
        }
    }

    #[test]
    fn the_timeout_defaults_to_30_seconds_and_is_always_passed() {
        let schema = schema();
        let secrets = SecretValues::default();
        let timeout = |config: &CrawlConfig| -> Vec<String> {
            config
                .to_argv(&schema, &secrets)
                .into_iter()
                .filter(|arg| arg.starts_with("--timeout"))
                .collect()
        };
        let mut config = CrawlConfig::new("https://example.com/");
        assert_eq!(config.get(&schema, "--timeout"), Some(Value::Text("30".to_string())));
        assert_eq!(timeout(&config), ["--timeout=30"]);
        assert!(!config.is_changed(&schema, "--timeout"));
        // The CLI's default is an override like any other value.
        config
            .set(&schema, "--timeout", Value::Text("5".to_string()))
            .expect("set");
        assert_eq!(timeout(&config), ["--timeout=5"]);
        // Back to the app's default: no override, still passed.
        config
            .set(&schema, "--timeout", Value::Text("30".to_string()))
            .expect("set");
        assert!(!config.is_changed(&schema, "--timeout"));
        assert_eq!(timeout(&config), ["--timeout=30"]);
        // A command without `--timeout` gets the app's default.
        let imported = import_args(&schema, &["--url=https://example.com/".to_string()]);
        assert_eq!(timeout(&imported.config), ["--timeout=30"]);
    }

    #[test]
    fn link_details_are_on_by_default_and_always_passed() {
        let schema = schema();
        let secrets = SecretValues::default();
        let details = |config: &CrawlConfig| -> Vec<String> {
            config
                .to_argv(&schema, &secrets)
                .into_iter()
                .filter(|arg| arg.starts_with("--link-details"))
                .collect()
        };
        let mut config = CrawlConfig::new("https://example.com/");
        assert_eq!(config.get(&schema, "--link-details"), Some(Value::Bool(true)));
        assert_eq!(details(&config), ["--link-details=true"]);
        assert!(!config.is_changed(&schema, "--link-details"));
        config.set(&schema, "--link-details", Value::Bool(false)).expect("set");
        assert!(config.is_changed(&schema, "--link-details"));
        assert_eq!(details(&config), ["--link-details=false"]);
        // A command or a stored configuration without it gets the app's default; with it off, off.
        let imported = import_args(&schema, &["--url=https://example.com/".to_string()]);
        assert_eq!(details(&imported.config), ["--link-details=true"]);
        let off = import_args(
            &schema,
            &[
                "--url=https://example.com/".to_string(),
                "--link-details=false".to_string(),
            ],
        );
        assert_eq!(details(&off.config), ["--link-details=false"]);
        // The CLI's parser takes both.
        for config in [&imported.config, &off.config] {
            let mut argv = config.to_argv(&schema, &secrets);
            argv.push(format!("--config-file={}", empty_config_file().display()));
            check_argv(&argv).expect("valid argv");
        }
    }

    /// The heart of the configuration model: every editable option, with adversarial values, comes
    /// back exactly from the crawler's own option parser — and touches no other option.
    #[test]
    fn every_option_round_trips_through_the_cli_parser() {
        let schema = schema();
        let tmp = tempfile::tempdir().expect("temp dir");
        let secrets_base = SecretValues::default();
        let base_argv = crawler_argv::<&str>(&[]);
        let baseline = parse_each(&base_argv).expect("defaults parse");
        let mut cases = 0;

        for spec in schema.options() {
            if !spec.is_editable() {
                continue;
            }
            for value in values_for(spec, tmp.path()) {
                let mut config = CrawlConfig::new("https://example.com/");
                config.ai_enabled = true;
                // The option on its own (the switch adds to `--ignore-regex`; its own test).
                config.skip_action_links = false;
                let mut secrets = secrets_base.clone();
                if spec.secret {
                    let Value::Text(text) = &value else { unreachable!() };
                    if text.is_empty() {
                        continue;
                    }
                    secrets.set(&spec.name, text);
                } else {
                    config.set(&schema, &spec.name, value.clone()).expect("settable");
                    assert!(
                        config.values.contains_key(&spec.name) || value == spec.default_value,
                        "{} {value:?} was not stored",
                        spec.name
                    );
                }
                let mut argv = config.to_argv(&schema, &secrets);
                argv.push(format!("--config-file={}", empty_config_file().display()));
                let parsed = parse_each(&argv).unwrap_or_else(|error| panic!("{argv:?}: {error}"));
                // The full parser also accepts the syntax (its cross-option rules may still refuse
                // a single option on its own, e.g. `--screenshots` without `--browser`).
                if let Err(error) = check_argv(&argv) {
                    assert!(!error.contains("Unknown options"), "{argv:?}: {error}");
                }
                for (name, parsed_value) in &parsed {
                    if *name == spec.name {
                        assert_same(spec, &value, parsed_value);
                    } else if *name == AI_PROVIDER {
                        // Always explicit with AI on; its value is the default.
                        assert_eq!(parsed_value.as_str(), Some("openai-compatible"));
                    } else if let Some(other) = schema.get(name).filter(|other| other.app_default) {
                        // Passed with the app's default, which is not the CLI's.
                        assert_same(other, &other.default_value, parsed_value);
                    } else if schema
                        .get(name)
                        .and_then(|other| other.default.as_deref())
                        .is_some_and(|d| d.contains('%'))
                    {
                        // A default with `%domain%`/`%datetime%` depends on the clock and on the
                        // parser's process-global domain, which parallel tests change.
                    } else {
                        assert_eq!(
                            format!("{parsed_value:?}"),
                            format!("{:?}", baseline[name]),
                            "setting {} {value:?} changed {name}: {argv:?}",
                            spec.name
                        );
                    }
                }
                cases += 1;
            }
        }
        assert!(cases > 250, "only {cases} cases");
    }

    #[test]
    fn a_realistic_configuration_passes_the_full_cli_validation() {
        let schema = schema();
        let tmp = tempfile::tempdir().expect("temp dir");
        let mut config = CrawlConfig::new("https://example.com/shop?q=1");
        let set = |config: &mut CrawlConfig, name: &str, value: Value| {
            config.set(&schema, name, value).expect(name);
        };
        set(&mut config, "--workers", Value::Text("8".to_string()));
        set(&mut config, "--max-reqs-per-sec", Value::Text("2.5".to_string()));
        set(&mut config, "--device", Value::Text("mobile".to_string()));
        set(
            &mut config,
            "--user-agent",
            Value::Text("My \"Bot\", v1 = 'x' !".to_string()),
        );
        set(
            &mut config,
            "--header",
            texts(&["Accept-Language: cs,en;q=0.8", "X-A: 1"]),
        );
        set(&mut config, "--include-regex", texts(&["^/(a|b),c$", "/shop/"]));
        set(&mut config, "--markdown-exclude-selector", texts(&["div.a, div.b"]));
        set(&mut config, "--accept-invalid-certs", Value::Bool(true));
        set(&mut config, "--offline-export-remove-unwanted-code", Value::Bool(false));
        set(&mut config, "--browser", Value::Bool(true));
        set(&mut config, "--browser-auto-scroll", Value::Bool(false));
        set(&mut config, "--browser-wait", Value::Text("load".to_string()));
        let offline = tmp.path().join("offline ž");
        set(
            &mut config,
            "--offline-export-dir",
            Value::Text(offline.to_string_lossy().to_string()),
        );
        set(&mut config, "--output-html-report", Value::Text(String::new()));
        let mut secrets = SecretValues::default();
        secrets.set("--http-auth", "user:pa ss,wörd");
        config.validate(&schema, &secrets, empty_config_file()).expect("valid");

        let mut argv = config.to_argv(&schema, &secrets);
        argv.push(format!("--config-file={}", empty_config_file().display()));
        let mut full = vec![PROGRAM_NAME.to_string()];
        full.extend(argv);
        let core = core_options::parse_argv(&full).expect("parses");
        assert_eq!(core.url, "https://example.com/shop?q=1");
        assert_eq!(core.workers, 8);
        assert_eq!(core.max_reqs_per_sec, 2.5);
        assert_eq!(core.user_agent.as_deref(), Some("My \"Bot\", v1 = 'x' !"));
        assert_eq!(core.http_headers, vec!["Accept-Language: cs,en;q=0.8", "X-A: 1"]);
        assert_eq!(core.include_regex, vec!["^/(a|b),c$", "/shop/"]);
        assert_eq!(core.markdown_exclude_selector, vec!["div.a, div.b"]);
        assert!(core.accept_invalid_certs);
        assert!(!core.offline_export_remove_unwanted_code);
        assert!(core.browser_enabled);
        assert!(!core.browser_auto_scroll);
        assert_eq!(core.output_html_report, None);
        assert_eq!(
            core.offline_export_dir.as_deref(),
            Some(offline.to_string_lossy().as_ref())
        );
        assert!(!core.ai_enabled);
    }

    #[test]
    fn ai_options_are_emitted_only_with_ai_on() {
        let schema = schema();
        let mut config = CrawlConfig::new("https://example.com/");
        config
            .set(
                &schema,
                "--ai-endpoint",
                Value::Text("http://127.0.0.1:9/v1".to_string()),
            )
            .expect("endpoint");
        config
            .set(&schema, "--ai-model", Value::Text("m".to_string()))
            .expect("model");
        config
            .set(&schema, "--ai-actions", texts(&["seo", "summary"]))
            .expect("actions");
        let secrets = SecretValues::default();

        let off = config.to_argv(&schema, &secrets);
        assert!(off.iter().all(|arg| !arg.starts_with("--ai-")), "{off:?}");
        let full = |argv: Vec<String>| {
            let mut full = vec![PROGRAM_NAME.to_string()];
            full.extend(argv);
            full.push(format!("--config-file={}", empty_config_file().display()));
            core_options::parse_argv(&full).expect("parses")
        };
        assert!(!full(off).ai_enabled);

        config.ai_enabled = true;
        let on = full(config.to_argv(&schema, &secrets));
        assert!(on.ai_enabled);
        assert_eq!(on.ai_actions, vec!["seo", "summary"]);
        assert_eq!(on.ai_model.as_deref(), Some("m"));
    }

    #[test]
    fn ai_on_without_any_ai_override_still_turns_ai_on_in_the_cli() {
        let schema = schema();
        let config = CrawlConfig {
            url: "https://example.com/".to_string(),
            ai_enabled: true,
            values: BTreeMap::new(),
            ..CrawlConfig::default()
        };
        let argv = config.to_argv(&schema, &SecretValues::default());
        assert!(argv.contains(&"--ai-provider=openai-compatible".to_string()));
        // The CLI now treats AI as on and asks for its endpoint.
        let error = check_argv(&crawler_argv(&argv[1..])).expect_err("needs an endpoint");
        assert!(error.contains("--ai-endpoint"), "{error}");
    }

    #[test]
    fn an_explicit_default_of_ai_actions_is_kept() {
        let schema = schema();
        let mut config = CrawlConfig::new("https://example.com/");
        config
            .set(&schema, "--ai-actions", texts(&["seo", "typos", "summary"]))
            .expect("set");
        assert!(config.values.contains_key("--ai-actions"));
        config
            .set(&schema, "--workers", Value::Text(" 3 ".to_string()))
            .expect("set");
        assert!(!config.values.contains_key("--workers"), "default removes the override");
    }

    #[test]
    fn set_rejects_secret_managed_hidden_and_mistyped_values() {
        let schema = schema();
        let mut config = CrawlConfig::default();
        let set = |config: &mut CrawlConfig, name: &str, value: Value| config.set(&schema, name, value);
        assert!(matches!(
            set(&mut config, "--http-auth", Value::Text("a:b".to_string())),
            Err(ConfigError::Secret(_))
        ));
        assert!(matches!(
            set(&mut config, "--output", Value::Text("json".to_string())),
            Err(ConfigError::NotEditable(_))
        ));
        assert!(matches!(
            set(&mut config, "--url", Value::Text("x".to_string())),
            Err(ConfigError::NotEditable(_))
        ));
        assert!(matches!(
            set(&mut config, "--workers", Value::Bool(true)),
            Err(ConfigError::TypeMismatch { .. })
        ));
        assert!(matches!(
            set(&mut config, "--header", Value::Text("X: 1".to_string())),
            Err(ConfigError::TypeMismatch { .. })
        ));
        assert!(matches!(
            set(&mut config, "--nope", Value::Bool(true)),
            Err(ConfigError::UnknownOption(_))
        ));
        set(&mut config, "-w", Value::Text("9".to_string())).expect("alt name");
        assert_eq!(config.values.get("--workers"), Some(&Value::Text("9".to_string())));
        set(&mut config, "--header", texts(&["", "  ", "X: 1"])).expect("blank items dropped");
        assert_eq!(config.values.get("--header"), Some(&texts(&["X: 1"])));
    }

    #[test]
    fn validation_reports_the_cli_message() {
        let schema = schema();
        let secrets = SecretValues::default();
        let mut config = CrawlConfig::new("");
        assert!(config.validate(&schema, &secrets, empty_config_file()).is_err());
        config.url = "ftp://example.com".to_string();
        assert!(
            config
                .validate(&schema, &secrets, empty_config_file())
                .unwrap_err()
                .contains("http")
        );
        config.url = "https://example.com/".to_string();
        config
            .set(&schema, "--workers", Value::Text("abc".to_string()))
            .expect("set");
        let error = config
            .validate(&schema, &secrets, empty_config_file())
            .expect_err("not a number");
        assert!(error.contains("--workers"), "{error}");
        config
            .set(&schema, "--workers", Value::Text("4".to_string()))
            .expect("set");
        config.validate(&schema, &secrets, empty_config_file()).expect("valid");
    }

    #[test]
    fn urls_are_completed_and_checked() {
        assert_eq!(complete_url("  example.com/a "), "https://example.com/a");
        assert_eq!(complete_url("http://example.com"), "http://example.com");
        assert_eq!(complete_url(""), "");
        assert!(check_url("https://example.com/").is_ok());
        assert!(check_url("HTTP://EXAMPLE.COM").is_ok());
        assert!(check_url("https://").is_err());
        assert!(check_url("mailto:a@b.c").is_err());
    }

    #[test]
    fn import_reads_arguments_the_way_the_cli_does() {
        let schema = schema();
        let args: Vec<String> = [
            "siteone-crawler",
            "--url",
            "https://example.com/",
            "-w=5",
            "--timeout",
            "9",
            "--include-regex=a,'b'",
            "--include-regex:=c,d",
            "--header=Accept-Language: cs,en",
            "--single-page",
            "--disable-images=yes",
            "--offline-export-remove-unwanted-code=0",
            "--user-agent='quoted value'",
            "--http-auth=user:secret",
            "--output=json",
            "--config-file=x.conf",
            "--bogus=1",
            "--single-page:=x",
            "stray",
            "--ai-model=m",
        ]
        .iter()
        .map(|arg| arg.to_string())
        .collect();
        let imported = import_args(&schema, &args);
        let config = &imported.config;
        assert_eq!(config.url, "https://example.com/");
        assert_eq!(config.values.get("--workers"), Some(&Value::Text("5".to_string())));
        assert_eq!(config.values.get("--timeout"), Some(&Value::Text("9".to_string())));
        assert_eq!(config.values.get("--include-regex"), Some(&texts(&["a", "b", "c,d"])));
        assert_eq!(config.values.get("--header"), Some(&texts(&["Accept-Language: cs,en"])));
        assert_eq!(config.values.get("--single-page"), Some(&Value::Bool(true)));
        assert_eq!(config.values.get("--disable-images"), Some(&Value::Bool(true)));
        assert_eq!(
            config.values.get("--offline-export-remove-unwanted-code"),
            Some(&Value::Bool(false))
        );
        assert_eq!(
            config.values.get("--user-agent"),
            Some(&Value::Text("quoted value".to_string()))
        );
        assert!(config.ai_enabled);
        assert_eq!(imported.secrets.get("--http-auth"), Some("user:secret"));
        assert!(!config.values.contains_key("--http-auth"));
        assert_eq!(imported.ignored, vec!["--config-file", "--output"]);
        assert_eq!(imported.unknown, vec!["--bogus", "--single-page:", "stray"]);
        assert!(imported.invalid.is_empty(), "{:?}", imported.invalid);

        // The CLI parses the same arguments into the same values.
        let mut full = args.clone();
        full.retain(|arg| !["--bogus=1", "--single-page:=x", "stray", "--config-file=x.conf"].contains(&arg.as_str()));
        full.retain(|arg| !arg.starts_with("--ai-"));
        full.push(format!("--config-file={}", empty_config_file().display()));
        let core = core_options::parse_argv(&full).expect("parses");
        assert_eq!(core.include_regex, vec!["a", "b", "c,d"]);
        assert_eq!(core.user_agent.as_deref(), Some("quoted value"));
        assert_eq!(core.workers, 5);
        assert_eq!(core.timeout, 9);
    }

    #[test]
    fn import_of_an_exported_argv_gives_back_the_configuration() {
        let schema = schema();
        let tmp = tempfile::tempdir().expect("temp dir");
        for spec in schema
            .options()
            .iter()
            .filter(|spec| spec.is_editable() && !spec.secret)
        {
            for value in values_for(spec, tmp.path()) {
                let mut config = CrawlConfig::new("https://example.com/");
                config.ai_enabled = spec.is_ai();
                config.set(&schema, &spec.name, value.clone()).expect("settable");
                let argv = config.to_argv(&schema, &SecretValues::default());
                let imported = import_args(&schema, &argv);
                // `--ai-provider` is emitted explicitly with AI on; as the default it is no override.
                assert_eq!(imported.config, config, "{} {value:?} via {argv:?}", spec.name);
                assert!(imported.unknown.is_empty() && imported.invalid.is_empty());
            }
        }
    }

    #[test]
    fn a_config_file_round_trips() {
        let schema = schema();
        let mut config = CrawlConfig::new("https://example.com/");
        config
            .set(&schema, "--user-agent", Value::Text("trailing space ".to_string()))
            .expect("set");
        config
            .set(&schema, "--include-regex", texts(&["a,b", "# not a comment"]))
            .expect("set");
        config.set(&schema, "--header", texts(&["X-A: 1 "])).expect("set");
        let export = config_file_text(&config.to_argv(&schema, &SecretValues::default()));
        assert_eq!(export.inexact, vec!["--header:"]);
        let imported = import_args(&schema, &parse_config_file(&export.text));
        let mut expected = config.clone();
        expected.values.remove("--header");
        assert_eq!(imported.config, expected);

        let with_bom = "\u{feff}--workers=4\n\n  # comment\n  --single-page  \r\n";
        assert_eq!(parse_config_file(with_bom), vec!["--workers=4", "--single-page"]);
    }

    #[test]
    fn previews_mask_secrets_and_show_the_api_key_where_a_run_passes_it() {
        let schema = schema();
        let mut config = CrawlConfig::new("https://example.com/");
        let mut secrets = SecretValues::default();
        secrets.set("--http-auth", "user:pass");
        secrets.set(AI_API_KEY, "sk-secret");
        let preview = config.to_preview_argv(&schema, &secrets, false);
        assert!(preview.contains(&format!("--http-auth={MASK}")));
        assert!(
            !preview
                .iter()
                .any(|arg| arg.contains("sk-secret") || arg.contains("user:pass"))
        );
        assert!(!preview.iter().any(|arg| arg.starts_with(AI_API_KEY)), "AI is off");

        config.ai_enabled = true;
        let preview = config.to_preview_argv(&schema, &secrets, false);
        assert!(preview.contains(&format!("--ai-api-key={MASK}")));
        let revealed = config.to_preview_argv(&schema, &secrets, true);
        assert!(revealed.contains(&"--ai-api-key=sk-secret".to_string()));
        assert!(revealed.contains(&"--http-auth=user:pass".to_string()));
        // A run passes the key through the environment, never in the argv.
        assert!(
            !config
                .to_argv(&schema, &secrets)
                .iter()
                .any(|arg| arg.contains("sk-secret"))
        );

        config
            .set(&schema, "--ai-api-key-file", Value::Text("C:\\keys\\k.txt".to_string()))
            .expect("set");
        assert!(!config.passes_api_key_by_env(&secrets));
        let preview = config.to_preview_argv(&schema, &secrets, true);
        assert!(!preview.iter().any(|arg| arg.starts_with("--ai-api-key=")));
    }

    #[test]
    fn command_lines_are_quoted_per_shell() {
        let argv: Vec<String> = [
            "--url=https://example.com/?a=1&b=2",
            "--user-agent=it's me",
            "--workers=3",
        ]
        .iter()
        .map(|arg| arg.to_string())
        .collect();
        let posix = command_line("siteone-crawler", &argv, Shell::Posix);
        assert_eq!(
            posix,
            r"siteone-crawler '--url=https://example.com/?a=1&b=2' '--user-agent=it'\''s me' --workers=3"
        );
        assert_eq!(shlex::split(&posix).expect("splits")[1..], argv[..]);

        let ps = command_line(r"C:\Program Files\siteone-crawler.exe", &argv, Shell::PowerShell);
        assert_eq!(
            ps,
            r"& 'C:\Program Files\siteone-crawler.exe' '--url=https://example.com/?a=1&b=2' '--user-agent=it''s me' --workers=3"
        );
        assert_eq!(quote_powershell("a\u{2019}b"), "'a\u{2019}\u{2019}b'");
        assert_eq!(quote_powershell("a,b"), "'a,b'");
        assert_eq!(quote_powershell(""), "''");
        assert_eq!(quote_posix(""), "''");
        // A parameter token PowerShell would split; `--name=…` is not one.
        assert_eq!(quote_powershell("-t=5.5"), "'-t=5.5'");
        assert_eq!(
            quote_powershell("--url=https://example.com/"),
            "--url=https://example.com/"
        );
        // What Windows PowerShell 5.1 passes differently.
        let hazards = powershell_legacy_hazards(&[
            "--user-agent=say \"hi\"".to_string(),
            "--header:=X-A: \"a\"".to_string(),
            "--header:=X-B: b".to_string(),
            r"--offline-export-dir=C:\My Exports\".to_string(),
            r"--output-html-report=C:\Exports\".to_string(),
            String::new(),
        ]);
        assert_eq!(hazards, ["--user-agent", "--header", "--offline-export-dir", ""]);
    }

    #[test]
    fn pasted_command_lines_are_split_and_imported() {
        let schema = schema();
        let text = "siteone-crawler --url='https://example.com/' \\\n  --workers 4 \\\n  --header=\"X-A: a, b\"";
        let args = split_command_line(text, Shell::Posix).expect("splits");
        let imported = import_args(&schema, &args);
        assert_eq!(imported.config.url, "https://example.com/");
        assert_eq!(
            imported.config.values.get("--workers"),
            Some(&Value::Text("4".to_string()))
        );
        assert_eq!(imported.config.values.get("--header"), Some(&texts(&["X-A: a, b"])));
        let ps = "& 'C:\\x\\siteone-crawler.exe' --url=https://example.com/ `\r\n --single-page";
        let imported = import_args(&schema, &split_command_line(ps, Shell::PowerShell).expect("splits"));
        assert_eq!(imported.config.values.get("--single-page"), Some(&Value::Bool(true)));
        assert!(imported.unknown.is_empty(), "{:?}", imported.unknown);
        assert!(split_command_line("--url='open", Shell::Posix).is_err());
        assert!(split_command_line("--url='open", Shell::PowerShell).is_err());
    }

    /// Hand-written PowerShell, read as PowerShell reads it — without evaluating anything.
    #[test]
    fn powershell_command_lines_follow_powershell_quoting() {
        let split = |text: &str| split_command_line(text, Shell::PowerShell);
        assert_eq!(
            split(r#"siteone-crawler --output-text-file=C:\out\a.txt "--user-agent=say `"hi`"" '--x=it''s' --y="a""b" `#not-a-comment # a comment"#)
                .expect("splits")[1..],
            [
                r"--output-text-file=C:\out\a.txt",
                r#"--user-agent=say "hi""#,
                "--x=it's",
                "--y=a\"b",
                "#not-a-comment",
            ]
        );
        // Adjacent parts form one argument; a doubled typographic quote is one.
        assert_eq!(
            split("--header='X: a b'c --name=\u{2018}Sam\u{2019}\u{2019}s\u{2019}").expect("splits"),
            ["--header=X: a bc", "--name=Sam\u{2019}s"]
        );
        assert_eq!(
            split("\"a`nb\" '$HOME' $ cost").expect("splits"),
            ["a\nb", "$HOME", "$", "cost"]
        );
        for expanded in ["--url=$url", "\"--url=$($x)\"", "--a=${b}", "\"$env:KEY\""] {
            let error = split(expanded).expect_err(expanded);
            assert!(error.contains("variable"), "{error}");
        }
        assert!(split("--a=b`").is_err());
        assert!(split("\"open").is_err());
    }

    /// Hand-written Bash, read as Bash reads it — without evaluating anything.
    #[test]
    fn bash_command_lines_follow_bash_quoting() {
        let split = |text: &str| split_command_line(text, Shell::Posix);
        let accepted: &[(&str, &[&str])] = &[
            (
                r#"siteone-crawler --a='x y' --b="p q" --c=r\ s 'a'b"c"d"#,
                &["siteone-crawler", "--a=x y", "--b=p q", "--c=r s", "abcd"],
            ),
            (
                r#""a\zb" "a\$b" "a\\b" "a\"b" "a\`b" 'a\b' a\b"#,
                &[r"a\zb", "a$b", r"a\b", "a\"b", "a`b", r"a\b", "ab"],
            ),
            ("a \\\n  b ab\\\ncd \"x\\\ny\"", &["a", "b", "abcd", "xy"]),
            ("a 'multi\nline'", &["a", "multi\nline"]),
            ("siteone-crawler --a=1 # comment\n", &["siteone-crawler", "--a=1"]),
            (
                "\n# comment\n  siteone-crawler a#b\n\n# end\n",
                &["siteone-crawler", "a#b"],
            ),
            (r#""a$ b" --a=$ 5$ "$""#, &["a$ b", "--a=$", "5$", "$"]),
            (
                r"$'a\tb\x41\u00e9\'c\\' $'\xc3\xa9' $'\101\cA\q'",
                &["a\tbA\u{e9}'c\\", "\u{e9}", "A\u{1}\\q"],
            ),
            (r#""Hi!" --a=b! 'x!y' "!" "a!""#, &["Hi!", "--a=b!", "x!y", "!", "a!"]),
            (
                "--url=https://example.com/?a=1 --a=~/x --b=*.pdf [x]",
                &["--url=https://example.com/?a=1", "--a=~/x", "--b=*.pdf", "[x]"],
            ),
            ("a\u{a0}b \u{201c}c d\u{201d}", &["a\u{a0}b", "\u{201c}c", "d\u{201d}"]),
        ];
        for (text, expected) in accepted {
            assert_eq!(
                split(text).unwrap_or_else(|error| panic!("{text:?}: {error}")),
                *expected,
                "{text:?}"
            );
        }
        for refused in [
            "--url=$URL",
            "\"$HOME\"",
            "\"${x}\"",
            "--a=$(x)",
            "$1",
            "$\"x\"",
            "--a=`x`",
            "\"a`b`\"",
            "a;b",
            "a|b",
            "--url=https://example.com/?a=1&b=2",
            "a>b",
            "<b",
            "(a)",
            "--a={x,y}",
            "~/x",
            "--a=!x",
            "\"!x\"",
            "a\n b",
            "a \\\n",
            "a\\",
            "'open",
            "\"open",
            "$'open",
            r"$'\x00'",
            r"$'\xff'",
            r"$'\ud800'",
        ] {
            assert!(split(refused).is_err(), "{refused:?}: {:?}", split(refused));
        }
        let error = split("--a=1\n--b=2").expect_err("a second command");
        assert!(error.contains("backslash"), "{error}");
    }

    fn strings(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| item.to_string()).collect()
    }

    /// Each sign of a shell, found in a command line.
    #[test]
    fn the_signs_of_each_shell_are_found() {
        let shells = |text: &str| -> Vec<Shell> { shell_hints(text).iter().map(|hint| hint.shell).collect() };
        for text in [
            r"siteone-crawler --offline-export-dir=C:\Exports\site",
            r"siteone-crawler --a=\\server\share",
            "siteone-crawler '--user-agent=Sam''s site'",
            "siteone-crawler '--user-agent=it\u{2019}\u{2019}s'",
            "siteone-crawler --a=1 `\n  --b=2",
            "& 'C:\\Program Files\\siteone-crawler.exe' --a=1",
            "siteone-crawler \"--user-agent=say `\"hi`\"\"",
        ] {
            assert_eq!(shells(text), [Shell::PowerShell], "{text}");
        }
        for text in [
            "siteone-crawler --a=1 \\\n  --b=2",
            r"siteone-crawler '--user-agent=it'\''s me'",
            r#"siteone-crawler "--user-agent=say \"hi\"""#,
            r#"siteone-crawler "--a=cost \$5" "--b=C:\\x""#,
            r"siteone-crawler --a=$'x\ty'",
            "siteone-crawler 'a'b",
        ] {
            assert!(
                !shells(text).is_empty() && shells(text).iter().all(|shell| *shell == Shell::Posix),
                "{text}: {:?}",
                shell_hints(text)
            );
        }
        // Neither: what both read alike, or differently without a sign.
        for text in [
            "siteone-crawler --url=https://example.com/ --workers=4",
            "siteone-crawler '--a=x y' \"--b=C:\\My Exports\\site\" 'C:\\x'",
            r"siteone-crawler --a=x\ y",
        ] {
            assert!(shell_hints(text).is_empty(), "{text}: {:?}", shell_hints(text));
        }
    }

    /// Every outcome of reading a command line with both shells.
    #[test]
    fn the_shell_of_a_pasted_command_line_is_detected() {
        use CommandLineReading::*;
        let read = read_command_line;
        // Both read the same arguments: no shell is named.
        assert_eq!(
            read("siteone-crawler --url=https://example.com/ --user-agent='a b' \"--x=y z\""),
            Same(strings(&[
                "siteone-crawler",
                "--url=https://example.com/",
                "--user-agent=a b",
                "--x=y z"
            ]))
        );
        // Only one shell reads it.
        match read("& 'C:\\x\\siteone-crawler.exe' --a=1 `\n  --b=\"a`tb\"") {
            Only {
                shell: Shell::PowerShell,
                argv,
                other_error,
            } => {
                assert_eq!(argv, strings(&["&", r"C:\x\siteone-crawler.exe", "--a=1", "--b=a\tb"]));
                assert!(other_error.contains("&"), "{other_error}");
            }
            other => panic!("{other:?}"),
        }
        match read("siteone-crawler '--user-agent=it'\\''s me' \\\n  --b=\"cost \\$5\"") {
            Only {
                shell: Shell::Posix,
                argv,
                ..
            } => assert_eq!(
                argv,
                strings(&["siteone-crawler", "--user-agent=it's me", "--b=cost $5"])
            ),
            other => panic!("{other:?}"),
        }
        // Both read it, differently: the signs decide.
        assert_eq!(
            read(r"siteone-crawler --offline-export-dir=C:\Exports\site"),
            Detected {
                shell: Shell::PowerShell,
                argv: strings(&["siteone-crawler", r"--offline-export-dir=C:\Exports\site"]),
                hint: "a backslash in a path, as in C:\\…",
                other: strings(&["siteone-crawler", "--offline-export-dir=C:Exportssite"]),
            }
        );
        match read("siteone-crawler '--user-agent=Sam''s site'") {
            Detected {
                shell: Shell::PowerShell,
                argv,
                other,
                ..
            } => {
                assert_eq!(argv[1], "--user-agent=Sam's site");
                assert_eq!(other[1], "--user-agent=Sams site");
            }
            other => panic!("{other:?}"),
        }
        for (text, bash, powershell) in [
            (
                r#"siteone-crawler "--user-agent=say \"hi\"""#,
                &["siteone-crawler", "--user-agent=say \"hi\""][..],
                &["siteone-crawler", r"--user-agent=say \", r"hi\"][..],
            ),
            (
                r#"siteone-crawler "--a=C:\\x""#,
                &["siteone-crawler", r"--a=C:\x"],
                &["siteone-crawler", r"--a=C:\\x"],
            ),
            (
                r"siteone-crawler --a=$'x\ty'",
                &["siteone-crawler", "--a=x\ty"],
                &["siteone-crawler", r"--a=$x\ty"],
            ),
        ] {
            match read(text) {
                Detected {
                    shell: Shell::Posix,
                    argv,
                    other,
                    ..
                } => {
                    assert_eq!(argv, strings(bash), "{text}");
                    assert_eq!(other, strings(powershell), "{text}");
                }
                other => panic!("{text}: {other:?}"),
            }
        }
        // Different readings without a sign: the user chooses.
        assert_eq!(
            read(r"siteone-crawler --user-agent=a\ b"),
            Ambiguous {
                powershell: strings(&["siteone-crawler", r"--user-agent=a\", "b"]),
                bash: strings(&["siteone-crawler", "--user-agent=a b"]),
                hints: Vec::new(),
            }
        );
        // Signs of both: no guess either.
        match read(r#"siteone-crawler --offline-export-dir=C:\Exports\site "--a=C:\\x""#) {
            Ambiguous { hints, .. } => {
                let mut shells: Vec<Shell> = hints.iter().map(|hint| hint.shell).collect();
                shells.dedup();
                assert_eq!(shells, [Shell::PowerShell, Shell::Posix], "{hints:?}");
            }
            other => panic!("{other:?}"),
        }
        // Neither reads it: the error of the more plausible shell first.
        match read("siteone-crawler 'open") {
            Invalid {
                shell: Shell::PowerShell,
                error,
                other_error,
            } => {
                assert!(error.contains("unclosed quote"), "{error}");
                assert!(other_error.contains("unclosed quote"), "{other_error}");
            }
            other => panic!("{other:?}"),
        }
        match read("siteone-crawler --a=$'x' \"open") {
            Invalid {
                shell: Shell::Posix, ..
            } => {}
            other => panic!("{other:?}"),
        }
        match read("siteone-crawler --a=$x") {
            Invalid {
                shell: Shell::PowerShell,
                error,
                other_error,
            } => {
                assert!(error.contains("PowerShell variable"), "{error}");
                assert!(other_error.contains("Bash variable"), "{other_error}");
            }
            other => panic!("{other:?}"),
        }
    }

    /// What the app exports for either shell is read back as that shell (or both read it alike),
    /// with its argv.
    #[test]
    fn exported_command_lines_are_read_as_their_shell() {
        let values = [
            r"C:\Exports\site",
            r"C:\My Exports\site",
            r"\\server\share\dir\",
            r"C:\Exports\",
            "Sam's site",
            "it\u{2019}s \u{2018}typographic\u{2019} \u{201a}low\u{201b}",
            "two  spaces and\ttab",
            "\"double\" quotes",
            "say \"hi\"",
            "`backtick` and `n",
            "$literal $HOME ${x} $(y) cost: 5$",
            "line\nbreak",
            "#not-a-comment",
            "",
            "  leading and trailing  ",
            "a=b,c;d|e&f<g>h(i){j}[k]@l%m^n!o~p*q?r",
            "\u{fc}n\u{ef}c\u{f8}d\u{e9} \u{2013} \u{201c}quoted\u{201d} \u{201e}low\u{201c}",
            r"trailing backslash\",
            "'",
            "''",
            "\"",
            "-t=5.5",
            "https://example.com/?a=1&b=2#frag",
        ];
        let mut cases: Vec<Vec<String>> = Vec::new();
        for value in values {
            cases.push(vec![format!("--user-agent={value}")]);
            cases.push(vec![format!("--header:=X-Test: {value}")]);
            cases.push(vec![value.to_string()]);
        }
        let schema = schema();
        let mut config = CrawlConfig::new("https://example.com/?a=1&b=2");
        for (name, value) in [
            ("--offline-export-dir", Value::Text(r"C:\My Exports\site".to_string())),
            (
                "--output-html-report",
                Value::Text(r"C:\Exports\report.html".to_string()),
            ),
            ("--user-agent", Value::Text("Sam's \"bot\" $x `y".to_string())),
            ("--header", texts(&["X-A: a, b", "X-B: it\u{2019}s"])),
            ("--workers", Value::Text("3".to_string())),
        ] {
            config.set(&schema, name, value).expect(name);
        }
        cases.push(config.to_preview_argv(&schema, &SecretValues::default(), false));
        let mut failures = Vec::new();
        for argv in &cases {
            for shell in [Shell::PowerShell, Shell::Posix] {
                for program in [PROGRAM_NAME, r"C:\Program Files\SiteOne Crawler\siteone-crawler.exe"] {
                    let line = command_line(program, argv, shell);
                    let continuation = match shell {
                        Shell::PowerShell => " `\n  ",
                        Shell::Posix => " \\\n  ",
                    };
                    let quote = |arg: &str| match shell {
                        Shell::PowerShell => quote_powershell(arg).into_owned(),
                        Shell::Posix => quote_posix(arg).into_owned(),
                    };
                    let multiline = std::iter::once(program)
                        .chain(argv.iter().map(String::as_str))
                        .map(quote)
                        .collect::<Vec<_>>()
                        .join(continuation);
                    for line in [line, multiline] {
                        let got = match read_command_line(&line) {
                            CommandLineReading::Same(got) => got,
                            CommandLineReading::Only { shell: read, argv, .. }
                            | CommandLineReading::Detected { shell: read, argv, .. }
                                if read == shell =>
                            {
                                argv
                            }
                            other => {
                                failures.push(format!("{shell:?}: {line}\n    {other:?}"));
                                continue;
                            }
                        };
                        if got[got.len() - argv.len()..] != argv[..] {
                            failures.push(format!("{shell:?}: {line}\n    read {got:?}"));
                        }
                    }
                }
            }
        }
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }

    /// What the app exports for a shell, pasted back with that shell's rules, gives back the argv —
    /// and imports to the same configuration.
    #[test]
    fn every_exported_command_line_splits_back_to_its_argv() {
        let values = [
            r"C:\Exports\site",
            r"\\server\share\dir\",
            "Sam's site",
            "it\u{2019}s \u{2018}typographic\u{2019}",
            "two  spaces and\ttab",
            "\"double\" quotes",
            "`backtick` and `n",
            "$literal $HOME ${x} $(y) cost: 5$",
            "line\nbreak",
            "#not-a-comment",
            "",
            "  leading and trailing  ",
            "a=b,c;d|e&f<g>h(i){j}[k]@l%m^n!o~p*q?r",
            "ünïcødé – “quoted” „low“",
            r"trailing backslash\",
            "'",
            "''",
            "\"",
        ];
        let mut argv = vec!["--url=https://example.com/".to_string()];
        for value in values {
            argv.push(format!("--user-agent={value}"));
            argv.push(format!("--header:=X-Test: {value}"));
            argv.push(value.to_string());
        }
        for shell in [Shell::PowerShell, Shell::Posix] {
            for program in [
                "siteone-crawler",
                r"C:\Program Files\SiteOne Crawler\siteone-crawler.exe",
            ] {
                let line = command_line(program, &argv, shell);
                let split =
                    split_command_line(&line, shell).unwrap_or_else(|error| panic!("{shell:?}: {error}\n{line}"));
                let start = split.len() - argv.len();
                assert_eq!(split[start..], argv[..], "{shell:?}: {line}");
                assert_eq!(split[start - 1], program);

                // The same command with every argument on a line of its own.
                let continuation = match shell {
                    Shell::PowerShell => " `\n  ",
                    Shell::Posix => " \\\n  ",
                };
                let multiline = std::iter::once(program)
                    .chain(argv.iter().map(String::as_str))
                    .map(|arg| match shell {
                        Shell::PowerShell => quote_powershell(arg).into_owned(),
                        Shell::Posix => quote_posix(arg).into_owned(),
                    })
                    .collect::<Vec<_>>()
                    .join(continuation);
                let split = split_command_line(&multiline, shell).expect("splits");
                assert_eq!(split[1..], argv[..], "{shell:?}: {multiline}");
            }
        }

        let schema = schema();
        let mut config = CrawlConfig::new("https://example.com/");
        config
            .set(
                &schema,
                "--offline-export-dir",
                Value::Text(r"C:\Exports\site".to_string()),
            )
            .expect("set");
        config
            .set(
                &schema,
                "--header",
                texts(&["X-Name: Sam's site", "X-Cost: $5 `quoted`"]),
            )
            .expect("set");
        let argv = config.to_argv(&schema, &SecretValues::default());
        for shell in [Shell::PowerShell, Shell::Posix] {
            let line = command_line(PROGRAM_NAME, &argv, shell);
            let imported = import_args(&schema, &split_command_line(&line, shell).expect("splits"));
            assert_eq!(imported.config, config, "{shell:?}: {line}");
        }
    }

    #[test]
    fn relative_input_paths_are_made_absolute_and_nothing_else() {
        let schema = schema();
        let base = Path::new(if cfg!(windows) { r"C:\data" } else { "/data" });
        let absolute = if cfg!(windows) {
            r"D:\lists\urls.txt"
        } else {
            "/lists/urls.txt"
        };
        let mut config = CrawlConfig::new("https://example.com/");
        for (name, value) in [
            ("--url-list", "urls.txt"),
            ("--ai-schema-file", "schemas/%domain%.json"),
            ("--ci-baseline", absolute),
            ("--browser-path", "chrome.exe"),
            ("--ffmpeg-path", "ffmpeg"),
            ("--output-text-file", "out.txt"),
            ("--offline-export-dir", "offline"),
            ("--user-agent", "agent.txt"),
        ] {
            config.set(&schema, name, Value::Text(value.to_string())).expect(name);
        }
        let resolved = config.with_absolute_inputs(&schema, base);
        let get = |name: &str| match resolved.values.get(name) {
            Some(Value::Text(text)) => text.clone(),
            other => panic!("{name}: {other:?}"),
        };
        assert_eq!(get("--url-list"), base.join("urls.txt").display().to_string());
        // The CLI's placeholders stay for the CLI to fill.
        assert_eq!(
            get("--ai-schema-file"),
            base.join("schemas/%domain%.json").display().to_string()
        );
        assert_eq!(get("--ci-baseline"), absolute);
        assert_eq!(get("--browser-path"), base.join("chrome.exe").display().to_string());
        // A program name is looked up on PATH; with a directory part it is a path.
        assert_eq!(get("--ffmpeg-path"), "ffmpeg");
        let mut with_dir = config.clone();
        with_dir
            .set(&schema, "--ffmpeg-path", Value::Text("bin/ffmpeg.exe".to_string()))
            .expect("set");
        assert_eq!(
            with_dir.with_absolute_inputs(&schema, base).values.get("--ffmpeg-path"),
            Some(&Value::Text(base.join("bin/ffmpeg.exe").display().to_string()))
        );
        // Outputs land in the run folder, other text is no path.
        assert_eq!(get("--output-text-file"), "out.txt");
        assert_eq!(get("--offline-export-dir"), "offline");
        assert_eq!(get("--user-agent"), "agent.txt");
        assert_eq!(resolved.url, config.url);
    }

    #[test]
    fn normalized_drops_values_of_renamed_or_mistyped_options() {
        let schema = schema();
        let mut config = CrawlConfig::new("https://example.com/");
        config.values.insert("--renamed-option".to_string(), Value::Bool(true));
        config.values.insert("--workers".to_string(), Value::Bool(true));
        config
            .values
            .insert("--timeout".to_string(), Value::Text("9".to_string()));
        config
            .values
            .insert("--http-auth".to_string(), Value::Text("u:p".to_string()));
        let (clean, dropped) = config.normalized(&schema);
        assert_eq!(clean.values.len(), 1);
        assert_eq!(dropped.len(), 3);
    }

    #[test]
    fn the_configuration_serializes_untagged() {
        let schema = schema();
        let mut config = CrawlConfig::new("https://example.com/");
        config.set(&schema, "--single-page", Value::Bool(true)).expect("set");
        config
            .set(&schema, "--workers", Value::Text("5".to_string()))
            .expect("set");
        config.set(&schema, "--header", texts(&["X: 1"])).expect("set");
        let json = serde_json::to_string(&config).expect("json");
        assert_eq!(
            json,
            r#"{"url":"https://example.com/","aiEnabled":false,"values":{"--header":["X: 1"],"--single-page":true,"--workers":"5"}}"#
        );
        assert_eq!(serde_json::from_str::<CrawlConfig>(&json).expect("parses"), config);
    }

    #[test]
    fn the_external_link_check_is_kept_but_never_reaches_the_command() {
        let schema = schema();
        let mut config = CrawlConfig::new("https://example.com/");
        config
            .set(&schema, "--workers", Value::Text("2".to_string()))
            .expect("set");
        let secrets = SecretValues::default();
        let off = config.to_argv(&schema, &secrets);
        let preview_off = config.to_preview_argv(&schema, &secrets, false);
        config.check_external_links = true;
        assert_eq!(config.to_argv(&schema, &secrets), off);
        assert_eq!(config.to_preview_argv(&schema, &secrets, false), preview_off);
        assert_eq!(config.changed_count(&schema, &secrets), 1);

        // Stored (session, profiles, History) and normalized without loss.
        let json = serde_json::to_string(&config).expect("json");
        assert!(json.contains(r#""checkExternalLinks":true"#), "{json}");
        let parsed: CrawlConfig = serde_json::from_str(&json).expect("parses");
        assert_eq!(parsed, config);
        assert_eq!(parsed.normalized(&schema).0, config);
        // Files written before the switch existed read as off.
        let old: CrawlConfig = serde_json::from_str(r#"{"url":"https://example.com/","values":{}}"#).expect("parses");
        assert!(!old.check_external_links);
    }

    fn ignore_regex(argv: &[String]) -> Vec<&str> {
        argv.iter()
            .filter_map(|arg| arg.strip_prefix("--ignore-regex:="))
            .collect()
    }

    #[test]
    fn skipping_action_links_adds_the_patterns_after_the_users_own_and_the_cli_takes_them() {
        let schema = schema();
        let secrets = SecretValues::default();
        let mut config = CrawlConfig::new("https://shop.example/");
        assert!(config.skip_action_links, "on by default");
        let argv = config.to_argv(&schema, &secrets);
        assert_eq!(ignore_regex(&argv), ACTION_LINK_PATTERNS);
        // The command preview says it too, and it is no changed option.
        assert_eq!(
            ignore_regex(&config.to_preview_argv(&schema, &secrets, false)),
            ACTION_LINK_PATTERNS
        );
        assert_eq!(config.changed_count(&schema, &secrets), 0);

        // The user's patterns come first; one of ours the user has already is not repeated.
        config
            .set(&schema, IGNORE_REGEX, texts(&["/\\.pdf$/", "/[?&]add-to-cart=/"]))
            .expect("set");
        let argv = config.to_argv(&schema, &secrets);
        let mut expected = vec!["/\\.pdf$/", "/[?&]add-to-cart=/"];
        expected.extend(ACTION_LINK_PATTERNS.iter().skip(1));
        assert_eq!(ignore_regex(&argv), expected);
        let mut full = vec![PROGRAM_NAME.to_string()];
        full.extend(argv);
        full.push(format!("--config-file={}", empty_config_file().display()));
        let core = core_options::parse_argv(&full).expect("parses");
        assert_eq!(core.ignore_regex, expected);

        // Off: only the user's own patterns.
        config.skip_action_links = false;
        assert_eq!(
            ignore_regex(&config.to_argv(&schema, &secrets)),
            ["/\\.pdf$/", "/[?&]add-to-cart=/"]
        );
        // Written only when off; a file from before the switch reads as on.
        let json = serde_json::to_string(&config).expect("json");
        assert!(json.contains(r#""skipActionLinks":false"#), "{json}");
        assert_eq!(serde_json::from_str::<CrawlConfig>(&json).expect("parses"), config);
        assert_eq!(config.normalized(&schema).0, config);
        let old: CrawlConfig = serde_json::from_str(r#"{"url":"https://example.com/","values":{}}"#).expect("parses");
        assert!(old.skip_action_links);
        assert!(!serde_json::to_string(&old).expect("json").contains("skipActionLinks"));
    }

    #[test]
    fn the_patterns_match_action_links_and_nothing_else() {
        // Compiled and matched the way the crawler does (`Crawler::new`, `is_url_allowed_by_regexes`).
        let patterns: Vec<fancy_regex::Regex> = ACTION_LINK_PATTERNS
            .iter()
            .map(|pattern| {
                fancy_regex::Regex::new(&siteone_crawler::utils::extract_pcre_regex_pattern(pattern)).expect("compiles")
            })
            .collect();
        let skipped = |url: &str| patterns.iter().any(|pattern| pattern.is_match(url).expect("matches"));
        for action in [
            "https://shop.example/product/red-string/?add-to-cart=31",
            "https://shop.example/shop/?orderby=price&add-to-cart=33",
            "https://shop.example/cart/?remove_item=0c74b7f78409a4022a2c4c5a5ca3ee19&_wpnonce=1a2b3c",
            "https://shop.example/cart/?undo_item=0c74b7f78409a4022a2c4c5a5ca3ee19&_wpnonce=1a2b3c",
            "https://shop.example/wp-login.php?action=logout&_wpnonce=1a2b3c",
            "https://shop.example/wp-login.php?redirect_to=x&action=logout",
        ] {
            assert!(skipped(action), "{action}");
        }
        for page in [
            "https://shop.example/product/red-string/",
            "https://shop.example/cart/",
            "https://shop.example/blog/add-to-cart-buttons/",
            "https://shop.example/wp-login.php",
            "https://shop.example/wp-login.php?action=lostpassword",
            "https://shop.example/blog/post/?replytocom=5",
        ] {
            assert!(!skipped(page), "{page}");
        }
    }

    #[test]
    fn a_command_with_the_patterns_imports_as_the_switch_without_duplicates() {
        let schema = schema();
        let secrets = SecretValues::default();
        let mut config = CrawlConfig::new("https://shop.example/");
        config.set(&schema, IGNORE_REGEX, texts(&["/\\.pdf$/"])).expect("set");
        // The app's own command: the patterns become the switch again.
        let imported = import_args(&schema, &config.to_argv(&schema, &secrets));
        assert_eq!(imported.config, config);
        assert_eq!(
            ignore_regex(&imported.config.to_argv(&schema, &secrets)),
            ignore_regex(&config.to_argv(&schema, &secrets))
        );
        // A command without them gets the app's default (on), like a missing `--timeout`.
        let imported = import_args(&schema, &["--url=https://shop.example/".to_string()]);
        assert!(imported.config.skip_action_links);
        // A command with only some of them keeps those as the user's own; none is passed twice.
        let imported = import_args(
            &schema,
            &[
                "--url=https://shop.example/".to_string(),
                "--ignore-regex:=/[?&]add-to-cart=/".to_string(),
            ],
        );
        assert_eq!(
            imported.config.values.get(IGNORE_REGEX),
            Some(&texts(&["/[?&]add-to-cart=/"]))
        );
        assert_eq!(
            ignore_regex(&imported.config.to_argv(&schema, &secrets)),
            ACTION_LINK_PATTERNS
        );
    }
}
