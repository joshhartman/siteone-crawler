//! Snapshots of the Run and History pages at 1280×820 and 1000×640, light and dark (GUI-SPEC §8):
//! no run, a live crawl, stopping, a finished run (every tab), failures, and History (empty, with
//! runs, with a damaged run). See `ui_common` for why they are written and checked rather than
//! compared.

mod ui_common;

use eframe::egui;
use eframe::egui::accesskit::Role;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use siteone_crawler_gui::app::{App, Page};
use siteone_crawler_gui::model::history::RecordedOutcome;
use ui_common::runs;
use ui_common::{Env, LARGE, SMALL, harness, settle, snapshot};

const SIZES: [(&str, egui::Vec2); 2] = [("1280x820", LARGE), ("1000x640", SMALL)];
const THEMES: [(&str, egui::Theme); 2] = [("light", egui::Theme::Light), ("dark", egui::Theme::Dark)];

/// Renders the app `prepare` builds in every size and theme as `<name>-<size>-<theme>.png`;
/// `after` acts on the settled harness (e.g. opens a tab).
fn each_variant(name: &str, prepare: impl Fn(&Env, &mut App), after: impl Fn(&mut Harness<'_, App>)) {
    let _gpu = ui_common::gpu_lock();
    for (size_name, size) in SIZES {
        for (theme_name, theme) in THEMES {
            let env = Env::new();
            let mut app = env.app();
            prepare(&env, &mut app);
            let mut harness = harness(app, size, theme);
            settle(&mut harness);
            after(&mut harness);
            settle(&mut harness);
            snapshot(&mut harness, &format!("{name}-{size_name}-{theme_name}"));
        }
    }
}

fn tab(label: &'static str) -> impl Fn(&mut Harness<'_, App>) {
    move |harness| harness.get_by_role_and_label(Role::Tab, label).click()
}

fn keep(control: runs::Control) {
    // The scripted child stays attached as long as the app lives; the control is not needed.
    drop(control);
}

#[test]
fn run_without_a_run() {
    each_variant("run-empty", |_, app| app.page = Page::Run, |_| {});
}

#[test]
fn run_live_overview() {
    each_variant("run-live-overview", |_, app| keep(runs::live(app)), |_| {});
}

#[test]
fn run_live_urls() {
    each_variant("run-live-urls", |_, app| keep(runs::live(app)), tab("URLs"));
}

#[test]
fn run_live_broken_links() {
    each_variant("run-live-broken", |_, app| keep(runs::live(app)), tab("Link checker"));
}

#[test]
fn run_stopping() {
    each_variant(
        "run-stopping",
        |_, app| {
            keep(runs::live(app));
            app.stop_run();
        },
        |_| {},
    );
}

#[test]
fn run_finished_overview() {
    each_variant("run-finished-overview", |_, app| keep(runs::finished(app)), |_| {});
}

#[test]
fn run_finished_scores_expanded() {
    each_variant(
        "run-finished-deductions",
        |_, app| keep(runs::finished(app)),
        |harness| harness.state_mut().run_ui.expanded_score = Some("accessibility".to_string()),
    );
}

#[test]
fn run_finished_urls() {
    each_variant("run-finished-urls", |_, app| keep(runs::finished(app)), tab("URLs"));
}

#[test]
fn run_finished_broken_links() {
    each_variant("run-broken", |_, app| keep(runs::finished(app)), tab("Link checker"));
}

#[test]
fn run_finished_without_broken_links() {
    each_variant(
        "run-broken-none",
        |_, app| keep(runs::finished_clean(app)),
        tab("Link checker"),
    );
}

#[test]
fn run_external_check_running() {
    each_variant(
        "run-check-running",
        |_, app| keep(runs::finished(app)),
        |harness| {
            runs::with_check(harness.state_mut(), 31, true);
            harness.get_by_role_and_label(Role::Tab, "Link checker").click();
        },
    );
}

#[test]
fn run_external_check_done() {
    each_variant(
        "run-check-done",
        |_, app| keep(runs::finished(app)),
        |harness| {
            runs::with_check(harness.state_mut(), 88, false);
            harness.get_by_role_and_label(Role::Tab, "Link checker").click();
        },
    );
}

#[test]
fn run_external_links_all() {
    each_variant(
        "run-external-all",
        |_, app| keep(runs::finished(app)),
        |harness| {
            runs::with_check(harness.state_mut(), 88, false);
            harness.get_by_role_and_label(Role::Tab, "Link checker").click();
            harness.run();
            harness.get_by_role_and_label(Role::RadioButton, "All external").click();
        },
    );
}

#[test]
fn run_external_check_overview() {
    each_variant(
        "run-check-overview",
        |_, app| keep(runs::finished(app)),
        |harness| runs::with_check(harness.state_mut(), 88, false),
    );
}

#[test]
fn run_findings() {
    each_variant("run-findings", |_, app| keep(runs::finished(app)), tab("Findings"));
}

#[test]
fn run_tables() {
    each_variant(
        "run-tables",
        |_, app| keep(runs::finished(app)),
        |harness| {
            harness.get_by_role_and_label(Role::Tab, "Tables").click();
            harness.run();
            harness.state_mut().run_ui.tables.selected = Some("seo".to_string());
        },
    );
}

#[test]
fn run_ai() {
    each_variant("run-ai", |_, app| keep(runs::finished_ai(app)), tab("AI"));
}

#[test]
fn run_files() {
    each_variant("run-files", |_, app| keep(runs::finished(app)), tab("Files"));
}

#[test]
fn run_console() {
    each_variant("run-console", |_, app| keep(runs::finished(app)), tab("Console"));
}

#[test]
fn run_failed() {
    each_variant("run-failed", |_, app| keep(runs::failed(app)), |_| {});
}

#[test]
fn run_crashed() {
    each_variant("run-crashed", |_, app| keep(runs::crashed(app)), |_| {});
}

#[test]
fn run_no_pages() {
    each_variant("run-no-pages", |_, app| keep(runs::no_pages(app)), |_| {});
}

#[test]
fn run_config_error() {
    each_variant("run-config-error", |_, app| keep(runs::config_error(app)), |_| {});
}

#[test]
fn history_empty() {
    each_variant("history-empty", |_, app| app.page = Page::History, |_| {});
}

#[test]
fn history_with_runs() {
    each_variant(
        "history",
        |env, app| {
            runs::history(&env.runs_dir());
            app.page = Page::History;
            app.history_ui.selected = Some(env.runs_dir().join("20261117-163012-www.example.com"));
        },
        |_| {},
    );
}

#[test]
fn history_with_a_damaged_run() {
    each_variant(
        "history-damaged",
        |env, app| {
            runs::record(
                &env.runs_dir(),
                "20261118-090507-crawler.siteone.io",
                runs::SITE,
                Some(RecordedOutcome::Finished),
                Some(8.3),
                73,
            );
            let damaged = runs::damaged(&env.runs_dir());
            app.page = Page::History;
            app.history_ui.selected = Some(damaged);
        },
        |_| {},
    );
}

#[test]
fn run_opened_from_history() {
    each_variant(
        "run-past",
        |_, app| keep(runs::crashed(app)),
        |harness| {
            let app = harness.state_mut();
            let run_dir = app.current_run.as_ref().expect("run").plan.run_dir.clone();
            app.current_run = None;
            app.open_past_run(run_dir);
        },
    );
}

/// Three broken rows of the finished run queued and retried with a scripted pass (`script`).
fn retried(harness: &mut Harness<'_, App>) {
    let urls: Vec<String> = [31, 32, 33]
        .iter()
        .map(|version| format!("{}docs/api/v{version}/", runs::SITE))
        .chain([runs::FIXTURE_404.to_string()])
        .collect();
    let app = harness.state_mut();
    app.enqueue_retries(&urls);
    app.retry_queue();
}

#[test]
fn run_retry_selection() {
    each_variant(
        "run-retry-selection",
        |_, app| keep(runs::finished(app)),
        |harness| {
            harness.get_by_role_and_label(Role::Tab, "Link checker").click();
            harness.run();
            for version in [31, 32, 33] {
                let label = format!("Select {}docs/api/v{version}/", runs::SITE);
                harness.get_by_role_and_label(Role::CheckBox, &label).click();
                harness.run();
            }
        },
    );
}

/// The finished run with its external links checked, one broken link found on two pages, and the
/// Link checker laid out by page.
fn by_page(harness: &mut Harness<'_, App>) {
    runs::with_check(harness.state_mut(), 88, false);
    if let Some(link) = harness
        .state_mut()
        .current_run
        .as_mut()
        .and_then(|run| run.links.report.as_mut())
        .and_then(|report| report.results.iter_mut().find(|link| link.status == Some(404)))
    {
        link.sources.push(runs::FIXTURE_404_SOURCE.to_string());
    }
    harness.get_by_role_and_label(Role::Tab, "Link checker").click();
    harness.run();
    harness.get_by_role_and_label(Role::CheckBox, "By page").click();
    harness.run();
}

#[test]
fn run_broken_links_by_page() {
    each_variant("run-by-page", |_, app| keep(runs::finished(app)), by_page);
}

/// By page with a retry queue (its view adds to the views) and two rows selected: the fullest
/// toolbar.
#[test]
fn run_broken_links_by_page_with_a_selection() {
    each_variant(
        "run-by-page-selection",
        |_, app| keep(runs::finished(app)),
        |harness| {
            harness
                .state_mut()
                .enqueue_retries(&[format!("{}docs/api/v31/", runs::SITE)]);
            by_page(harness);
            let urls: Vec<String> = siteone_crawler_gui::ui::run::broken_pages(harness.state())
                .first()
                .map(|(_, rows)| rows.iter().take(2).map(|row| row.url.clone()).collect())
                .unwrap_or_default();
            for url in urls {
                if let Some(checkbox) = harness
                    .query_all_by_role_and_label(Role::CheckBox, &format!("Select {url}"))
                    .next()
                {
                    checkbox.click();
                }
                harness.run();
            }
        },
    );
}

#[test]
fn run_retry_running() {
    each_variant(
        "run-retry-running",
        |_, app| {
            keep(runs::finished(app));
            let _ = runs::scripted_passes(app, runs::PassScript::Hold);
        },
        |harness| {
            retried(harness);
            harness.get_by_role_and_label(Role::Tab, "Link checker").click();
            harness.run();
            harness.get_by_role_and_label(Role::RadioButton, "Retry queue").click();
        },
    );
}

/// The Link checker after a pass: the URLs that answer OK now are gone, the one still broken stays.
#[test]
fn run_retry_after_pass() {
    each_variant(
        "run-retry-after-pass",
        |_, app| {
            keep(runs::finished(app));
            let _ = runs::scripted_passes(app, runs::PassScript::AnswerBut(runs::FIXTURE_404));
        },
        |harness| {
            retried(harness);
            harness.get_by_role_and_label(Role::Tab, "Link checker").click();
        },
    );
}

#[test]
fn run_retry_overview() {
    each_variant(
        "run-retry-overview",
        |_, app| {
            keep(runs::finished(app));
            let _ = runs::scripted_passes(app, runs::PassScript::Answer);
        },
        retried,
    );
}

#[test]
fn run_retry_files() {
    each_variant(
        "run-retry-files",
        |_, app| {
            keep(runs::finished(app));
            let _ = runs::scripted_passes(app, runs::PassScript::Answer);
        },
        |harness| {
            retried(harness);
            harness.get_by_role_and_label(Role::Tab, "Files").click();
        },
    );
}

/// The Link checker with where each page references its broken link: a page (`:212`) and a
/// stylesheet (`:1929`), by link and by page.
#[test]
fn run_broken_links_with_positions() {
    each_variant(
        "run-broken-positions",
        |_, app| keep(runs::finished_with_positions(app)),
        tab("Link checker"),
    );
    each_variant(
        "run-by-page-positions",
        |_, app| keep(runs::finished_with_positions(app)),
        |harness| {
            harness.get_by_role_and_label(Role::Tab, "Link checker").click();
            harness.run();
            harness.get_by_role_and_label(Role::CheckBox, "By page").click();
        },
    );
}

/// A run of a crawler with link data (`--link-details`): Found on for every status with its line,
/// "+N more" for a link on many pages, the external links with all their pages — by link and by
/// page; the URLs tab with Depth and In links; the pages linking to a URL.
#[test]
fn run_with_link_data() {
    let checked = |harness: &mut Harness<'_, App>| {
        runs::with_check(harness.state_mut(), 88, false);
        harness.get_by_role_and_label(Role::Tab, "Link checker").click();
    };
    each_variant(
        "run-link-data-broken",
        |_, app| keep(runs::finished_with_link_data(app)),
        checked,
    );
    each_variant(
        "run-link-data-by-page",
        |_, app| keep(runs::finished_with_link_data(app)),
        |harness| {
            checked(harness);
            harness.run();
            harness.get_by_role_and_label(Role::CheckBox, "By page").click();
        },
    );
    each_variant(
        "run-link-data-urls",
        |_, app| keep(runs::finished_with_link_data(app)),
        tab("URLs"),
    );
    each_variant(
        "run-link-data-linking-pages",
        |_, app| keep(runs::finished_with_link_data(app)),
        |harness| {
            harness.get_by_role_and_label(Role::Tab, "URLs").click();
            harness.run();
            let app = harness.state_mut();
            let run = app.current_run.as_ref().expect("run");
            let graph = run.links.graph.clone().expect("links table");
            let pages = siteone_crawler_gui::ui::run::urls::linking_pages(runs::FIXTURE_404, Some(&graph), None, false)
                .expect("pages");
            siteone_crawler_gui::ui::run::show_linking_pages(app, &pages);
        },
    );
}
