//! The option schema: every crawl option of the CLI, read from the crawler's own metadata
//! (`options::core_options::get_options()`), plus the small GUI annotation tables of this module.
//!
//! The GUI never keeps its own list of options: forms, defaults, help texts and validation come
//! from the code the CLI uses, so the two cannot drift apart. A unit test fails when an annotation
//! names an option that no longer exists.

use std::collections::HashMap;

use siteone_crawler::options::core_options::{self, GROUP_AI_SETTINGS};
use siteone_crawler::options::option_type::OptionType;
use siteone_crawler::options::options::Options;

use super::config::Value;

/// An option's long name including the leading dashes, e.g. `--workers`.
pub type OptionName = String;

/// Whether the GUI lets a person edit an option.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Visibility {
    /// An ordinary crawl setting.
    Editable,
    /// Set by the GUI for every run (GUI-SPEC §4.2); never shown or stored.
    Managed,
    /// Not a crawl setting (utility modes) or edited through a dedicated field (`--url`,
    /// `--ai-api-key`).
    Hidden,
}

/// What kind of path an option takes, which decides the Browse dialog.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathRole {
    Dir,
    /// A file the crawler writes (save dialog).
    OutputFile,
    /// A file the crawler reads (open dialog).
    InputFile,
}

/// A fixed value set of a string option.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Choices {
    pub values: Vec<String>,
    /// Several values may be chosen. For a list option each chosen value is one item; for a text
    /// option the chosen values are joined with `,` into one value (the CLI splits it itself).
    pub multiple: bool,
}

#[derive(Debug, Clone)]
pub struct OptionSpec {
    /// Long name, e.g. `--workers`.
    pub name: OptionName,
    /// Short or alternative name, e.g. `-w`.
    pub alt_name: Option<String>,
    /// The crawler's camelCase property, e.g. `workers` (the key in the result JSON's `options`).
    pub property: String,
    /// Code of the group, e.g. `basic-settings`.
    pub group: String,
    pub kind: OptionType,
    /// A list option: several values, given by repeating the option.
    pub is_list: bool,
    /// The default as text (`None` = no default): the CLI's, or the app's ([`APP_DEFAULTS`]).
    pub default: Option<String>,
    /// The default as a configuration value (see [`Value`]).
    pub default_value: Value,
    /// The default is the app's, not the CLI's: the option is passed also with its default.
    pub app_default: bool,
    /// The CLI's help text.
    pub description: String,
    pub nullable: bool,
    /// Inclusive numeric range of `Int`/`Float` options.
    pub range: Option<(f64, f64)>,
    pub visibility: Visibility,
    pub choices: Option<Choices>,
    pub secret: bool,
    pub path_role: Option<PathRole>,
}

impl OptionSpec {
    pub fn is_ai(&self) -> bool {
        self.group == GROUP_AI_SETTINGS
    }

    pub fn is_editable(&self) -> bool {
        self.visibility == Visibility::Editable
    }
}

#[derive(Debug, Clone)]
pub struct GroupSpec {
    pub code: String,
    pub name: String,
    /// Indices into [`OptionSchema::options`], in CLI order.
    pub options: Vec<usize>,
}

/// All options of the CLI in CLI order (groups in order, options in order within a group).
#[derive(Debug, Clone)]
pub struct OptionSchema {
    options: Vec<OptionSpec>,
    groups: Vec<GroupSpec>,
    /// Long and alternative names → index.
    index: HashMap<String, usize>,
}

/// Options the GUI sets itself for every run (GUI-SPEC §4.2, §5.1).
pub const MANAGED: &[&str] = &[
    "--events-file",
    "--control-stdin",
    "--output",
    "--force-color",
    "--no-color",
    "--console-width",
    "--hide-progress-bar",
    "--output-json-file",
];

