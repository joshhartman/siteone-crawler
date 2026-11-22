//! The AI panel of the Simple view (GUI-SPEC §6.2): connection (provider, endpoint, API key, model
//! with Load models, Test connection), what to run, and limits. Everything else of the AI group is
//! in All options.

use std::time::{Duration, Instant};

use eframe::egui::{self, RichText, TextEdit, Ui};
use egui_material_icons::icons;

use super::editor::Editing;
use super::simple;
use crate::app::App;
use crate::backend::tools;
use crate::model::config::Value;
use crate::model::run_plan;
use crate::theme;
use crate::ui::widgets::{self, Tone, format};

const CONTROL_WIDTH: f32 = 380.0;
const MODELS_TIMEOUT: Duration = Duration::from_secs(30);
const CHECK_TIMEOUT: Duration = Duration::from_secs(120);

const MODEL: &str = "--ai-model";

/// From this width on, the panel shows the connection next to what to run.
const TWO_COLUMNS_FROM: f32 = 1040.0;

/// (option, label, note after the control — what the switch produces, in plain words).
type Field = (&'static str, &'static str, &'static str);

const CONNECTION: &[Field] = &[
    ("--ai-provider", "Provider", ""),
    ("--ai-endpoint", "Endpoint", ""),
    ("--ai-api-key", "API key", ""),
];

const WHAT_TO_RUN: &[Field] = &[
    ("--ai-actions", "Per-page analyses", ""),
    ("--ai-report", "Report preset", "a structured report over all pages"),
    // Short enough to stay on the switch's line in the two-column layout.
    ("--ai-elaborate", "Brand profile", "one long profile of the brand"),
    ("--ai-profile", "Subject profile", "a profile in chapters"),
    (
        "--ai-consistency",
        "Consistency check",
        "contradicting facts across pages",
    ),
    ("--ai-geo", "AI search readiness", "how AI search sees the site"),
    ("--ai-report-language", "Output language", "e.g. en, cs, de"),
];

const LIMITS: &[Field] = &[
    ("--ai-max-pages", "Pages sent to the model", "at most"),
    ("--ai-max-concurrency", "Concurrent AI requests", ""),
    ("--ai-dry-run", "Dry run", "plan only, no API calls"),
];

pub fn show(app: &mut App, ui: &mut Ui) {
    let p = theme::palette(ui);
    if !app.session.config.ai_enabled {
        ui.label(RichText::new(
            "Turn AI analysis on to let a language model review the crawled pages: SEO suggestions, typos, summaries, reports and profiles. It works with OpenAI, Anthropic, Gemini or any OpenAI-compatible server (e.g. a local vLLM or Ollama).",
        ).color(p.text_weak));
        return;
    }

    if ui.available_width() >= TWO_COLUMNS_FROM {
        ui.scope(|ui| {
            // The gutter between the columns only: the columns keep the normal spacing.
            let spacing = ui.spacing().item_spacing.x;
            ui.spacing_mut().item_spacing.x = theme::GRID * 3.0;
            ui.columns(2, |columns| {
                if let [left, right] = columns {
                    left.spacing_mut().item_spacing.x = spacing;
                    right.spacing_mut().item_spacing.x = spacing;
                    connection(app, left);
                    ui_what_to_run(app, right);
                }
            });
        });
    } else {
        connection(app, ui);
        ui.add_space(theme::GRID);
        ui_what_to_run(app, ui);
    }
    ui.add_space(4.0);
    ui.label(widgets::weak(
        ui,
        "More AI options (timeouts, prompts, report folders, extraction fields) are in All options → AI options.",
    ));
}

fn connection(app: &mut App, ui: &mut Ui) {
    subheading(ui, "Connection");
    {
        let mut edit = editing(app);
        rows(ui, &mut edit, CONNECTION);
    }
    model_row(app, ui);
    test_row(app, ui);
}

fn ui_what_to_run(app: &mut App, ui: &mut Ui) {
    subheading(ui, "What to run");
    {
        let mut edit = editing(app);
        rows(ui, &mut edit, WHAT_TO_RUN);
    }
    ui.add_space(theme::GRID);
    subheading(ui, "Limits");
    {
        let mut edit = editing(app);
        rows(ui, &mut edit, LIMITS);
    }
}

fn editing(app: &mut App) -> Editing<'_> {
    let App {
        schema,
        session,
        secrets,
        setup,
        dialogs,
        ..
    } = app;
    Editing {
        schema,
        config: &mut session.config,
        secrets,
        state: &mut setup.editor,
        dialogs: &**dialogs,
    }
}

