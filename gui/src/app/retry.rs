//! The retry queue of a run as the app drives it (GUI-SPEC §4.9, §6.3).
//!
//! ```text
//! Add to queue / Remove / Clear queue ──► <run>/retries.json
//! Retry now (rows) / Retry queue ──► (no crawl, no check of this run, no other pass) start
//! start ──► Retrying n of m ──(answer)──► newest answer in the run's rows / the check's entry,
//!   │                                      out of the queue, retries.json (and external-links.json)
//!   │                └──(no answer)──► the old answer stays, queued, the reason shown
//!   ├──(every URL handled)──► Retried n: k now OK … ; link report, History record numbers
//!   ├──Stop / a crawl starts──► the URL in flight and the rest stay queued
//!   ├──app exit──► stopped, retries.json written synchronously
//!   └──run deleted from History──► dropped (nothing written)
//! ```
//!
//! One pass at a time ([`App::retry`]). Like the link check it belongs to a run folder: the results
//! land in the run whose folder it is (the current run or the one opened from History).

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use eframe::egui;

use super::{App, PastLoad, ViewedRun};
use crate::backend::link_check::CheckSettings;
use crate::backend::retry::{self, PassSettings, RetryHandle, RetryItem, RetryMsg, RetryTarget, WATCHDOG_EXTRA};
use crate::model::config::CrawlConfig;
use crate::model::history::RunRecord;
use crate::model::link_check::{CheckReport, ExternalLink, LinkResult, RunLinks};
use crate::model::retry::{self as model, Answer, CrawledAnswer, PassSummary, RetryState};
use crate::model::run_state::RunState;

/// At most this many pass messages are applied per frame.
const MAX_RETRY_MESSAGES_PER_FRAME: usize = 100;

/// A running pass, as the app drives it. [`RetryHandle`] is the real one; UI tests drive a
/// scripted one through the same seam ([`App::retry_spawner`]).
pub trait RetryControl {
    /// The next message, without blocking.
    fn try_next(&mut self) -> Option<RetryMsg>;
    /// Ends the pass (the request in flight is killed; nothing more is sent).
    fn cancel(&mut self);
}

impl RetryControl for RetryHandle {
    fn try_next(&mut self) -> Option<RetryMsg> {
        RetryHandle::try_next(self)
    }

    fn cancel(&mut self) {
        RetryHandle::cancel(self)
    }
}

/// Starts the backend of a pass in place of [`retry::start`].
pub type RetrySpawner = Box<dyn Fn(Vec<RetryItem>, PassSettings) -> Box<dyn RetryControl>>;

/// The pass that is running.
pub struct ActiveRetry {
    /// The folder of the run whose URLs are retried.
    pub run_dir: PathBuf,
    pub handle: Box<dyn RetryControl>,
    /// The URLs of the pass, in order.
    pub urls: Vec<String>,
    /// URLs handled so far.
    pub done: usize,
    /// The URL being requested.
    pub current: Option<String>,
    summary: PassSummary,
}

/// Orders the writes of files from background tasks: each write has a generation, and a write
/// older than the last one made to the same file is skipped.
#[derive(Clone, Default)]
pub(crate) struct LatestWrites {
    written: Arc<Mutex<HashMap<PathBuf, u64>>>,
    generation: u64,
}

/// The parts of a shown run a retry changes.
struct RunParts<'a> {
    state: &'a mut RunState,
    links: &'a mut RunLinks,
    retries: &'a mut RetryState,
    record: &'a mut RunRecord,
}

/// What a pass needs from a run.
struct PassSource {
    run_argv: Vec<String>,
    site_url: String,
    config: CrawlConfig,
    /// What the run's configuration lacks (a run from History).
    missing: Vec<String>,
}

