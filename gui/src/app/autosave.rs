//! Ordered autosave of the working configuration and of the settings (GUI-SPEC §5.6).
//!
//! An [`Autosave`] decides when a value is written and keeps the writes in order:
//!
//! - a changed value is written after it has stayed unchanged for a while (debounce);
//! - writes run one at a time and in the order they were made: each has a generation, a write
//!   holds the shared order lock while it writes, and a write older than one that has started is
//!   skipped. [`Autosave::flush`] takes part in the same order, so nothing older runs after it;
//! - a value counts as saved only when its write succeeded. After a failure the stored state is
//!   unknown (a part may have been written): the current value is written again — after the next
//!   change, after a back-off ([`RETRY_FIRST`] doubling up to [`RETRY_MAX`]) and on exit.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// The first automatic retry after a failed write.
pub const RETRY_FIRST: Duration = Duration::from_secs(5);

/// The longest wait between two automatic retries.
pub const RETRY_MAX: Duration = Duration::from_secs(60);

/// What [`Autosave::poll`] asks for.
pub enum Step<T> {
    /// Nothing to write now.
    Idle,
    /// Ask again after this long.
    Wait(Duration),
    /// Write this, on a worker ([`Ticket::run`]).
    Save(Ticket<T>),
}

/// A write to run.
pub struct Ticket<T> {
    value: T,
    generation: u64,
    order: Arc<Mutex<u64>>,
}

/// The end of a write, for [`Autosave::finish`].
pub struct Done<T> {
    value: T,
    generation: u64,
    outcome: Outcome,
}

enum Outcome {
    Saved,
    /// A newer write started first.
    Skipped,
    Failed(String),
}

/// What a finished write means for the user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Report {
    Nothing,
    /// The write failed (the message of the failure).
    Failed(String),
    /// A write succeeded after a failure.
    Recovered,
}

impl<T> Ticket<T> {
    /// Writes the value unless a newer write has started, after every earlier write.
    pub fn run(self, write: impl FnOnce(&T) -> Result<(), String>) -> Done<T> {
        let outcome = {
            let mut last = self.order.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            if *last > self.generation {
                Outcome::Skipped
            } else {
                *last = self.generation;
                match write(&self.value) {
                    Ok(()) => Outcome::Saved,
                    Err(error) => Outcome::Failed(error),
                }
            }
        };
        Done {
            value: self.value,
            generation: self.generation,
            outcome,
        }
    }
}

pub struct Autosave<T> {
    /// The value known to be stored; `None` after a failed write (the stored state is unknown).
    saved: Option<T>,
    /// The newest value handed to a writer that has not finished, with its generation.
    in_flight: Option<(u64, T)>,
    /// The value whose write failed last, and when to retry it.
    failed: Option<(T, Instant)>,
    /// Failed writes in a row (for the back-off).
    failures: u32,
    dirty_since: Option<Instant>,
    generation: u64,
    /// The newest generation whose end was applied.
    finished: u64,
    /// The generation of the newest write that started; held while a write runs.
    order: Arc<Mutex<u64>>,
}

impl<T: PartialEq + Clone> Autosave<T> {
    /// `saved` is the value as it was loaded.
    pub fn new(saved: T) -> Self {
        Self {
            saved: Some(saved),
            in_flight: None,
            failed: None,
            failures: 0,
            dirty_since: None,
            generation: 0,
            finished: 0,
            order: Arc::new(Mutex::new(0)),
        }
    }

    /// What to do about `current` now.
    pub fn poll(&mut self, current: &T, delay: Duration, now: Instant) -> Step<T> {
        if self.saved.as_ref() == Some(current) {
            self.dirty_since = None;
            return Step::Idle;
        }
        if self.in_flight.as_ref().is_some_and(|(_, value)| value == current) {
            self.dirty_since = None;
            return Step::Idle;
        }
        if let Some((value, retry_at)) = &self.failed
            && value == current
        {
            // Unchanged since the failure: retried after the back-off.
            self.dirty_since = None;
            return match retry_at.checked_duration_since(now) {
                Some(wait) if !wait.is_zero() => Step::Wait(wait),
                _ => self.send(current),
            };
        }
        let since = *self.dirty_since.get_or_insert(now);
        let waited = now.saturating_duration_since(since);
        if waited >= delay {
            self.send(current)
        } else {
            Step::Wait(delay - waited)
        }
    }