/// Options that are no crawl setting, or that have a dedicated field (`--url`, `--ai-api-key`).
pub const HIDDEN: &[&str] = &[
    "--help",
    "--version",
    "--serve-markdown",
    "--serve-offline",
    "--serve-port",
    "--serve-bind-address",
    "--html-to-markdown",
    "--html-to-markdown-output",
    "--ai-list-models",
    "--ai-check",
    "--url",
    "--ai-api-key",
];

/// Defaults of the app that differ from the CLI's. Such an option is passed on every run, so the
/// command shows what the run does.
///
/// `--timeout`: the CLI's 5 s is too short for a slow site that has to build a page its cache no
/// longer holds.
pub const APP_DEFAULTS: &[(&str, &str)] = &[("--timeout", "30")];

/// Options whose values live in the credential store, never in a file.
pub const SECRET: &[&str] = &["--http-auth", "--mail-smtp-pass", "--upload-password", "--ai-api-key"];

/// Path roles that do not follow from the option type (`File` → output file, `Dir` → directory).
const PATH_ROLES: &[(&str, PathRole)] = &[
    ("--url-list", PathRole::InputFile),
    ("--ai-prompt-file", PathRole::InputFile),
    ("--ai-api-key-file", PathRole::InputFile),
    ("--ai-schema-file", PathRole::InputFile),
    ("--ci-baseline", PathRole::InputFile),
    ("--browser-path", PathRole::InputFile),
    ("--ffmpeg-path", PathRole::InputFile),
    ("--screenshots-dir", PathRole::Dir),
];

/// Fixed value sets, taken from the option descriptions and the crawler's validation code
/// (`CoreOptions::from_options`, `DeviceType::from_text`, `StorageType::from_text`).
/// `(option, values, multiple)`; `--ai-report` is built from the crawler's preset list.
const CHOICES: &[(&str, &[&str], bool)] = &[
    ("--device", &["desktop", "tablet", "mobile"], false),
    ("--output", &["text", "json"], false),
    ("--hide-columns", &["type", "time", "size", "cache"], true),
    ("--result-storage", &["memory", "file"], false),
    (
        "--html-report-options",
        &[
            "summary",
            "seo-opengraph",
            "image-gallery",
            "video-gallery",
            "visited-urls",
            "dns-ssl",
            "crawler-stats",
            "crawler-info",
            "headers",
            "content-types",
            "skipped-urls",
            "caching",
            "best-practices",
            "accessibility",
            "security",
            "redirects",
            "404-pages",
            "slowest-urls",
            "fastest-urls",
            "source-domains",
            "technologies",
        ],
        true,
    ),
    (
        "--sitemap-changefreq",
        &["always", "hourly", "daily", "weekly", "monthly", "yearly", "never"],
        false,
    ),
    (
        "--upload-retention",
        &["1h", "4h", "12h", "24h", "3d", "7d", "30d", "365d", "forever"],
        false,
    ),
    (
        "--ai-provider",
        &["openai", "anthropic", "gemini", "openai-compatible"],
        false,
    ),
    (
        "--ai-actions",
        &["seo", "llms-txt", "llms-full", "typos", "custom", "summary", "extract"],
        true,
    ),
    ("--ai-schema-enforce", &["auto", "on", "off"], false),
    (
        "--ai-elaborate-template",
        &["auto", "corporate", "personal", "product"],
        false,
    ),
    (
        "--ai-profile-template",
        &[
            "auto",
            "corporate",
            "smb-services",
            "product",
            "ecommerce",
            "personal",
            "media-news",
            "expert-content",
            "nonprofit",
            "government",
            "institution",
            "portal-directory",
            "events-culture",
            "general",
        ],
        false,
    ),
    ("--browser-wait", &["load", "domcontentloaded", "networkidle"], false),
    ("--screenshot-mode", &["viewport", "full-page"], false),
    ("--screenshot-format", &["png", "jpg", "webp"], false),
    ("--screenshots-animation", &["gif", "mp4"], true),
];

/// `--ai-report` takes a preset key of the crawler or `extract`.
const AI_REPORT: &str = "--ai-report";

