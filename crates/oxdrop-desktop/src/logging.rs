//! The local log a user can attach to a bug report. Nothing is ever sent anywhere.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use tracing_appender::rolling::{RollingFileAppender, Rotation};

/// How many days of log files to keep.
const KEEP_DAYS: usize = 7;

/// Sends `tracing` output to a daily log file in the platform's log folder, and makes
/// a panic write its message and backtrace there before the app exits. Returns the
/// folder, or `None` if there is nowhere to log to (output then goes to stderr).
pub fn init() -> Option<PathBuf> {
    let dir = log_dir(std::env::consts::OS, |name| std::env::var_os(name));
    let appender = dir.as_deref().and_then(|dir| match open_log(dir) {
        Ok(appender) => Some(appender),
        Err(error) => {
            eprintln!("can't log to {}: {error}", dir.display());
            None
        }
    });
    let logged = appender.is_some();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .with_ansi(false);
    match appender {
        // Written straight to the file, not from a background thread, so nothing is
        // lost if the app goes down right after.
        Some(appender) => subscriber.with_writer(appender).init(),
        None => subscriber.with_writer(std::io::stderr).init(),
    }

    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let backtrace = std::backtrace::Backtrace::force_capture();
        tracing::error!("panic: {info}\n{backtrace}");
        default_hook(info);
    }));
    tracing::info!(version = env!("CARGO_PKG_VERSION"), "Oxdrop starting");
    dir.filter(|_| logged)
}

fn open_log(dir: &Path) -> Result<RollingFileAppender, Box<dyn std::error::Error>> {
    std::fs::create_dir_all(dir)?;
    Ok(RollingFileAppender::builder()
        .rotation(Rotation::DAILY)
        .filename_prefix("oxdrop")
        .filename_suffix("log")
        .max_log_files(KEEP_DAYS)
        .build(dir)?)
}

/// Opens `target`, a folder or a web address, the way the OS opens things: in Finder
/// or Explorer, or the default browser.
pub fn reveal(target: &str) {
    let program = match std::env::consts::OS {
        "macos" => "open",
        "windows" => "explorer",
        _ => "xdg-open",
    };
    if let Err(error) = std::process::Command::new(program).arg(target).spawn() {
        tracing::warn!(%error, target, "can't open");
    }
}

/// Where the log goes on `os` (as in `std::env::consts::OS`), given the environment
/// variable lookup `var`, if it can be worked out.
fn log_dir(os: &str, var: impl Fn(&str) -> Option<OsString>) -> Option<PathBuf> {
    // Only an absolute path is trusted; judged by `os`'s rules, not this machine's.
    let absolute = |name: &str| {
        let value = var(name)?;
        let text = value.to_string_lossy();
        let ok = if os == "windows" {
            let bytes = text.as_bytes();
            text.starts_with(r"\\")
                || (bytes.len() >= 3
                    && bytes[0].is_ascii_alphabetic()
                    && bytes[1] == b':'
                    && matches!(bytes[2], b'\\' | b'/'))
        } else {
            text.starts_with('/')
        };
        ok.then(|| PathBuf::from(value))
    };
    match os {
        "macos" => Some(absolute("HOME")?.join("Library/Logs/Oxdrop")),
        "windows" => Some(absolute("LOCALAPPDATA")?.join("Oxdrop").join("logs")),
        _ => match absolute("XDG_STATE_HOME") {
            Some(state) => Some(state.join("oxdrop/logs")),
            None => Some(absolute("HOME")?.join(".local/state/oxdrop/logs")),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env<'a>(vars: &'a [(&str, &str)]) -> impl Fn(&str) -> Option<OsString> + 'a {
        move |name| {
            vars.iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| OsString::from(value))
        }
    }

    #[test]
    fn each_platform_uses_its_own_log_folder() {
        assert_eq!(
            log_dir("macos", env(&[("HOME", "/Users/ana")])),
            Some(PathBuf::from("/Users/ana/Library/Logs/Oxdrop"))
        );
        assert_eq!(
            log_dir(
                "windows",
                env(&[("LOCALAPPDATA", r"C:\Users\ana\AppData\Local")])
            ),
            Some(
                PathBuf::from(r"C:\Users\ana\AppData\Local")
                    .join("Oxdrop")
                    .join("logs")
            )
        );
        assert_eq!(
            log_dir(
                "linux",
                env(&[
                    ("XDG_STATE_HOME", "/home/ana/.state"),
                    ("HOME", "/home/ana")
                ])
            ),
            Some(PathBuf::from("/home/ana/.state/oxdrop/logs"))
        );
        assert_eq!(
            log_dir("linux", env(&[("HOME", "/home/ana")])),
            Some(PathBuf::from("/home/ana/.local/state/oxdrop/logs"))
        );
    }

    #[test]
    fn a_missing_or_relative_variable_gives_no_folder() {
        assert_eq!(log_dir("macos", env(&[])), None);
        assert_eq!(log_dir("macos", env(&[("HOME", "")])), None);
        assert_eq!(
            log_dir("windows", env(&[("LOCALAPPDATA", r"relative\dir")])),
            None
        );
        assert_eq!(
            log_dir(
                "linux",
                env(&[("XDG_STATE_HOME", "relative"), ("HOME", "/home/ana")])
            ),
            Some(PathBuf::from("/home/ana/.local/state/oxdrop/logs")),
            "XDG says to ignore a relative path"
        );
    }
}