    fn send(&mut self, current: &T) -> Step<T> {
        self.generation += 1;
        self.dirty_since = None;
        self.in_flight = Some((self.generation, current.clone()));
        Step::Save(Ticket {
            value: current.clone(),
            generation: self.generation,
            order: Arc::clone(&self.order),
        })
    }

    /// Applies the end of a write.
    pub fn finish(&mut self, done: Done<T>, now: Instant) -> Report {
        if self
            .in_flight
            .as_ref()
            .is_some_and(|(generation, _)| *generation == done.generation)
        {
            self.in_flight = None;
        }
        // A skipped write, or the end of one older than an end already applied, says nothing about
        // what is stored now.
        if matches!(done.outcome, Outcome::Skipped) || done.generation < self.finished {
            return Report::Nothing;
        }
        self.finished = done.generation;
        self.record(done.value, done.outcome, now)
    }

    fn record(&mut self, value: T, outcome: Outcome, now: Instant) -> Report {
        match outcome {
            Outcome::Saved => {
                self.saved = Some(value);
                self.failed = None;
                let recovered = std::mem::take(&mut self.failures) > 0;
                if recovered { Report::Recovered } else { Report::Nothing }
            }
            Outcome::Failed(error) => {
                self.saved = None;
                self.failures = self.failures.saturating_add(1);
                let backoff = RETRY_FIRST
                    .saturating_mul(1 << (self.failures - 1).min(6))
                    .min(RETRY_MAX);
                self.failed = Some((value, now + backoff));
                Report::Failed(error)
            }
            Outcome::Skipped => Report::Nothing,
        }
    }

    /// Writes `current` now on this thread unless it is known to be stored with nothing in flight
    /// (on exit). Waits for a write that is running; a write that has not started yet is skipped.
    pub fn flush(&mut self, current: &T, write: impl FnOnce(&T) -> Result<(), String>) -> Report {
        if !self.is_pending(current) {
            return Report::Nothing;
        }
        self.generation += 1;
        let generation = self.generation;
        let done = Ticket {
            value: current.clone(),
            generation,
            order: Arc::clone(&self.order),
        }
        .run(write);
        // Ends of older writes that arrive later are ignored.
        self.in_flight = None;
        self.dirty_since = None;
        self.finished = generation;
        self.record(done.value, done.outcome, Instant::now())
    }

