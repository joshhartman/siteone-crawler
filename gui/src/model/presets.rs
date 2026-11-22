//! The presets of the CLI wizard (`siteone_crawler::wizard::presets::PRESETS`), as override sets.
//!
//! Each preset is converted with the wizard's own `WizardState::build_argv()`, read back by
//! [`import_args`], so a preset means exactly what it means in the CLI. The wizard's path
//! placeholders `{domain}` and `{date}` become the CLI's `%domain%` and `%date%`, which the crawler
//! fills in at run time — so a preset does not depend on the URL typed later.
//!
//! The crawler keeps that module private (`mod presets;` in `src/wizard/mod.rs`, nothing
//! re-exported), so the GUI compiles the same source file itself: the preset table stays
//! single-sourced. Making it `pub` in the crawler would let this include become a plain `use`.

use std::collections::BTreeMap;

use super::config::{CrawlConfig, Value, import_args};
use super::schema::{OptionName, OptionSchema};

/// The crawler's `src/wizard/presets.rs`, compiled into the GUI. Parts the GUI does not use
/// (`content_summary`, `resolve_export_path`, …) are allowed to be unused.
#[path = "../../../src/wizard/presets.rs"]
#[allow(dead_code)]
mod wizard_presets;

use wizard_presets::{PRESETS, WizardState};

/// The tests of the included file call the wizard's `resolve_export_paths` as
/// `super::super::resolve_export_paths`; this is that function (`src/wizard/mod.rs`).
#[cfg(test)]
fn resolve_export_paths(state: &mut WizardState) {
    let url = state.url.clone();
    for path in [
        &mut state.offline_export_dir,
        &mut state.markdown_export_dir,
        &mut state.sitemap_xml_file,
        &mut state.sitemap_txt_file,
    ]
    .into_iter()
    .flatten()
    {
        if path.contains("{domain}") || path.contains("{date}") {
            *path = wizard_presets::resolve_export_path(path, &url);
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Preset {
    pub name: &'static str,
    pub description: &'static str,
    /// The options the preset changes (never AI options).
    pub overrides: BTreeMap<OptionName, Value>,
}

/// The wizard's presets, in wizard order.
pub fn presets(schema: &OptionSchema) -> Vec<Preset> {
    PRESETS
        .iter()
        .map(|preset| {
            let mut state = WizardState::from_preset(preset);
            state.url = "https://example.com/".to_string();
            let args: Vec<String> = state
                .build_argv()
                .into_iter()
                .map(|arg| arg.replace("{domain}", "%domain%").replace("{date}", "%date%"))
                .collect();
            let imported = import_args(schema, &args);
            Preset {
                name: preset.name,
                description: preset.description,
                overrides: imported.config.values,
            }
        })
        .collect()
}

/// Applies a preset: the non-AI overrides are replaced by the preset's; the URL, the AI switch and
/// the AI options are kept.
pub fn apply(config: &mut CrawlConfig, preset: &Preset, schema: &OptionSchema) {
    config
        .values
        .retain(|name, _| schema.get(name).is_some_and(|spec| spec.is_ai()));
    config.values.extend(
        preset
            .overrides
            .iter()
            .map(|(name, value)| (name.clone(), value.clone())),
    );
}

/// The preset whose overrides are exactly the configuration's non-AI overrides, if any.
pub fn matching<'a>(config: &CrawlConfig, presets: &'a [Preset], schema: &OptionSchema) -> Option<&'a Preset> {
    let non_ai: BTreeMap<&OptionName, &Value> = config
        .values
        .iter()
        .filter(|(name, _)| !schema.get(name).is_some_and(|spec| spec.is_ai()))
        .collect();
    presets.iter().find(|preset| {
        preset.overrides.len() == non_ai.len()
            && preset
                .overrides
                .iter()
                .all(|(name, value)| non_ai.get(name).is_some_and(|other| *other == value))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::config::check_argv;
    use crate::model::secrets::SecretValues;
    use crate::model::test_support::empty_config_file;

    #[test]
    fn every_wizard_preset_converts_cleanly_and_validates() {
        let schema = OptionSchema::load();
        let presets = presets(&schema);
        assert_eq!(presets.len(), 11);
        for preset in PRESETS {
            let mut state = WizardState::from_preset(preset);
            state.url = "https://example.com/".to_string();
            let imported = import_args(&schema, &state.build_argv());
            assert!(
                imported.unknown.is_empty() && imported.invalid.is_empty() && imported.ignored.is_empty(),
                "{}: {imported:?}",
                preset.name
            );
        }
        for preset in &presets {
            let mut config = CrawlConfig::new("https://example.com/");
            apply(&mut config, preset, &schema);
            let mut argv = config.to_argv(&schema, &SecretValues::default());
            argv.push(format!("--config-file={}", empty_config_file().display()));
            assert!(check_argv(&argv).is_ok(), "{}: {argv:?}", preset.name);
        }
        let custom = presets.iter().find(|preset| preset.name == "Custom").expect("Custom");
        assert!(custom.overrides.is_empty());
    }

    #[test]
    fn presets_carry_the_wizard_values() {
        let schema = OptionSchema::load();
        let presets = presets(&schema);
        let quick = &presets[0];
        assert_eq!(quick.name, "Quick Audit");
        assert_eq!(quick.overrides.get("--workers"), Some(&Value::Text("5".to_string())));
        assert_eq!(
            quick.overrides.get("--extra-columns"),
            Some(&Value::List(vec!["Title(20)".to_string()]))
        );
        let offline = presets
            .iter()
            .find(|preset| preset.name == "Offline Clone")
            .expect("preset");
        assert_eq!(
            offline.overrides.get("--offline-export-dir"),
            Some(&Value::Text("./tmp/offline-%domain%-%date%/".to_string()))
        );
        assert_eq!(offline.overrides.get("--no-cache"), Some(&Value::Bool(true)));
    }

    #[test]
    fn applying_a_preset_keeps_url_and_ai_and_replaces_the_rest() {
        let schema = OptionSchema::load();
        let presets = presets(&schema);
        let mut config = CrawlConfig::new("https://mysite.cz/");
        config.ai_enabled = true;
        config
            .set(&schema, "--ai-model", Value::Text("m".to_string()))
            .expect("set");
        config
            .set(&schema, "--max-depth", Value::Text("2".to_string()))
            .expect("set");
        let seo = presets
            .iter()
            .find(|preset| preset.name == "SEO Analysis")
            .expect("preset");
        apply(&mut config, seo, &schema);
        assert_eq!(config.url, "https://mysite.cz/");
        assert!(config.ai_enabled);
        assert_eq!(config.values.get("--ai-model"), Some(&Value::Text("m".to_string())));
        assert!(!config.values.contains_key("--max-depth"));
        assert_eq!(config.values.get("--disable-images"), Some(&Value::Bool(true)));
        assert_eq!(
            matching(&config, &presets, &schema).map(|preset| preset.name),
            Some("SEO Analysis")
        );
        config
            .set(&schema, "--workers", Value::Text("2".to_string()))
            .expect("set");
        assert_eq!(matching(&config, &presets, &schema), None);
    }
}