impl OptionSchema {
    /// The schema of the linked crawler.
    ///
    /// `get_options()` creates `./tmp` in the current directory, so the GUI sets its working
    /// directory to its data directory before the first call (GUI-SPEC §2).
    pub fn load() -> Self {
        Self::from_options(&core_options::get_options())
    }

    pub fn from_options(source: &Options) -> Self {
        let mut options = Vec::new();
        let mut groups = Vec::new();
        let mut index = HashMap::new();

        for group in source.get_groups().values() {
            let mut members = Vec::new();
            for option in group.options.values() {
                let at = options.len();
                index.insert(option.name.clone(), at);
                if let Some(alt) = &option.alt_name {
                    index.insert(alt.clone(), at);
                }
                members.push(at);

                let name = option.name.as_str();
                let visibility = if MANAGED.contains(&name) {
                    Visibility::Managed
                } else if HIDDEN.contains(&name) {
                    Visibility::Hidden
                } else {
                    Visibility::Editable
                };
                let path_role = PATH_ROLES
                    .iter()
                    .find(|(option_name, _)| *option_name == name)
                    .map(|(_, role)| *role)
                    .or(match option.option_type {
                        OptionType::File => Some(PathRole::OutputFile),
                        OptionType::Dir => Some(PathRole::Dir),
                        _ => None,
                    });
                let range = option
                    .extras
                    .as_ref()
                    .filter(|extras| extras.len() == 2)
                    .and_then(|extras| Some((extras[0].parse().ok()?, extras[1].parse().ok()?)));

                let app_default = APP_DEFAULTS
                    .iter()
                    .find(|(option_name, _)| *option_name == name)
                    .map(|(_, value)| value.to_string());
                let default = app_default.clone().or_else(|| option.default_value.clone());

                options.push(OptionSpec {
                    name: option.name.clone(),
                    alt_name: option.alt_name.clone(),
                    property: option.property_to_fill.clone(),
                    group: group.apl_code.clone(),
                    kind: option.option_type,
                    is_list: option.is_array,
                    default_value: default_value(option.option_type, option.is_array, default.as_deref()),
                    default,
                    app_default: app_default.is_some(),
                    description: option.description.clone(),
                    nullable: option.is_nullable,
                    range,
                    visibility,
                    choices: choices_for(name),
                    secret: SECRET.contains(&name),
                    path_role,
                });
            }
            groups.push(GroupSpec {
                code: group.apl_code.clone(),
                name: group.name.clone(),
                options: members,
            });
        }

        Self { options, groups, index }
    }

    /// All options in CLI order.
    pub fn options(&self) -> &[OptionSpec] {
        &self.options
    }

    pub fn groups(&self) -> &[GroupSpec] {
        &self.groups
    }

    /// Looks an option up by its long or alternative name.
    pub fn get(&self, name: &str) -> Option<&OptionSpec> {
        self.index.get(name).and_then(|at| self.options.get(*at))
    }

    /// The options of a group, in CLI order.
    pub fn group_options<'a>(&'a self, group: &'a GroupSpec) -> impl Iterator<Item = &'a OptionSpec> + 'a {
        group.options.iter().filter_map(|at| self.options.get(*at))
    }

    /// Long names of the secret options.
    pub fn secret_options(&self) -> impl Iterator<Item = &str> {
        self.options
            .iter()
            .filter(|spec| spec.secret)
            .map(|spec| spec.name.as_str())
    }
}

/// The CLI default as a configuration value, interpreted the way the CLI's parser interprets it:
/// a list default is split on commas, a missing text default is the empty text.
fn default_value(kind: OptionType, is_list: bool, default: Option<&str>) -> Value {
    if is_list {
        Value::List(
            default
                .unwrap_or_default()
                .split(',')
                .map(str::trim)
                .filter(|item| !item.is_empty())
                .map(str::to_string)
                .collect(),
        )
    } else if kind == OptionType::Bool {
        Value::Bool(matches!(default, Some("1" | "yes" | "true")))
    } else {
        Value::Text(default.unwrap_or_default().to_string())
    }
}