fn subheading(ui: &mut Ui, text: &str) {
    ui.label(theme::semibold(text).size(13.0).color(theme::palette(ui).text_weak));
    ui.add_space(2.0);
}

fn rows(ui: &mut Ui, edit: &mut Editing<'_>, fields: &[Field]) {
    simple::rows(ui, edit, fields);
}

/// The model: an editable field with a list of the loaded models, and Load models.
fn model_row(app: &mut App, ui: &mut Ui) {
    let Some(spec) = app.schema.get(MODEL).cloned() else {
        return;
    };
    let tooltip = simple::option_tooltip(&spec);
    let changed = app.session.config.is_changed(&app.schema, MODEL);
    let crawler = app.crawler.path().map(std::path::Path::to_path_buf);
    let mut load = false;
    widgets::field_row(ui, "Model", &tooltip, changed, |ui| {
        // Room for the list button and Load models after the field.
        let field_width = (ui.available_width() - 184.0).clamp(110.0, CONTROL_WIDTH - 36.0);
        ui.horizontal(|ui| {
            let mut text = match app.session.config.get(&app.schema, MODEL) {
                Some(Value::Text(text)) => text,
                _ => String::new(),
            };
            let response = ui
                .add(
                    TextEdit::singleline(&mut text)
                        .desired_width(field_width)
                        .hint_text("model id, e.g. gpt-4.1-mini")
                        .margin(egui::vec2(8.0, 5.0)),
                )
                .accessible_name("Model")
                .on_hover_text(&tooltip);
            if response.changed() {
                let _ = app.session.config.set(&app.schema, MODEL, Value::Text(text.clone()));
            }
            let mut picked = None;
            let (menu, _) = egui::containers::menu::MenuButton::new(icons::ICON_ARROW_DROP_DOWN.rich_text().size(20.0))
                .ui(ui, |ui| {
                    ui.set_min_width(320.0);
                    match &app.setup.ai.models {
                        Some(Ok(list)) if !list.models.is_empty() => {
                            egui::ScrollArea::vertical().max_height(320.0).show(ui, |ui| {
                                for model in &list.models {
                                    let mut label = model.id.clone();
                                    if let Some(window) = model.context_window {
                                        label.push_str(&format!("   ·   {} tokens context", format::count(window)));
                                    }
                                    if ui.selectable_label(text == model.id, label).clicked() {
                                        picked = Some(model.id.clone());
                                    }
                                }
                            });
                        }
                        Some(Ok(_)) => {
                            ui.label("The endpoint offers no models.");
                        }
                        _ => {
                            ui.label("Use Load models to list the models of the endpoint.");
                        }
                    }
                });
            let _ = menu
                .accessible_name("Choose a loaded model")
                .on_hover_text("Choose a loaded model");
            if let Some(model) = picked {
                let _ = app.session.config.set(&app.schema, MODEL, Value::Text(model));
            }
            let loading = app.setup.ai.loading_models;
            let button = ui
                .add_enabled(crawler.is_some() && !loading, egui::Button::new("Load models"))
                .accessible_name("Load models")
                .on_hover_text("Ask the endpoint which models it offers (--ai-list-models)")
                .on_disabled_hover_text(if loading {
                    "Loading…"
                } else {
                    "Locate the crawler program first"
                });
            load = button.clicked();
            if loading {
                ui.spinner();
            }
        });
        match &app.setup.ai.models {
            Some(Ok(list)) => {
                ui.label(widgets::weak(
                    ui,
                    format!(
                        "{} available at {}",
                        format::plural(list.models.len() as u64, "model"),
                        list.endpoint
                    ),
                ));
            }
            Some(Err(error)) => widgets::inline_message(ui, Tone::Danger, error),
            None => {}
        }
    });
    ui.add_space(4.0);
    if load && let Some(binary) = crawler {
        let config_file = app.paths.empty_config_file();
        let (args, env) = run_plan::ai_tool_args(&app.run_config(), &app.schema, &app.secrets, &config_file);
        app.setup.ai.loading_models = true;
        app.spawn_task(move || {
            let result = tools::ai_list_models(&binary, &args, &env, MODELS_TIMEOUT);
            Box::new(move |app: &mut App| {
                app.setup.ai.loading_models = false;
                app.setup.ai.models = Some(result.map_err(|error| error.to_string()));
            })
        });
    }
}