impl App {
    /// The run of `run_dir`, if it is the current run or the past run shown.
    fn run_parts(&mut self, run_dir: &Path) -> Option<RunParts<'_>> {
        if let Some(run) = &mut self.current_run
            && run.plan.run_dir == run_dir
        {
            return Some(RunParts {
                state: &mut run.state,
                links: &mut run.links,
                retries: &mut run.retries,
                record: &mut run.record,
            });
        }
        if let Some(past) = &mut self.past_run
            && past.run_dir == run_dir
            && let PastLoad::Ready(data) = &mut past.load
        {
            return Some(RunParts {
                state: &mut data.state,
                links: &mut data.links,
                retries: &mut data.retries,
                record: &mut data.record,
            });
        }
        None
    }

    /// The folder of the run on the Run page (a finished one).
    fn viewed_run_dir(&self) -> Option<PathBuf> {
        match &self.viewed_run {
            ViewedRun::Current => self
                .current_run
                .as_ref()
                .filter(|run| !run.is_active())
                .map(|run| run.plan.run_dir.clone()),
            ViewedRun::Past { .. } => self.past_run.as_ref().and_then(|past| match past.load {
                PastLoad::Ready(_) => Some(past.run_dir.clone()),
                _ => None,
            }),
        }
    }

    /// Why no pass can start for the run in `run_dir` now (the tooltip of a disabled button).
    pub fn retry_blocker(&self, run_dir: &Path) -> Option<&'static str> {
        if self.has_active_run() {
            Some("A crawl is running; retry when it has finished")
        } else if self.link_check.as_ref().is_some_and(|check| check.run_dir == run_dir) {
            Some("The external links of this run are being checked")
        } else if self.retry.is_some() {
            Some("A retry is running")
        } else {
            None
        }
    }

    /// What the configuration of the run in `run_dir` lacks for retrying crawled URLs (a run from
    /// History that used a password or headers, which History does not keep); `None` when it is
    /// complete.
    pub fn retry_missing(&self, run_dir: &Path) -> Option<String> {
        if self.current_run.as_ref().is_some_and(|run| run.plan.run_dir == run_dir) {
            return None;
        }
        let past = self.past_run.as_ref().filter(|past| past.run_dir == run_dir)?;
        let PastLoad::Ready(data) = &past.load else {
            return None;
        };
        let missing = super::lifecycle::credentials_not_kept(&data.record);
        (!missing.is_empty()).then(|| missing.join("; "))
    }

    fn pass_source(&self, run_dir: &Path) -> Option<PassSource> {
        if let Some(run) = &self.current_run
            && run.plan.run_dir == run_dir
        {
            return Some(PassSource {
                run_argv: run.plan.argv.clone(),
                site_url: super::lifecycle::site_url(&run.state, &run.config.url),
                config: run.config.clone(),
                missing: Vec::new(),
            });
        }
        let past = self.past_run.as_ref().filter(|past| past.run_dir == run_dir)?;
        let PastLoad::Ready(data) = &past.load else {
            return None;
        };
        let record = &data.record;
        // The record masks secrets and leaves out headers; no value of this session takes their
        // place. A run that used them retries no crawled URL (`missing`); a masked option that is
        // no request option never reaches a child.
        let missing = super::lifecycle::credentials_not_kept(record);
        Some(PassSource {
            run_argv: record.argv.clone(),
            site_url: super::lifecycle::site_url(&data.state, &record.url),
            config: record.config.clone(),
            missing,
        })
    }

    /// The answer `url` has now in the run, if the Link checker lists it (a crawled row or a checked
    /// link). `rows` maps the run's URLs to their rows.
    fn answer_now(parts: &RunParts<'_>, rows: &HashMap<&str, usize>, url: &str) -> Option<Answer> {
        if let Some(entry) = parts.retries.entry(url) {
            return Some(entry.current().clone());
        }
        if let Some(row) = rows.get(url).and_then(|index| parts.state.urls.get(*index)) {
            return Some(Answer::Crawled(CrawledAnswer::of_row(row)));
        }
        parts
            .links
            .report
            .as_ref()
            .and_then(|report| report.results.iter().find(|link| link.url == url))
            .map(|link| Answer::External(link.clone()))
    }

    /// Gives every URL an entry (with its answer now); the closure then changes the state.
    /// Returns the URLs that have one.
    fn with_entries(
        &mut self,
        run_dir: &Path,
        urls: &[String],
        change: impl FnOnce(&mut RetryState, &[String]),
    ) -> Vec<String> {
        let Some(parts) = self.run_parts(run_dir) else {
            return Vec::new();
        };
        let rows: HashMap<&str, usize> = parts
            .state
            .urls
            .iter()
            .enumerate()
            .map(|(index, row)| (&*row.url, index))
            .collect();
        let mut known = Vec::with_capacity(urls.len());
        let mut firsts = Vec::with_capacity(urls.len());
        for url in urls {
            if let Some(answer) = Self::answer_now(&parts, &rows, url) {
                known.push(url.clone());
                firsts.push(answer);
            }
        }
        drop(rows);
        for (url, first) in known.iter().zip(firsts) {
            parts.retries.prepare(url, first);
        }
        change(&mut *parts.retries, &known);
        let state = parts.retries.clone();
        self.save_retries(run_dir.to_path_buf(), state);
        known
    }

    /// *Add to retry queue* for URLs of the shown run.
    pub fn enqueue_retries(&mut self, urls: &[String]) {
        let Some(run_dir) = self.viewed_run_dir() else {
            return;
        };
        let added = std::cell::Cell::new(0usize);
        self.with_entries(&run_dir, urls, |retries, known| {
            for url in known {
                if let Some(entry) = retries.entry(url).cloned()
                    && retries.enqueue(url, entry.first)
                {
                    added.set(added.get() + 1);
                }
            }
        });
        let added = added.get();
        if added > 0 && self.retry.is_some() {
            self.toasts.info(format!(
                "{} added to the retry queue; {} for the next pass.",
                crate::ui::widgets::format::plural(added as u64, "URL"),
                if added == 1 { "it waits" } else { "they wait" }
            ));
        }
    }

    /// Removes URLs of the shown run from its queue.
    pub fn dequeue_retries(&mut self, urls: &[String]) {
        let Some(run_dir) = self.viewed_run_dir() else {
            return;
        };
        if let Some(parts) = self.run_parts(&run_dir) {
            parts.retries.dequeue(urls.iter().map(String::as_str));
            let state = parts.retries.clone();
            self.save_retries(run_dir, state);
        }
    }

    /// *Clear queue* of the shown run.
    pub fn clear_retry_queue(&mut self) {
        let Some(run_dir) = self.viewed_run_dir() else {
            return;
        };
        if let Some(parts) = self.run_parts(&run_dir) {
            parts.retries.clear_queue();
            let state = parts.retries.clone();
            self.save_retries(run_dir, state);
        }
    }

    /// *Retry now* for URLs of the shown run.
    pub fn retry_now(&mut self, urls: &[String]) {
        let Some(run_dir) = self.viewed_run_dir() else {
            return;
        };
        if let Some(reason) = self.retry_blocker(&run_dir) {
            self.toasts.warning(format!("{reason}."));
            return;
        }
        let known = self.with_entries(&run_dir, urls, |_, _| {});
        self.start_pass(run_dir, known);
    }

    /// *Retry queue* of the shown run.
    pub fn retry_queue(&mut self) {
        let Some(run_dir) = self.viewed_run_dir() else {
            return;
        };
        if let Some(reason) = self.retry_blocker(&run_dir) {
            self.toasts.warning(format!("{reason}."));
            return;
        }
        let queue = self
            .run_parts(&run_dir)
            .map(|parts| parts.retries.queue.clone())
            .unwrap_or_default();
        self.start_pass(run_dir, queue);
    }

    /// Starts a pass over `urls` (each has an entry) of the run in `run_dir`.
    fn start_pass(&mut self, run_dir: PathBuf, urls: Vec<String>) {
        if urls.is_empty() || self.retry.is_some() {
            return;
        }
        let Some(source) = self.pass_source(&run_dir) else {
            return;
        };
        let missing = (!source.missing.is_empty()).then(|| {
            format!(
                "Not retried: this run's configuration lacks {}. Use Run again to enter it.",
                source.missing.join("; ")
            )
        });
        let Some(parts) = self.run_parts(&run_dir) else {
            return;
        };
        let items: Vec<RetryItem> = urls
            .iter()
            .filter_map(|url| {
                let entry = parts.retries.entry(url)?;
                let target = match &entry.first {
                    Answer::Crawled(_) => match &missing {
                        Some(reason) => RetryTarget::Refused(reason.clone()),
                        None => RetryTarget::Crawled {
                            options: model::request_options(&source.run_argv, &source.site_url, url),
                        },
                    },
                    Answer::External(link) => RetryTarget::External(ExternalLink {
                        url: link.url.clone(),
                        sources: link.sources.clone(),
                        count: link.count,
                    }),
                };
                Some(RetryItem {
                    url: url.clone(),
                    target,
                })
            })
            .collect();
        parts.retries.last_pass = None;
        parts.retries.revision += 1;
        let urls: Vec<String> = items.iter().map(|item| item.url.clone()).collect();
        let settings = PassSettings {
            binary: self.crawler.path().map(Path::to_path_buf),
            run_dir: run_dir.clone(),
            config_file: self.paths.empty_config_file(),
            watchdog: Duration::from_secs(model::request_timeout(&source.run_argv)) + WATCHDOG_EXTRA,
            host_gap: model::host_gap(&source.run_argv),
            check: CheckSettings::from_config(&source.config, &self.schema),
        };
        let handle: Box<dyn RetryControl> = match &self.retry_spawner {
            Some(spawn) => spawn(items, settings),
            None => {
                let waker = self.waker.clone();
                match retry::start(items, settings, move || waker.wake()) {
                    Ok(handle) => Box::new(handle),
                    Err(error) => {
                        self.toasts.error(format!("The retry could not be started: {error}."));
                        return;
                    }
                }
            }
        };
        self.retry = Some(ActiveRetry {
            run_dir,
            handle,
            summary: PassSummary {
                total: urls.len(),
                ..PassSummary::default()
            },
            urls,
            done: 0,
            current: None,
        });
    }

    /// Applies the messages of the running pass (bounded per frame) and finishes it at its end.
    pub(super) fn pump_retry(&mut self, ctx: &egui::Context) {
        let mut applied = 0;
        let mut finished = false;
        while applied < MAX_RETRY_MESSAGES_PER_FRAME {
            let Some(active) = &mut self.retry else {
                return;
            };
            let Some(message) = active.handle.try_next() else {
                break;
            };
            applied += 1;
            let run_dir = active.run_dir.clone();
            match message {
                RetryMsg::Started(url) => active.current = Some(url),
                RetryMsg::Answered { url, answer } => {
                    active.done += 1;
                    active.current = None;
                    if answer.is_ok() {
                        active.summary.ok += 1;
                    } else {
                        active.summary.failing += 1;
                    }
                    self.apply_answer(&run_dir, &url, answer);
                }
                RetryMsg::Failed { url, reason } => {
                    active.done += 1;
                    active.current = None;
                    active.summary.not_retried += 1;
                    active.summary.add_reason(&reason);
                    if let Some(parts) = self.run_parts(&run_dir) {
                        parts.retries.record_failure(&url, &reason);
                        let state = parts.retries.clone();
                        self.save_retries(run_dir, state);
                    }
                }
                RetryMsg::Finished => {
                    finished = true;
                    break;
                }
            }
        }
        if applied == MAX_RETRY_MESSAGES_PER_FRAME {
            ctx.request_repaint();
        }
        if finished {
            self.finish_pass(false);
        }
    }

    /// A retry answered: the newest answer goes into the run's views and files.
    fn apply_answer(&mut self, run_dir: &Path, url: &str, answer: Answer) {
        let Some(parts) = self.run_parts(run_dir) else {
            return;
        };
        let mut report: Option<CheckReport> = None;
        match &answer {
            Answer::Crawled(crawled) => {
                if let Some(index) = parts.state.urls.iter().position(|row| &*row.url == url) {
                    parts.state.replace_answer(
                        index,
                        i64::from(crawled.status),
                        &crawled.content_type,
                        crawled.time_ms,
                        crawled.size,
                        crawled.cached,
                    );
                }
            }
            Answer::External(link) => {
                if let Some(check) = &mut parts.links.report
                    && let Some(entry) = check.results.iter_mut().find(|entry| entry.url == url)
                {
                    *entry = LinkResult {
                        sources: entry.sources.clone(),
                        count: entry.count,
                        ..link.clone()
                    };
                    report = Some(check.clone());
                }
            }
        }
        parts.retries.record_answer(url, answer);
        let state = parts.retries.clone();
        self.save_retries(run_dir.to_path_buf(), state);
        if let Some(report) = report {
            let dir = run_dir.to_path_buf();
            self.write_latest(CheckReport::path(run_dir), "The external link check", move || {
                report.write(&dir)
            });
        }
    }

    /// Ends the running pass. With `stopped`, the URL in flight and the rest stay (or become)
    /// queued.
    fn finish_pass(&mut self, stopped: bool) {
        let Some(mut active) = self.retry.take() else {
            return;
        };
        active.handle.cancel();
        let run_dir = active.run_dir.clone();
        let mut summary = active.summary;
        summary.stopped = stopped;
        let Some(parts) = self.run_parts(&run_dir) else {
            return;
        };
        if stopped {
            for url in active.urls.iter().skip(active.done) {
                if let Some(entry) = parts.retries.entry(url).cloned() {
                    parts.retries.enqueue(url, entry.first);
                }
            }
        }
        parts.retries.last_pass = Some(summary);
        parts.retries.revision += 1;
        parts.record.summary.errors = parts.state.status_counts.errors();
        parts.record.summary.total_bytes = parts.state.total_bytes;
        let record = parts.record.clone();
        let state = parts.retries.clone();
        self.save_retries(run_dir.clone(), state);
        let dir = run_dir.clone();
        self.write_latest(
            dir.join(crate::model::history::RUN_FILE),
            "The History record",
            move || record.write(&dir),
        );
        self.history_ui.stale = true;
        self.write_link_report(&run_dir);
    }

    /// *Stop* of the queue bar (and a crawl that starts): the pass ends; what was not retried stays
    /// queued.
    pub fn stop_retry(&mut self) {
        self.finish_pass(true);
    }

    /// On exit: ends the pass and writes the queue synchronously (the child dies with the app's Job
    /// Object).
    pub fn end_retry_for_exit(&mut self) {
        let Some(mut active) = self.retry.take() else {
            return;
        };
        active.handle.cancel();
        if let Some(parts) = self.run_parts(&active.run_dir) {
            for url in active.urls.iter().skip(active.done) {
                if let Some(entry) = parts.retries.entry(url).cloned() {
                    parts.retries.enqueue(url, entry.first);
                }
            }
            let _ = parts.retries.write(&active.run_dir);
        }
    }

    /// The run in `run_dir` is being deleted: its pass stops, nothing is written.
    pub fn discard_retry(&mut self, run_dir: &Path) {
        if self.retry.as_ref().is_some_and(|active| active.run_dir == run_dir)
            && let Some(mut active) = self.retry.take()
        {
            active.handle.cancel();
        }
    }

    fn save_retries(&mut self, run_dir: PathBuf, state: RetryState) {
        self.write_latest(RetryState::path(&run_dir), "The retry queue", move || {
            state.write(&run_dir)
        });
    }

    /// Writes a file of a run on a worker, after every earlier write to it (an older write that
    /// comes late is skipped). A run deleted meanwhile is not created again and not warned about.
    pub(super) fn write_latest<W>(&mut self, path: PathBuf, what: &'static str, write: W)
    where
        W: FnOnce() -> io::Result<()> + Send + 'static,
    {
        self.latest_writes.generation += 1;
        let generation = self.latest_writes.generation;
        let written = Arc::clone(&self.latest_writes.written);
        self.spawn_task(move || {
            let result = {
                let mut written = written.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
                if written.get(&path).is_some_and(|last| *last > generation) {
                    Ok(())
                } else {
                    written.insert(path.clone(), generation);
                    write()
                }
            };
            let failed = result.err().filter(|_| path.parent().is_some_and(Path::is_dir));
            Box::new(move |app: &mut App| {
                if let Some(error) = failed {
                    app.toasts
                        .warning(format!("{what} could not be saved in the run folder: {error}"));
                }
            })
        });
    }
}
