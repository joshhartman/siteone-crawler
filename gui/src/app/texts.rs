//! Link texts as the app drives them (GUI-SPEC §4.8). Where the crawler reported the link text and
//! position of a reference (`--link-details`), nothing is fetched. The fallback: after a crawl, the
//! source pages of its broken URLs that the crawler gave no position for — a run without link
//! details, a page rendered with `--browser`, a run of an older crawler (its `404` table) — HTML
//! pages and stylesheets, are fetched (pages of the crawled site only) to read the text of the
//! broken link and where the page references it; once the external link check of a run has run,
//! so are the source pages of its external links (again only those without a crawler position).
//! Every page is fetched once per run: what it says about every target the run knows is kept in
//! `<run>/link-texts.json`.
//!
//! One fetch at a time ([`App::text_job`]). Like the link check it belongs to a run folder: it goes
//! on when another run is shown and writes its file when it ends (or when the app exits).
//!
//! The site's `--http-auth` is the run's own: the current run's copy (in memory), never Setup's,
//! which may have changed since or belong to another site. A run from History keeps no password
//! and no headers: when it used them, its source pages are not fetched (the reason names what is
//! missing), like a retry of its crawled URLs (§4.9).

use std::path::{Path, PathBuf};

use eframe::egui;

use super::lifecycle::{HTTP_AUTH, credentials_not_kept};
use super::{App, PastLoad};
use crate::backend::link_text::{self, SourceSettings, TextHandle, TextMsg};
use crate::model::config::CrawlConfig;
use crate::model::link_check::RunLinks;
use crate::model::link_text::{LinkTexts, MAX_SOURCE_PAGES, PageTexts};
use crate::model::results::ResultLoad;

/// At most this many page messages are applied per frame.
const MAX_TEXT_MESSAGES_PER_FRAME: usize = 100;

/// A running fetch of source pages.
pub struct TextJob {
    pub run_dir: PathBuf,
    handle: TextHandle,
    /// The run's texts at the start, with every page handled since.
    texts: LinkTexts,
}

/// What a fetch needs from a run.
struct Candidate {
    run_dir: PathBuf,
    pages: Vec<String>,
    targets: Vec<String>,
    texts: LinkTexts,
    config: CrawlConfig,
    site_url: String,
    /// The run's `--http-auth` value.
    http_auth: Option<String>,
    /// The request credentials the run used that are not known (a run from History).
    missing: Vec<String>,
}

impl App {
    /// The links of the run in `run_dir`, if it is the current run or the past run shown.
    fn texts_targets(&mut self, run_dir: &Path) -> Vec<&mut RunLinks> {
        let mut found = Vec::new();
        if let Some(run) = &mut self.current_run
            && run.plan.run_dir == run_dir
        {
            found.push(&mut run.links);
        }
        if let Some(past) = &mut self.past_run
            && past.run_dir == run_dir
            && let PastLoad::Ready(data) = &mut past.load
        {
            found.push(&mut data.links);
        }
        found
    }

    /// Whether source pages of the run in `run_dir` are being fetched.
    pub fn texts_pending(&self, run_dir: &Path) -> bool {
        self.text_job.as_ref().is_some_and(|job| job.run_dir == run_dir)
    }

    /// Starts a fetch when a shown run has source pages that were not handled: the current run's
    /// 404 sources once its result is loaded, and the external links' sources of a run whose
    /// external links were checked.
    pub(super) fn auto_link_texts(&mut self) {
        if !self.fetch_link_texts || self.text_job.is_some() {
            return;
        }
        let Some(candidate) = self.text_candidate() else {
            return;
        };
        let Candidate {
            run_dir,
            pages,
            targets,
            mut texts,
            config,
            site_url,
            http_auth,
            missing,
        } = candidate;
        let settings = SourceSettings::from_config(&config, &self.schema, &site_url, http_auth);
        let incomplete = (!missing.is_empty()).then(|| {
            format!(
                "Not fetched: this run's configuration lacks {}. Use Run again to enter it.",
                missing.join("; ")
            )
        });
        // Pages of other hosts, pages over the cap and every page of a run whose credentials are
        // not known are settled without a request. A stylesheet is a page like any other: it is
        // fetched to locate its `url()` references, and counts within the cap.
        let mut fetch = Vec::new();
        let mut fetched_or_wanted = texts.pages.values().filter(|page| page.is_read()).count();
        for page in pages {
            let settled = if !settings.is_site_page(&page) {
                Some(PageTexts::NotFetched {
                    reason: "Only pages of the crawled site are fetched.".to_string(),
                })
            } else if let Some(reason) = &incomplete {
                Some(PageTexts::NotFetched { reason: reason.clone() })
            } else if fetched_or_wanted >= MAX_SOURCE_PAGES {
                texts.cut += 1;
                Some(PageTexts::NotFetched {
                    reason: format!("Over the limit of {MAX_SOURCE_PAGES} source pages per run."),
                })
            } else {
                fetched_or_wanted += 1;
                None
            };
            match settled {
                Some(state) => {
                    texts.pages.insert(page, state);
                }
                None => fetch.push(page),
            }
        }
        for links in self.texts_targets(&run_dir) {
            links.texts = texts.clone();
        }
        if fetch.is_empty() {
            self.save_texts(run_dir, texts);
            return;
        }
        let waker = self.waker.clone();
        match link_text::start(fetch, targets, settings, move || waker.wake()) {
            Ok(handle) => self.text_job = Some(TextJob { run_dir, handle, texts }),
            Err(error) => {
                self.toasts
                    .warning(format!("The link texts could not be read: {error}"));
                for links in self.texts_targets(&run_dir) {
                    links.texts_settled = true;
                }
            }
        }
    }

