//! Child processes. Every `Command` Hallmark spawns starts here (ADR 0054).
//!
//! On Windows, a process without a console gets a new, visible console for
//! every console program it starts. The daemon runs without one
//! (`DETACHED_PROCESS`, ADR 0017), so each `git`, `gh`, `claude` and gate it
//! ran opened a window of its own, several a second. `CREATE_NO_WINDOW`
//! gives such a child an invisible console instead, which the child's own
//! children inherit, so `cmd /C claude.cmd` and `cmd /C <gate>` stay hidden
//! too.
//!
//! The flag is set only when this process has no console. With one (`hall`
//! in a terminal) children inherit it and open no window anyway, and the
//! flag would move them to a hidden console of their own, out of reach of
//! the terminal's Ctrl-C. Unix is untouched.

use std::ffi::OsStr;
use std::process::Command;

/// `Command::new`, with no console window on Windows.
pub fn command(program: impl AsRef<OsStr>) -> Command {
    let mut cmd = Command::new(program);
    hide_console(&mut cmd);
    cmd
}

const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// The Windows creation flags a child needs: `CREATE_NO_WINDOW` only when
/// this process has no console.
#[cfg_attr(not(windows), allow(dead_code))]
fn creation_flags(has_console: bool) -> Option<u32> {
    if has_console {
        None
    } else {
        Some(CREATE_NO_WINDOW)
    }
}

#[cfg(windows)]
fn hide_console(cmd: &mut Command) {
    use std::os::windows::process::CommandExt;
    if let Some(flags) = creation_flags(has_console()) {
        cmd.creation_flags(flags);
    }
}

#[cfg(not(windows))]
fn hide_console(_cmd: &mut Command) {}

/// Whether this process is attached to a console, visible or hidden.
#[cfg(windows)]
fn has_console() -> bool {
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetConsoleWindow() -> *mut core::ffi::c_void;
    }
    // SAFETY: GetConsoleWindow takes no arguments and only reads this
    // process's state.
    !unsafe { GetConsoleWindow() }.is_null()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_window_flag_only_without_a_console() {
        assert_eq!(creation_flags(false), Some(0x0800_0000));
        assert_eq!(creation_flags(true), None);
    }

    #[test]
    fn a_command_runs_like_any_other() {
        let out = if cfg!(windows) {
            command("cmd").args(["/C", "echo", "hi"]).output()
        } else {
            command("echo").arg("hi").output()
        };
        let out = out.unwrap();
        assert!(out.status.success());
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "hi");
    }
}