    /// Whether `current` is not known to be stored, or a write is in flight ([`Self::flush`] writes
    /// it then).
    pub fn is_pending(&self, current: &T) -> bool {
        self.saved.as_ref() != Some(current) || self.in_flight.is_some()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;
    use std::thread;

    use super::*;

    const DELAY: Duration = Duration::from_millis(800);

    fn save(step: Step<u32>) -> Ticket<u32> {
        match step {
            Step::Save(ticket) => ticket,
            _ => panic!("expected a write"),
        }
    }

    #[test]
    fn a_change_is_written_once_after_it_stays_quiet() {
        let start = Instant::now();
        let mut autosave = Autosave::new(1);
        assert!(matches!(autosave.poll(&1, DELAY, start), Step::Idle));
        assert!(matches!(autosave.poll(&2, DELAY, start), Step::Wait(_)));
        let ticket = save(autosave.poll(&2, DELAY, start + DELAY));
        // In flight: not written twice.
        assert!(matches!(autosave.poll(&2, DELAY, start + DELAY * 3), Step::Idle));
        assert!(autosave.is_pending(&2));
        let done = ticket.run(|_| Ok(()));
        assert_eq!(autosave.finish(done, start + DELAY), Report::Nothing);
        assert!(!autosave.is_pending(&2));
        assert!(matches!(autosave.poll(&2, DELAY, start + DELAY * 4), Step::Idle));
    }

    #[test]
    fn a_failed_write_stays_pending_and_is_retried_after_a_backoff_or_the_next_change() {
        let start = Instant::now();
        let mut autosave = Autosave::new(1);
        autosave.poll(&2, DELAY, start);
        let done = save(autosave.poll(&2, DELAY, start + DELAY)).run(|_| Err("disk full".to_string()));
        let failed_at = start + DELAY;
        assert_eq!(
            autosave.finish(done, failed_at),
            Report::Failed("disk full".to_string())
        );
        assert!(autosave.is_pending(&2), "a failed value is not saved");
        // No retry storm: nothing before the back-off.
        assert!(matches!(
            autosave.poll(&2, DELAY, failed_at + RETRY_FIRST / 2),
            Step::Wait(_)
        ));
        let done = save(autosave.poll(&2, DELAY, failed_at + RETRY_FIRST)).run(|_| Err("disk full".to_string()));
        let failed_again = failed_at + RETRY_FIRST;
        autosave.finish(done, failed_again);
        // The back-off doubles.
        assert!(matches!(
            autosave.poll(&2, DELAY, failed_again + RETRY_FIRST),
            Step::Wait(_)
        ));
        // A change is written after the usual quiet time, without waiting for the back-off.
        assert!(matches!(autosave.poll(&3, DELAY, failed_again), Step::Wait(_)));
        let done = save(autosave.poll(&3, DELAY, failed_again + DELAY)).run(|_| Ok(()));
        assert_eq!(autosave.finish(done, failed_again + DELAY), Report::Recovered);
        assert!(!autosave.is_pending(&3));
        // Even back to the value stored before the failure: what is stored is not known then.
        let mut autosave = Autosave::new(1);
        autosave.poll(&2, DELAY, start);
        let done = save(autosave.poll(&2, DELAY, start + DELAY)).run(|_| Err("partly".to_string()));
        autosave.finish(done, start + DELAY);
        assert!(autosave.is_pending(&1));
    }

    #[test]
    fn flush_writes_a_failed_value_and_waits_for_a_running_write() {
        let start = Instant::now();
        let mut autosave = Autosave::new(1);
        autosave.poll(&2, DELAY, start);
        let done = save(autosave.poll(&2, DELAY, start + DELAY)).run(|_| Err("locked".to_string()));
        autosave.finish(done, start + DELAY);
        let mut written = None;
        let report = autosave.flush(&2, |value| {
            written = Some(*value);
            Ok(())
        });
        assert_eq!((report, written), (Report::Recovered, Some(2)));
        assert!(!autosave.is_pending(&2));
        assert_eq!(autosave.flush(&2, |_| panic!("nothing to write")), Report::Nothing);
    }

    #[test]
    fn an_older_write_never_runs_after_a_newer_one_or_after_flush() {
        let start = Instant::now();
        let mut autosave = Autosave::new(0);
        let stored = Arc::new(Mutex::new(0));
        autosave.poll(&1, DELAY, start);
        let first = save(autosave.poll(&1, DELAY, start + DELAY));
        autosave.poll(&2, DELAY, start + DELAY);
        let second = save(autosave.poll(&2, DELAY, start + DELAY * 2));
        // The newer write runs first: the older one is skipped when it comes.
        let write = |stored: Arc<Mutex<u32>>| {
            move |value: &u32| -> Result<(), String> {
                *stored.lock().expect("lock") = *value;
                Ok(())
            }
        };
        let done_second = second.run(write(Arc::clone(&stored)));
        let done_first = first.run(write(Arc::clone(&stored)));
        assert_eq!(*stored.lock().expect("lock"), 2);
        autosave.finish(done_second, start);
        autosave.finish(done_first, start);
        assert!(!autosave.is_pending(&2));

        // A write that started first holds the order: a newer one waits for it.
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel::<()>();
        autosave.poll(&3, DELAY, start);
        let third = save(autosave.poll(&3, DELAY, start + DELAY));
        let slow = {
            let stored = Arc::clone(&stored);
            thread::spawn(move || {
                third.run(|value| {
                    started_tx.send(()).expect("started");
                    release_rx.recv().expect("released");
                    *stored.lock().expect("lock") = *value;
                    Ok(())
                })
            })
        };
        started_rx.recv().expect("the slow write started");
        // A write made later but not started yet when flush runs is skipped.
        autosave.poll(&4, DELAY, start);
        let fourth = save(autosave.poll(&4, DELAY, start + DELAY));
        let flushing = {
            let stored = Arc::clone(&stored);
            thread::spawn(move || {
                let mut autosave = autosave;
                let report = autosave.flush(&5, |value| {
                    *stored.lock().expect("lock") = *value;
                    Ok(())
                });
                (autosave, report)
            })
        };
        thread::sleep(Duration::from_millis(50));
        assert_eq!(*stored.lock().expect("lock"), 2, "flush waits for the running write");
        release_tx.send(()).expect("release");
        let done_third = slow.join().expect("slow write");
        let (mut autosave, report) = flushing.join().expect("flush");
        assert_eq!(report, Report::Nothing);
        let done_fourth = fourth.run(|_| panic!("older than the flush"));
        assert_eq!(*stored.lock().expect("lock"), 5);
        autosave.finish(done_third, start);
        autosave.finish(done_fourth, start);
        assert!(!autosave.is_pending(&5));
    }
}
