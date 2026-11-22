//! The OS shell: open a file or URL with its default program, show a file in Explorer.
//! Paths are passed as paths, never through a command interpreter (Windows: `ShellExecuteW`, and
//! `explorer.exe /select,"<path>"` started directly).
//!
//! Command-line quoting for the preview and exports lives in the model
//! ([`crate::model::config::command_line`], re-exported here).

use std::io;
use std::path::Path;

pub use crate::model::config::{Shell, command_line, quote_posix, quote_powershell};

/// Opens a file or directory with its default program.
pub fn open_path(path: &Path) -> io::Result<()> {
    if !path.exists() {
        return Err(not_found(path));
    }
    platform::open(path.as_os_str())
}

/// Opens a URL in the default browser (`http`/`https` only).
pub fn open_url(url: &str) -> io::Result<()> {
    let parsed = url::Url::parse(url.trim())
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, format!("not a URL: {error}")))?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("only http and https URLs are opened, not {}:", parsed.scheme()),
        ));
    }
    platform::open(std::ffi::OsStr::new(parsed.as_str()))
}

/// Shows a file or directory selected in Explorer.
pub fn reveal_in_explorer(path: &Path) -> io::Result<()> {
    if !path.exists() {
        return Err(not_found(path));
    }
    platform::reveal(&std::path::absolute(path)?)
}

fn not_found(path: &Path) -> io::Error {
    io::Error::new(io::ErrorKind::NotFound, format!("{} does not exist", path.display()))
}

#[cfg(windows)]
mod platform {
    use std::ffi::{OsStr, OsString};
    use std::io;
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::process::CommandExt;
    use std::path::Path;
    use std::process::Command;

    use windows_sys::Win32::UI::Shell::ShellExecuteW;
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    fn wide(text: &OsStr) -> Vec<u16> {
        text.encode_wide().chain(std::iter::once(0)).collect()
    }

    pub fn open(target: &OsStr) -> io::Result<()> {
        let verb = wide(OsStr::new("open"));
        let file = wide(target);
        // SAFETY: both strings are NUL-terminated UTF-16 buffers that outlive the call; the other
        // pointers may be null.
        let result = unsafe {
            ShellExecuteW(
                std::ptr::null_mut(),
                verb.as_ptr(),
                file.as_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                SW_SHOWNORMAL,
            )
        };
        // Values above 32 mean success; the others are error codes.
        let code = result as isize;
        if code > 32 {
            return Ok(());
        }
        let error = io::Error::last_os_error();
        Err(match error.raw_os_error() {
            Some(0) | None => io::Error::other(format!("the shell could not open it (code {code})")),
            Some(_) => error,
        })
    }

    pub fn reveal(path: &Path) -> io::Result<()> {
        // Explorer reads its own command line: `/select,"<path>"` as one raw argument.
        let mut arg = OsString::from("/select,\"");
        arg.push(path.as_os_str());
        arg.push("\"");
        // Explorer's exit code says nothing about success; it is not waited for.
        Command::new("explorer.exe").raw_arg(arg).spawn().map(drop)
    }
}

#[cfg(not(windows))]
mod platform {
    use std::ffi::OsStr;
    use std::io;
    use std::path::Path;

    pub fn open(target: &OsStr) -> io::Result<()> {
        open::that_detached(target)
    }

    /// Without a portable "select" call: the containing directory is opened.
    pub fn reveal(path: &Path) -> io::Result<()> {
        open::that_detached(path.parent().unwrap_or(path))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bad_input_is_refused_before_the_shell_is_asked() {
        let kind = |result: io::Result<()>| result.err().map(|error| error.kind());
        assert_eq!(
            kind(open_url("file:///C:/Windows/notepad.exe")),
            Some(io::ErrorKind::InvalidInput)
        );
        assert_eq!(kind(open_url("javascript:alert(1)")), Some(io::ErrorKind::InvalidInput));
        assert_eq!(kind(open_url("not a url")), Some(io::ErrorKind::InvalidInput));
        let missing = Path::new("definitely-missing-dir").join("no such file.html");
        assert_eq!(kind(open_path(&missing)), Some(io::ErrorKind::NotFound));
        assert_eq!(kind(reveal_in_explorer(&missing)), Some(io::ErrorKind::NotFound));
    }
}
