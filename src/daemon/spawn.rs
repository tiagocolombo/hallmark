//! Launching `hall daemon run` in the background, detached from the
//! terminal that ran `hall daemon start`.
//!
//! - Unix: a new process group, so the terminal's Ctrl-C and hangup don't
//!   reach it.
//! - Windows: `DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP |
//!   CREATE_NO_WINDOW`: no console, no window.
//!
//! stdin is null; stdout and stderr go to `daemon/daemon.log`, so the
//! process holds nothing of the caller's (an `assert_cmd` pipe would
//! otherwise stay open until the daemon exits).

use std::fs::OpenOptions;
use std::process::{Child, Command, Stdio};

use crate::error::{ErrorCode, HallError, Result};

use super::state;

/// Session variables that must not leak into the daemon: it is nobody's
/// session, and its `claude -p` runs must not look nested.
const SCRUB: &[&str] = &[
    "CLAUDECODE",
    "CLAUDE_CODE_ENTRYPOINT",
    "CLAUDE_SESSION_ID",
    "CLAUDE_CODE_SESSION_ID",
    "HALL_ACTOR",
];

pub fn launch() -> Result<Child> {
    let exe = std::env::current_exe()?;
    std::fs::create_dir_all(state::dir())?;
    let log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(state::log_path())?;
    let err = log.try_clone()?;
    let mut cmd = crate::process::command(exe);
    cmd.args(["daemon", "run"])
        .current_dir(state::dir())
        .stdin(Stdio::null())
        .stdout(log)
        .stderr(err);
    for v in SCRUB {
        cmd.env_remove(v);
    }
    detach(&mut cmd);
    cmd.spawn().map_err(|e| {
        HallError::new(
            ErrorCode::CommandFailed,
            format!("could not start the daemon: {e}"),
        )
        .next(["hall daemon run"])
    })
}

#[cfg(unix)]
pub(crate) fn detach(cmd: &mut Command) {
    use std::os::unix::process::CommandExt;
    cmd.process_group(0);
}

#[cfg(windows)]
pub(crate) fn detach(cmd: &mut Command) {
    use std::os::windows::process::CommandExt;
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    cmd.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW);
    keep_std_handles_to_ourselves();
}

/// Windows starts a child with every inheritable handle of ours, not just the
/// ones given as its stdio. Our own stdin, stdout and stderr (a pipe, when a
/// caller captures our output) would stay open in the detached process, and
/// the caller would wait for them until it exits. The child gets its stdio
/// from the `Command`, which duplicates what it needs, so ours can stop being
/// inheritable.
#[cfg(windows)]
fn keep_std_handles_to_ourselves() {
    use core::ffi::c_void;
    const STD_HANDLES: [u32; 3] = [-10i32 as u32, -11i32 as u32, -12i32 as u32];
    const HANDLE_FLAG_INHERIT: u32 = 1;
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetStdHandle(which: u32) -> *mut c_void;
        fn SetHandleInformation(handle: *mut c_void, mask: u32, flags: u32) -> i32;
    }
    for which in STD_HANDLES {
        // SAFETY: both calls only read or flag a handle of this process; a
        // null or invalid handle makes SetHandleInformation fail, which is
        // ignored.
        unsafe {
            let h = GetStdHandle(which);
            if !h.is_null() && h as isize != -1 {
                SetHandleInformation(h, HANDLE_FLAG_INHERIT, 0);
            }
        }
    }
}

#[cfg(not(any(unix, windows)))]
pub(crate) fn detach(_cmd: &mut Command) {}

/// The last `n` lines of the daemon log, for an error message.
pub fn log_tail(n: usize) -> String {
    let text = std::fs::read_to_string(state::log_path()).unwrap_or_default();
    let lines: Vec<&str> = text.lines().collect();
    lines[lines.len().saturating_sub(n)..].join("\n")
}
