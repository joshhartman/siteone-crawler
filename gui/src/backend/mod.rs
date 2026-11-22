//! The backend: the crawler child process, one-shot tool calls, ANSI parsing, the OS shell.
//!
//! Free of egui: workers wake the UI through a `waker: impl Fn() + Send + Sync + 'static`
//! (the app passes a closure that calls `egui::Context::request_repaint`).

pub mod ansi;
pub mod link_check;
pub mod link_text;
pub mod retry;
pub mod runner;
pub mod shell;
pub mod tools;

use std::cell::Cell;
use std::panic::{AssertUnwindSafe, UnwindSafe};
use std::process::Command;

thread_local! {
    /// How many [`catch_panic`] scopes the current thread is in.
    static CATCHING: Cell<u32> = const { Cell::new(0) };
}

/// Runs `work` and catches a panic in it. While it runs, [`panic_is_caught`] is `true` on this
/// thread, so the panic hook can tell a panic the app survives from one that ends it.
pub fn catch_panic<R>(work: impl FnOnce() -> R + UnwindSafe) -> std::thread::Result<R> {
    struct Scope;
    impl Drop for Scope {
        fn drop(&mut self) {
            CATCHING.with(|depth| depth.set(depth.get().saturating_sub(1)));
        }
    }
    CATCHING.with(|depth| depth.set(depth.get() + 1));
    let _scope = Scope;
    std::panic::catch_unwind(work)
}

/// [`catch_panic`] for closures that are not `UnwindSafe` (the caller drops their state after a
/// panic).
pub fn catch_panic_unchecked<R>(work: impl FnOnce() -> R) -> std::thread::Result<R> {
    catch_panic(AssertUnwindSafe(work))
}

/// Whether a panic on this thread now would be caught by [`catch_panic`] (the app goes on).
pub fn panic_is_caught() -> bool {
    CATCHING.with(Cell::get) > 0
}

/// Starts the child without a console window of its own (Windows: `CREATE_NO_WINDOW`).
fn hide_console_window(command: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(not(windows))]
    let _ = command;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_panic_inside_catch_panic_is_known_to_be_caught() {
        assert!(!panic_is_caught());
        let seen = catch_panic(panic_is_caught).expect("no panic");
        assert!(seen);
        let caught = catch_panic_unchecked(|| -> () { panic!("a test panic, caught on purpose") });
        assert!(caught.is_err());
        assert!(!panic_is_caught(), "the scope ends after a panic too");
    }
}