fn choices_for(name: &str) -> Option<Choices> {
    if name == AI_REPORT {
        let mut values: Vec<String> = siteone_crawler::ai::report::presets::PRESET_KEYS
            .iter()
            .map(|key| key.to_string())
            .collect();
        values.push("extract".to_string());
        return Some(Choices {
            values,
            multiple: false,
        });
    }
    CHOICES
        .iter()
        .find(|(option, _, _)| *option == name)
        .map(|(_, values, multiple)| Choices {
            values: values.iter().map(|value| value.to_string()).collect(),
            multiple: *multiple,
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::test_support::{check, crawler_argv};

    fn schema() -> OptionSchema {
        OptionSchema::load()
    }

    #[test]
    fn every_annotation_names_an_existing_option() {
        let schema = schema();
        let annotated = MANAGED
            .iter()
            .chain(HIDDEN)
            .chain(SECRET)
            .chain(PATH_ROLES.iter().map(|(name, _)| name))
            .chain(CHOICES.iter().map(|(name, _, _)| name))
            .chain(std::iter::once(&AI_REPORT));
        for name in annotated {
            let spec = schema
                .get(name)
                .unwrap_or_else(|| panic!("annotation names a missing option {name}"));
            assert_eq!(spec.name, *name, "annotations use long names");
        }
    }

    #[test]
    fn the_schema_mirrors_the_cli_option_list() {
        let schema = schema();
        let source = core_options::get_options();
        let count: usize = source.get_groups().values().map(|group| group.options.len()).sum();
        assert_eq!(schema.options().len(), count);
        assert_eq!(schema.groups().len(), source.get_groups().len());
        assert!(schema.groups().len() >= 18);
        // Names are unique, alt names resolve to their option.
        let workers = schema.get("-w").expect("alt name");
        assert_eq!(workers.name, "--workers");
        assert_eq!(workers.default_value, Value::Text("3".to_string()));
        assert_eq!(
            schema.get("--ai-actions").map(|spec| spec.default_value.clone()),
            Some(Value::List(vec![
                "seo".to_string(),
                "typos".to_string(),
                "summary".to_string()
            ]))
        );
        assert_eq!(
            schema
                .get("--offline-export-remove-unwanted-code")
                .map(|spec| spec.default_value.clone()),
            Some(Value::Bool(true))
        );
        assert_eq!(
            schema.get("--max-heading-level").and_then(|spec| spec.range),
            Some((1.0, 6.0))
        );
    }

    #[test]
    fn visibility_secret_and_path_annotations_apply() {
        let schema = schema();
        let get = |name: &str| schema.get(name).expect(name).clone();
        assert_eq!(get("--events-file").visibility, Visibility::Managed);
        assert_eq!(get("--url").visibility, Visibility::Hidden);
        assert_eq!(get("--workers").visibility, Visibility::Editable);
        assert!(get("--ai-api-key").secret);
        assert!(get("--http-auth").secret);
        assert!(!get("--header").secret);
        assert_eq!(get("--url-list").path_role, Some(PathRole::InputFile));
        assert_eq!(get("--offline-export-dir").path_role, Some(PathRole::Dir));
        assert_eq!(get("--sitemap-xml-file").path_role, Some(PathRole::OutputFile));
        assert_eq!(get("--screenshots-dir").path_role, Some(PathRole::Dir));
        assert!(
            get("--ai-report")
                .choices
                .is_some_and(|c| c.values.contains(&"ia".to_string()))
        );
        assert!(get("--ai-actions").is_ai());
        assert!(!get("--workers").is_ai());
    }

    #[test]
    fn every_choice_is_named_in_the_option_description() {
        let schema = schema();
        for spec in schema.options() {
            let Some(choices) = &spec.choices else { continue };
            assert!(
                spec.kind == OptionType::String,
                "{} has choices but is not a string",
                spec.name
            );
            for value in &choices.values {
                assert!(
                    spec.description.contains(value.as_str()),
                    "{}: `{value}` is not in its description",
                    spec.name
                );
            }
            if spec.is_list {
                assert!(choices.multiple, "{} is a list, so its chips are multiple", spec.name);
            }
        }
    }

    /// Options a value of their set needs to be accepted at all (cross-option rules of the CLI).
    fn context(option: &str) -> Vec<&'static str> {
        let ai = vec![
            "--ai-endpoint=http://127.0.0.1:9/v1",
            "--ai-model=m",
            "--ai-provider=openai-compatible",
        ];
        match option {
            "--ai-provider" => vec!["--ai-endpoint=http://127.0.0.1:9/v1", "--ai-model=m"],
            "--ai-schema-enforce" | "--ai-elaborate-template" => ai,
            "--ai-profile-template" => [ai, vec!["--ai-profile"]].concat(),
            "--browser-wait" => vec!["--browser"],
            "--screenshot-mode" | "--screenshot-format" => vec!["--browser", "--screenshots"],
            "--screenshots-animation" => vec!["--browser", "--screenshots"],
            _ => Vec::new(),
        }
    }

    #[test]
    fn every_choice_is_accepted_by_the_cli_and_a_wrong_value_is_rejected() {
        let schema = schema();
        // Validated by the CLI itself; the others are free text the CLI interprets leniently.
        let validated = [
            "--device",
            "--result-storage",
            "--sitemap-changefreq",
            "--ai-provider",
            "--ai-schema-enforce",
            "--ai-elaborate-template",
            "--ai-profile-template",
            "--browser-wait",
            "--screenshot-mode",
            "--screenshot-format",
            "--screenshots-animation",
        ];
        for spec in schema.options() {
            let Some(choices) = &spec.choices else { continue };
            // `--ai-report` needs more context; `ai_actions_and_report_presets_are_accepted` covers it.
            if spec.is_list || spec.visibility != Visibility::Editable || spec.name == AI_REPORT {
                continue;
            }
            let extra = context(&spec.name);
            for value in &choices.values {
                let mut args: Vec<String> = extra.iter().map(|arg| arg.to_string()).collect();
                args.push(format!("{}={}", spec.name, value));
                let result = check(&crawler_argv(&args));
                assert!(result.is_ok(), "{}={value} rejected: {result:?}", spec.name);
            }
            if validated.contains(&spec.name.as_str()) {
                let mut args: Vec<String> = extra.iter().map(|arg| arg.to_string()).collect();
                args.push(format!("{}=not-a-choice", spec.name));
                assert!(check(&crawler_argv(&args)).is_err(), "{} accepts anything", spec.name);
            }
        }
    }

    #[test]
    fn ai_actions_and_report_presets_are_accepted() {
        let ai = [
            "--ai-provider=openai-compatible",
            "--ai-endpoint=http://127.0.0.1:9/v1",
            "--ai-model=m",
        ];
        let schema = schema();
        for action in &schema
            .get("--ai-actions")
            .expect("ai-actions")
            .choices
            .clone()
            .expect("set")
            .values
        {
            let mut args: Vec<String> = ai.iter().map(|arg| arg.to_string()).collect();
            args.push(format!("--ai-actions:={action}"));
            match action.as_str() {
                "custom" => args.push("--ai-prompt=Check it".to_string()),
                "extract" => args.push("--ai-report=ia".to_string()),
                _ => {}
            }
            let result = check(&crawler_argv(&args));
            assert!(result.is_ok(), "--ai-actions {action}: {result:?}");
        }
        for preset in &schema
            .get(AI_REPORT)
            .expect("ai-report")
            .choices
            .clone()
            .expect("set")
            .values
        {
            let mut args: Vec<String> = ai.iter().map(|arg| arg.to_string()).collect();
            args.push(format!("--ai-report={preset}"));
            if preset == "extract" {
                args.push("--ai-extract-fields=title:string".to_string());
            }
            let result = check(&crawler_argv(&args));
            assert!(result.is_ok(), "--ai-report={preset}: {result:?}");
        }
    }
}
