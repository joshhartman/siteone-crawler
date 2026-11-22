//! The external link check of a run as the app drives it (GUI-SPEC §4.7, §6.3).
//!
//! ```text
//! crawl ended, result JSON loaded, run.config.check_external_links ──► (no other check) start
//! Check external links now (any finished run, current or from History) ──────────────────► start
//! start ──► Running (n of m) ──(all checked)──► Done: <run>/external-links.json (complete)
//!                │      └──(the check cannot run: e.g. an invalid proxy)──► Failed (nothing written)
//!                ├──Stop check──► Stopped: the file with what was checked (complete: false)
//!                ├──app exit──► Stopped, written synchronously
//!                └──run deleted from History──► dropped (nothing written)
//! ```
//!
//! One check at a time ([`App::link_check`]). It belongs to a run folder, not to a page: the user
//! may look at other runs while it runs, and the results land in the run whose folder it is. The
//! crawl's outcome and History record never wait for it.

use std::path::{Path, PathBuf};

use eframe::egui;

use super::{App, ViewedRun};
use crate::backend::link_check::{self, CheckHandle, CheckMsg, CheckSettings};
use crate::model::config::CrawlConfig;
use crate::model::link_check::{CheckReport, LinkList, LinkResult, RunLinks};
use crate::model::results::ResultLoad;

/// At most this many check messages are applied per frame.
pub const MAX_CHECK_MESSAGES_PER_FRAME: usize = 500;

/// A running check, as the app drives it. [`CheckHandle`] is the real one; UI tests drive a
/// scripted one through the same seam.
pub trait CheckControl {
    /// The next message, without blocking.
    fn try_next(&mut self) -> Option<CheckMsg>;
    /// Stops the check (no new request starts; nothing more is sent).
    fn cancel(&mut self);
}

impl CheckControl for CheckHandle {
    fn try_next(&mut self) -> Option<CheckMsg> {
        CheckHandle::try_next(self)
    }

    fn cancel(&mut self) {
        CheckHandle::cancel(self)
    }
}

/// The check that is running.
pub struct ActiveCheck {
    /// The folder of the run whose links are checked.
    pub run_dir: PathBuf,
    pub handle: Box<dyn CheckControl>,
    /// Links to check.
    pub total: usize,
    pub files: usize,
    pub truncated: usize,
    /// Checked so far, in the order they finished.
    pub results: Vec<LinkResult>,
    pub started_at: chrono::DateTime<chrono::Local>,
    /// The check cannot run (from [`CheckMsg::Failed`]).
    failed: Option<String>,
}

impl ActiveCheck {
    fn report(&self, complete: bool) -> CheckReport {
        CheckReport {
            complete: complete && self.results.len() >= self.total,
            checked_at: self.started_at.to_rfc3339(),
            total: self.total,
            files_not_checked: self.files,
            truncated: self.truncated,
            results: self.results.clone(),
        }
    }
}

impl App {
    /// The links of the run in `run_dir`, if that run is the current one or the past one shown.
    fn run_links_mut(&mut self, run_dir: &Path) -> Vec<&mut RunLinks> {
        let mut found = Vec::new();
        if let Some(run) = &mut self.current_run
            && run.plan.run_dir == run_dir
        {
            found.push(&mut run.links);
        }
        if let Some(past) = &mut self.past_run
            && past.run_dir == run_dir
            && let super::PastLoad::Ready(data) = &mut past.load
        {
            found.push(&mut data.links);
        }
        found
    }

