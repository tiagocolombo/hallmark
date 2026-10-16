//! The test bench: the repo's gates, in order, at the frozen commit, in the
//! assay worktree. A script decides; no model is involved.

use std::fs::{self, File};
use std::io::Write;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use crate::error::Result;
use crate::event::{Body, CheckRun, short};
use crate::item::{Item, State};
use crate::journal::ev;
use crate::worker::{Progress, WorkerCtx};

/// The exit code recorded for a gate that ran out of time (as `timeout(1)`).
pub const TIMEOUT_EXIT: i32 = 124;

/// Runs every gate (even after one fails, so the fixer sees all of them in
/// one round), writes `checks/r<round>.log`, and records `checks`.
pub fn run(ctx: &WorkerCtx, item: &Item) -> Result<Progress> {
    let repo = ctx.repo;
    let Some(sha) = item.frozen_sha.clone() else {
        return Ok(Progress::Idle);
    };
    let round = item.round;
    let gates = repo.config.gates.clone();
    let mut runs = Vec::new();
    if !gates.is_empty() {
        // A slot of the assay pool, locked until the checks end.
        let slot = super::checkout_checks(repo, &sha)?;
        let wt = slot.path.clone();
        let dir = repo.hall.item_checks(item.id);
        fs::create_dir_all(&dir)?;
        let rel = format!("checks/r{round}.log");
        let mut log = File::create(dir.join(format!("r{round}.log")))?;
        writeln!(
            log,
            "# {} round {round}: checks at {sha} in {}",
            item.id,
            crate::paths::display(&wt)
        )?;
        for (i, g) in gates.iter().enumerate() {
            // Renew the lease: each gate may take up to its timeout.
            let _ = crate::lease::claim(
                &repo.hall,
                item.id,
                super::ROLE,
                &ctx.holder,
                super::lease_ttl(repo),
            );
            repo.hall.live(&format!(
                "{} checks {}/{}: {g}",
                item.id,
                i + 1,
                gates.len()
            ));
            writeln!(log, "\n$ {g}")?;
            log.flush()?;
            let timeout = repo.config.gate_timeout_secs(g);
            let (exit_code, duration_ms) = run_gate(g.cmd(), &wt, &log, timeout)?;
            writeln!(
                log,
                "\n[exit {exit_code} after {:.1}s]",
                duration_ms as f64 / 1000.0
            )?;
            runs.push(CheckRun {
                command: g.cmd().to_string(),
                exit_code,
                duration_ms,
                log: Some(rel.clone()),
            });
        }
        log.flush()?;
    }
    let passed = runs.iter().all(|r| r.exit_code == 0);
    let failed: Vec<String> = runs
        .iter()
        .filter(|r| r.exit_code != 0)
        .map(|r| format!("`{}` exit {}", r.command, r.exit_code))
        .collect();
    let n = runs.len();
    let (_, recorded) = repo.hall.transact(item.id, |cur, _| {
        // Pulled back, dropped or resubmitted while the gates ran.
        if cur.state != State::Checks
            || cur.frozen_sha.as_deref() != Some(sha.as_str())
            || cur.round != round
        {
            return Ok((vec![], false));
        }
        let e = ev(
            super::ROLE,
            Some(cur.id),
            Body::Checks {
                sha: sha.clone(),
                round,
                passed,
                runs: runs.clone(),
            },
        );
        Ok((vec![e], true))
    })?;
    let what = if !recorded {
        format!(
            "{} checks at {} discarded: the item moved on",
            item.id,
            short(&sha)
        )
    } else if passed {
        format!(
            "{} checks passed at {} ({n} gate{})",
            item.id,
            short(&sha),
            if n == 1 { "" } else { "s" }
        )
    } else {
        format!("{} checks failed: {}", item.id, failed.join(", "))
    };
    repo.hall.live(&what);
    Ok(Progress::worked(what))
}