    /// The first shown run with source pages to fetch (marking runs without any as settled).
    fn text_candidate(&mut self) -> Option<Candidate> {
        if let Some(run) = &mut self.current_run
            && run.finalized
            && !run.links.texts_settled
            && matches!(run.result.ready(), Some(ResultLoad::Loaded(_)))
        {
            let crawled = run.links.crawled_pairs(&run.state.urls);
            let pages = run.links.wanted_pages(&crawled);
            if pages.is_empty() {
                run.links.texts_settled = true;
            } else {
                return Some(Candidate {
                    run_dir: run.plan.run_dir.clone(),
                    pages,
                    targets: run.links.text_targets(&crawled),
                    texts: run.links.texts.clone(),
                    config: run.config.clone(),
                    site_url: run
                        .state
                        .started
                        .as_ref()
                        .map(|started| started.url.clone())
                        .filter(|url| !url.is_empty())
                        .unwrap_or_else(|| run.config.url.clone()),
                    http_auth: run.secrets.get(HTTP_AUTH).map(str::to_string),
                    missing: Vec::new(),
                });
            }
        }
        if let Some(past) = &mut self.past_run
            && let PastLoad::Ready(data) = &mut past.load
            && !data.links.texts_settled
        {
            // A past run fetches only when asked (its external links were checked now).
            let crawled = if data.links.texts_external {
                data.links.crawled_pairs(&data.state.urls)
            } else {
                Vec::new()
            };
            let pages = if data.links.texts_external {
                data.links.wanted_pages(&crawled)
            } else {
                Vec::new()
            };
            if pages.is_empty() {
                data.links.texts_settled = true;
            } else {
                return Some(Candidate {
                    run_dir: past.run_dir.clone(),
                    pages,
                    targets: data.links.text_targets(&crawled),
                    texts: data.links.texts.clone(),
                    config: data.record.config.clone(),
                    site_url: data.record.url.clone(),
                    http_auth: None,
                    missing: credentials_not_kept(&data.record),
                });
            }
        }
        None
    }

    /// Applies the pages of the running fetch (bounded per frame); saves when it ends.
    pub(super) fn pump_text_job(&mut self, ctx: &egui::Context) {
        let Some(job) = &mut self.text_job else {
            return;
        };
        let mut pages = Vec::new();
        let mut finished = false;
        while pages.len() < MAX_TEXT_MESSAGES_PER_FRAME {
            match job.handle.try_next() {
                Some(TextMsg::Page(page, texts)) => {
                    job.texts.pages.insert(page.clone(), texts.clone());
                    pages.push((page, texts));
                }
                Some(TextMsg::Finished) => {
                    finished = true;
                    break;
                }
                None => break,
            }
        }
        if pages.len() == MAX_TEXT_MESSAGES_PER_FRAME {
            ctx.request_repaint();
        }
        let run_dir = job.run_dir.clone();
        for links in self.texts_targets(&run_dir) {
            for (page, texts) in &pages {
                links.texts.pages.insert(page.clone(), texts.clone());
            }
        }
        if finished && let Some(job) = self.text_job.take() {
            for links in self.texts_targets(&run_dir) {
                // Wanted pages may have grown meanwhile (the external check started).
                links.texts_settled = false;
            }
            self.save_texts(job.run_dir, job.texts);
            self.write_link_report(&run_dir);
        }
    }

    fn save_texts(&mut self, run_dir: PathBuf, texts: LinkTexts) {
        self.spawn_task(move || {
            let failed = texts.write(&run_dir).err().filter(|_| run_dir.is_dir());
            Box::new(move |app: &mut App| {
                if let Some(error) = failed {
                    app.toasts
                        .warning(format!("The link texts could not be saved in the run folder: {error}"));
                }
            })
        });
    }

    /// The external links of the run in `run_dir` are being checked: their source pages are
    /// wanted too.
    pub(super) fn want_external_texts(&mut self, run_dir: &Path) {
        for links in self.texts_targets(run_dir) {
            links.texts_external = true;
            links.texts_settled = false;
        }
    }

    /// On exit: stops the fetch and saves what it read, synchronously.
    pub fn end_text_job_for_exit(&mut self) {
        if let Some(job) = self.text_job.take() {
            job.handle.cancel();
            let mut texts = job.texts;
            while let Some(TextMsg::Page(page, page_texts)) = job.handle.try_next() {
                texts.pages.insert(page, page_texts);
            }
            let _ = texts.write(&job.run_dir);
        }
    }

    /// The run in `run_dir` is being deleted: its fetch stops, nothing is written.
    pub fn discard_text_job(&mut self, run_dir: &Path) {
        if self.text_job.as_ref().is_some_and(|job| job.run_dir == run_dir)
            && let Some(job) = self.text_job.take()
        {
            job.handle.cancel();
        }
    }
}