    /// Why no check can start now (the tooltip of a disabled *Check external links now*).
    pub fn link_check_blocker(&self) -> Option<&'static str> {
        if self.has_active_run() {
            Some("A crawl is running; check the links when it has finished")
        } else if self.link_check.is_some() {
            Some("Another external link check is running")
        } else {
            None
        }
    }

    /// Starts checking `list` for the run in `run_dir`, with the network settings of `config`.
    pub fn start_link_check(&mut self, run_dir: PathBuf, list: &LinkList, config: &CrawlConfig) {
        if self.link_check.is_some() {
            return;
        }
        let settings = CheckSettings::from_config(config, &self.schema);
        let waker = self.waker.clone();
        match link_check::start(list.links.clone(), settings, move || waker.wake()) {
            Ok(handle) => self.attach_link_check(run_dir, Box::new(handle), list),
            Err(error) => {
                let message = format!("The external link check could not be started: {error}.");
                for links in self.run_links_mut(&run_dir) {
                    links.error = Some(message.clone());
                }
                self.toasts.error(message);
            }
        }
    }

    /// Takes over a started check of the run in `run_dir`. `start_link_check` calls it; tests
    /// attach a scripted check.
    pub fn attach_link_check(&mut self, run_dir: PathBuf, handle: Box<dyn CheckControl>, list: &LinkList) {
        for links in self.run_links_mut(&run_dir) {
            links.error = None;
        }
        if let Some(run) = &mut self.current_run
            && run.plan.run_dir == run_dir
        {
            run.auto_check = true;
        }
        self.want_external_texts(&run_dir);
        self.link_check = Some(ActiveCheck {
            run_dir,
            handle,
            total: list.links.len(),
            files: list.files,
            truncated: list.truncated,
            results: Vec::with_capacity(list.links.len()),
            started_at: chrono::Local::now(),
            failed: None,
        });
    }

    /// Applies the messages of the running check (bounded per frame) and finishes it when every
    /// worker has ended.
    pub(super) fn pump_link_check(&mut self, ctx: &egui::Context) {
        let Some(active) = &mut self.link_check else {
            return;
        };
        let mut finished = false;
        let mut applied = 0;
        while applied < MAX_CHECK_MESSAGES_PER_FRAME {
            let Some(message) = active.handle.try_next() else {
                break;
            };
            applied += 1;
            match message {
                CheckMsg::Checked(result) => {
                    if active.results.len() < active.total {
                        active.results.push(result);
                    }
                }
                CheckMsg::Failed(error) => active.failed = Some(error),
                CheckMsg::Finished => {
                    finished = true;
                    break;
                }
            }
        }
        if applied == MAX_CHECK_MESSAGES_PER_FRAME {
            ctx.request_repaint();
        }
        if finished {
            self.finish_link_check(true);
        }
    }

    /// Ends the running check: its report goes to the run (if shown) and to the run folder.
    fn finish_link_check(&mut self, complete: bool) {
        let Some(active) = self.link_check.take() else {
            return;
        };
        let run_dir = active.run_dir.clone();
        if let Some(error) = &active.failed {
            let message = format!("The external links could not be checked: {error}");
            for links in self.run_links_mut(&run_dir) {
                links.error = Some(message.clone());
            }
            self.toasts.error(message);
            return;
        }
        let report = active.report(complete);
        for links in self.run_links_mut(&run_dir) {
            links.report = Some(report.clone());
            links.error = None;
        }
        self.write_link_report(&run_dir);
        self.spawn_task(move || {
            let result = report.write(&run_dir);
            // A run deleted in the meantime is gone on purpose: no warning.
            let failed = result.err().filter(|_| run_dir.is_dir());
            Box::new(move |app: &mut App| {
                if let Some(error) = failed {
                    app.toasts.warning(format!(
                        "The external link check could not be saved in the run folder: {error}"
                    ));
                }
            })
        });
    }

    /// *Stop check*: no new request starts; what was checked is kept, marked as stopped.
    pub fn stop_link_check(&mut self) {
        if let Some(active) = &mut self.link_check {
            active.handle.cancel();
        }
        self.finish_link_check(false);
    }

    /// On exit: stops the check and writes what it found synchronously (a small file). The workers
    /// are not waited for: a request in flight ends with the process.
    pub fn end_link_check_for_exit(&mut self) {
        let Some(mut active) = self.link_check.take() else {
            return;
        };
        active.handle.cancel();
        while let Some(CheckMsg::Checked(result)) = active.handle.try_next() {
            if active.results.len() < active.total {
                active.results.push(result);
            }
        }
        if active.failed.is_none() {
            let _ = active.report(false).write(&active.run_dir);
        }
    }

    /// The run in `run_dir` is being deleted: its check stops and nothing is written.
    pub fn discard_link_check(&mut self, run_dir: &Path) {
        if self.link_check.as_ref().is_some_and(|active| active.run_dir == run_dir)
            && let Some(mut active) = self.link_check.take()
        {
            active.handle.cancel();
        }
    }

    /// Starts the automatic check of the current run once it has ended with a result (when its
    /// configuration asks for it and no other check runs).
    pub(super) fn auto_check_links(&mut self) {
        if self.link_check.is_some() {
            return;
        }
        let Some(run) = &mut self.current_run else {
            return;
        };
        if run.auto_check || !run.config.check_external_links || !run.finalized {
            return;
        }
        let Some(result) = run.result.ready() else {
            return;
        };
        run.auto_check = true;
        if !matches!(result, ResultLoad::Loaded(_)) {
            run.links.error =
                Some("The run has no readable result file, so its external links are not known.".to_string());
            return;
        }
        if run.links.report.is_some() {
            return;
        }
        let Some(list) = run.links.list.clone() else {
            return;
        };
        let run_dir = run.plan.run_dir.clone();
        let config = run.config.clone();
        self.start_link_check(run_dir, &list, &config);
    }

    /// *Check external links now* for the run on the Run page.
    pub fn check_links_now(&mut self) {
        if let Some(reason) = self.link_check_blocker() {
            self.toasts.warning(format!("{reason}."));
            return;
        }
        let target = match &self.viewed_run {
            ViewedRun::Current => self.current_run.as_ref().and_then(|run| {
                let list = run.links.list.clone()?;
                Some((run.plan.run_dir.clone(), list, run.config.clone()))
            }),
            ViewedRun::Past { .. } => self.past_run.as_ref().and_then(|past| match &past.load {
                super::PastLoad::Ready(data) => {
                    let list = data.links.list.clone()?;
                    Some((past.run_dir.clone(), list, data.record.config.clone()))
                }
                _ => None,
            }),
        };
        match target {
            Some((run_dir, list, config)) => self.start_link_check(run_dir, &list, &config),
            None => self
                .toasts
                .warning("This run has no list of external links (its result file is missing)."),
        }
    }
}
