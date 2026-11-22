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
/// - [`Shell::Posix`]: POSIX shell rules (`shlex`); `\` before a line break continues the line
///   (outside single quotes).
/// - [`Shell::PowerShell`]: PowerShell's rules for the arguments of a program ([`split_powershell`]).
pub fn split_command_line(text: &str, shell: Shell) -> Result<Vec<String>, String> {
    let text = text.replace("\r\n", "\n");
    match shell {
        Shell::Posix => shlex::split(&join_posix_continuations(&text))
            .ok_or_else(|| "The command line has an unclosed quote or a trailing backslash.".to_string()),
        Shell::PowerShell => split_powershell(&text),
    }
}

/// Removes each `\` + line break outside single quotes: a POSIX shell's line continuation, which
/// `shlex` does not join.
fn join_posix_continuations(text: &str) -> String {
    let mut joined = String::with_capacity(text.len());
    let (mut single, mut double) = (false, false);
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        match c {
            '\'' if !double => single = !single,
            '"' if !single => double = !double,
            '\\' if !single => {
                match chars.next() {
                    Some('\n') => {}
                    Some(next) => {
                        joined.push(c);
                        joined.push(next);
                    }
                    None => joined.push(c),
                }
                continue;
            }
            _ => {}
        }
        joined.push(c);
    }
    joined
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

/// Splits a command line the way PowerShell reads the arguments of a program: whitespace separates
/// arguments and adjacent parts form one; `'…'` is literal, a doubled single quote inside it is one;
/// `"…"` takes the backtick escapes (`` `n ``, `` `t ``, `` `" ``, …) and a doubled `"`; outside
/// quotes a backtick takes the next character literally, and before a line break it continues the
/// line; `#` at the start of an argument begins a comment. A variable or expression (`$name`,
/// `$(…)`) outside single quotes is refused: an import cannot know its value.
fn split_powershell(text: &str) -> Result<Vec<String>, String> {
    const UNCLOSED: &str = "The command line has an unclosed quote.";
    const EXPANSION: &str = "The command line uses a PowerShell variable or expression ($…), which an import cannot evaluate. Replace it with its value.";
    let escape = |c: char| match c {
        '0' => '\0',
        'a' => '\u{7}',
        'b' => '\u{8}',
        'e' => '\u{1b}',
        'f' => '\u{c}',
        'n' => '\n',
        'r' => '\r',
        't' => '\t',
        'v' => '\u{b}',
        other => other,
    };
    let chars: Vec<char> = text.chars().collect();
    let mut args = Vec::new();
    let mut current = String::new();
    let mut in_arg = false;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        i += 1;
        match c {
            ' ' | '\t' | '\n' => {
                if in_arg {
                    args.push(std::mem::take(&mut current));
                    in_arg = false;
                }
            }
            '#' if !in_arg => {
                while i < chars.len() && chars[i] != '\n' {
                    i += 1;
                }
            }
            '`' => match chars.get(i) {
                Some('\n') => {
                    i += 1;
                    if in_arg {
                        args.push(std::mem::take(&mut current));
                        in_arg = false;
                    }
                }
                Some(&next) => {
                    current.push(next);
                    in_arg = true;
                    i += 1;
                }
                None => return Err("The command line ends with a backtick.".to_string()),
            },
            '$' if starts_ps_expansion(chars.get(i)) => return Err(EXPANSION.to_string()),
            c if is_ps_single_quote(c) => {
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
                in_arg = true;
                loop {
                    match chars.get(i) {
                        None => return Err(UNCLOSED.to_string()),
                        Some('`') => match chars.get(i + 1) {
                            Some(&next) => {
                                current.push(escape(next));
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
/// quotes, where every single quote — PowerShell also takes the typographic ones — is doubled.
pub fn quote_powershell(arg: &str) -> Cow<'_, str> {
    let safe = |c: char| c.is_ascii_alphanumeric() || "-_./:=\\".contains(c);
    if !arg.is_empty() && arg.chars().all(safe) {
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