/// Runs every gate at `sha` in a slot of the assay pool, outside the
/// review loop, and writes `checks/<name>.log`. `renew` runs before each
/// gate, so the caller can renew its lease. Used by the Dispatch Dock to
/// check a rebased commit before carrying its hallmark (ADR 0030).
/// `CLAIMED` when every slot is busy.
pub fn gates_at(
    repo: &crate::repo::Repo,
    id: crate::item::ItemId,
    sha: &str,
    name: &str,
    renew: &dyn Fn(),
) -> Result<Vec<CheckRun>> {
    let gates = repo.config.gates.clone();
    if gates.is_empty() {
        return Ok(vec![]);
    }
    let slot = super::checkout_checks(repo, sha)?;
    let dir = repo.hall.item_checks(id);
    fs::create_dir_all(&dir)?;
    let rel = format!("checks/{name}.log");
    let mut log = File::create(dir.join(format!("{name}.log")))?;
    writeln!(
        log,
        "# {id} {name}: checks at {sha} in {}",
        crate::paths::display(&slot.path)
    )?;
    let mut runs = Vec::new();
    for (i, g) in gates.iter().enumerate() {
        renew();
        repo.hall.live(&format!(
            "{id} {name} checks {}/{}: {g}",
            i + 1,
            gates.len()
        ));
        writeln!(log, "\n$ {g}")?;
        log.flush()?;
        let timeout = repo.config.gate_timeout_secs(g);
        let (exit_code, duration_ms) = run_gate(g.cmd(), &slot.path, &log, timeout)?;
        writeln!(
            log,
            "\n[exit {exit_code} after {:.1}s]",
            duration_ms as f64 / 1000.0
        )?;
        runs.push(CheckRun {
            command: g.cmd().to_string(),
            exit_code,
            duration_ms,
            log: Some(rel.clone()),
        });
    }
    log.flush()?;
    Ok(runs)
}

/// Runs one gate through the platform shell with stdout and stderr both
/// appended to `log`. Returns (exit code, milliseconds).
pub fn run_gate(cmd: &str, dir: &Path, log: &File, timeout_secs: u64) -> Result<(i32, u64)> {
    let mut c = crate::commands::new::shell(cmd);
    c.current_dir(dir)
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log.try_clone()?)
        .env("HALL_ACTOR", super::ROLE)
        .env("HALL_NO_HOOKS", "1");
    for v in ["GIT_DIR", "GIT_WORK_TREE", "GIT_INDEX_FILE", "CLAUDECODE"] {
        c.env_remove(v);
    }
    // Its own process group, so a timeout kills the whole tree.
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        c.process_group(0);
    }
    let start = Instant::now();
    let mut child = match c.spawn() {
        Ok(ch) => ch,
        Err(e) => {
            let mut l = log;
            writeln!(l, "could not run `{cmd}`: {e}")?;
            return Ok((127, 0));
        }
    };
    let status = if timeout_secs == 0 {
        child.wait().ok()
    } else {
        use wait_timeout::ChildExt;
        match child.wait_timeout(Duration::from_secs(timeout_secs)) {
            Ok(Some(s)) => Some(s),
            _ => {
                kill_tree(&mut child);
                None
            }
        }
    };
    let ms = start.elapsed().as_millis() as u64;
    Ok(match status {
        Some(s) => (s.code().unwrap_or(-1), ms),
        None => {
            let mut l = log;
            writeln!(l, "\n[timed out after {timeout_secs}s; killed]")?;
            (TIMEOUT_EXIT, ms)
        }
    })
}

fn kill_tree(child: &mut Child) {
    let pid = child.id().to_string();
    #[cfg(unix)]
    let _ = Command::new("sh")
        .arg("-c")
        .arg(format!("kill -KILL -- -{pid}"))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    #[cfg(windows)]
    let _ = Command::new("taskkill")
        .args(["/T", "/F", "/PID", &pid])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    let _ = child.kill();
    let _ = child.wait();
}