/// Test connection: one prompt through `--ai-check`; reply, latency and speed, or the error.
fn test_row(app: &mut App, ui: &mut Ui) {
    let p = theme::palette(ui);
    let crawler = app.crawler.path().map(std::path::Path::to_path_buf);
    let mut test = false;
    widgets::field_row(
        ui,
        "",
        "Sends one short prompt with these settings (--ai-check)",
        false,
        |ui| {
            ui.horizontal(|ui| {
                let checking = app.setup.ai.checking;
                test = ui
                    .add_enabled(
                        crawler.is_some() && !checking,
                        egui::Button::new(widgets::with_icon(icons::ICON_BOLT, "Test connection")),
                    )
                    .accessible_name("Test connection")
                    .on_hover_text("Send one short prompt with these settings (--ai-check)")
                    .on_disabled_hover_text(if checking {
                        "Testing…"
                    } else {
                        "Locate the crawler program first"
                    })
                    .clicked();
                if checking {
                    ui.spinner();
                    ui.label(widgets::weak(ui, "Waiting for the model…"));
                }
            });
            match &app.setup.ai.check {
                Some(Ok((result, elapsed))) => {
                    let mut facts = vec![format::millis(result.ms.unwrap_or(elapsed.as_millis() as u64))];
                    if let Some(speed) = result.output_tokens_per_second {
                        facts.push(format!("{speed:.0} tokens/s"));
                    }
                    if let (Some(input), Some(output)) = (result.input_tokens, result.output_tokens) {
                        facts.push(format!("{input} in · {output} out"));
                    }
                    ui.horizontal_wrapped(|ui| {
                        ui.label(icons::ICON_CHECK_CIRCLE.rich_text().color(p.success));
                        ui.label(RichText::new(format!("{} answered", result.model)).color(p.success));
                        ui.label(widgets::weak(ui, facts.join(" · ")));
                    });
                    let reply: String = result.reply.chars().take(300).collect();
                    if !reply.trim().is_empty() {
                        ui.label(widgets::weak(ui, format!("Reply: “{}”", reply.trim())));
                    }
                }
                Some(Err(error)) => widgets::inline_message(ui, Tone::Danger, error),
                None => {}
            }
        },
    );
    if test && let Some(binary) = crawler {
        let config_file = app.paths.empty_config_file();
        let (args, env) = run_plan::ai_tool_args(&app.run_config(), &app.schema, &app.secrets, &config_file);
        app.setup.ai.checking = true;
        app.spawn_task(move || {
            let started = Instant::now();
            let result = tools::ai_check(&binary, &args, &env, CHECK_TIMEOUT);
            let elapsed = started.elapsed();
            Box::new(move |app: &mut App| {
                app.setup.ai.checking = false;
                app.setup.ai.check = Some(result.map(|check| (check, elapsed)).map_err(|error| error.to_string()));
            })
        });
    }
}
