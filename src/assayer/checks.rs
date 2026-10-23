//! The test bench: the repo's gates, in order, at the frozen commit, in the
//! assay worktree. A script decides; no model is involved.

use std::fs::{self, File};
use std::io::Write;
use std::path::Path;
use std::process::{Child, Stdio};
use std::time::{Duration, Instant};

use super::cache;
use crate::config::Gate;
use crate::error::Result;
use crate::event::{Body, CheckRun, short};
use crate::item::{HoldKind, Item, State};
use crate::journal::ev;
use crate::worker::{Progress, WorkerCtx};

/// The exit code recorded for a gate that ran out of time (as `timeout(1)`).
pub const TIMEOUT_EXIT: i32 = 124;

/// Runs every gate (even after one fails, so the fixer sees all of them in
/// one round), writes `checks/r<round>.log`, and records `checks`. The
/// `max_check_failures`-th failure in a row holds the item for you (kind
/// `checks`): fixes that never turn the gates green don't loop.
pub fn run(ctx: &WorkerCtx, item: &Item) -> Result<Progress> {
    let repo = ctx.repo;
    let Some(sha) = item.frozen_sha.clone() else {
        return Ok(Progress::Idle);
    };
    let round = item.round;
    let gates = repo.config.gates.clone();
    let mut runs = Vec::new();
    if !gates.is_empty() {
        let mut bench = Bench::open(
            repo,
            item.id,
            &sha,
            &format!("r{round}"),
            &format!("round {round}"),
        )?;
        let forced = bench.forced(&gates);
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
                "{} checks {}/{}: {}",
                item.id,
                i + 1,
                gates.len(),
                g.cmd()
            ));
            runs.push(bench.gate(g, i < forced)?);
        }
        bench.log.flush()?;
    }
    let passed = runs.iter().all(|r| r.exit_code == 0);
    let failed: Vec<String> = runs
        .iter()
        .filter(|r| r.exit_code != 0)
        .map(|r| format!("`{}` exit {}", r.command, r.exit_code))
        .collect();
    let n = runs.len();
    let max = repo.config.max_check_failures;
    let (_, (recorded, held, in_a_row)) = repo.hall.transact(item.id, |cur, _| {
        // Pulled back, dropped or resubmitted while the gates ran.
        if cur.state != State::Checks
            || cur.frozen_sha.as_deref() != Some(sha.as_str())
            || cur.round != round
        {
            return Ok((vec![], (false, false, 0)));
        }
        let mut events = vec![ev(
            super::ROLE,
            Some(cur.id),
            Body::Checks {
                sha: sha.clone(),
                round,
                passed,
                runs: runs.clone(),
            },
        )];
        let in_a_row = cur.check_failures + 1;
        let held = !passed && max > 0 && in_a_row >= max;
        if held {
            events.push(ev(
                super::ROLE,
                Some(cur.id),
                Body::Held {
                    kind: HoldKind::Checks,
                    reason: format!(
                        "the checks failed {in_a_row} times in a row (max_check_failures = {max}): {}",
                        failed.join(", ")
                    ),
                },
            ));
        }
        Ok((events, (true, held, in_a_row)))
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
    } else if held {
        format!(
            "{} held: the checks failed {in_a_row} times in a row: {}",
            item.id,
            failed.join(", ")
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
/// check a rebased commit before carrying its hallmark (ADR 0030). Passes
/// on the same tree come from the checks cache, as in [`run`].
/// `CLAIMED` when every slot is busy and a gate has to run.
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
    let mut bench = Bench::open(repo, id, sha, name, name)?;
    let mut runs = Vec::new();
    let forced = bench.forced(&gates);
    for (i, g) in gates.iter().enumerate() {
        renew();
        repo.hall.live(&format!(
            "{id} {name} checks {}/{}: {}",
            i + 1,
            gates.len(),
            g.cmd()
        ));
        runs.push(bench.gate(g, i < forced)?);
    }
    bench.log.flush()?;
    Ok(runs)
}

/// One run of the gates at one commit: its log, and a slot of the assay
/// pool, taken only when a gate has to run. A gate that passed before on the
/// same tree is served from the checks cache ([`super::cache`]) instead,
/// unless `gates_no_cache` lists it; every gate that runs is recorded there.
struct Bench<'a> {
    repo: &'a crate::repo::Repo,
    id: crate::item::ItemId,
    sha: String,
    log: File,
    /// The log, relative to the item's journal directory.
    rel: String,
    /// None when git can't name the tree: nothing is cached.
    tree: Option<String>,
    slot: Option<super::Checkout>,
}

impl<'a> Bench<'a> {
    /// Starts `checks/<name>.log`.
    fn open(
        repo: &'a crate::repo::Repo,
        id: crate::item::ItemId,
        sha: &str,
        name: &str,
        title: &str,
    ) -> Result<Bench<'a>> {
        let dir = repo.hall.item_checks(id);
        fs::create_dir_all(&dir)?;
        let mut log = File::create(dir.join(format!("{name}.log")))?;
        writeln!(log, "# {id} {title}: checks at {sha}")?;
        Ok(Bench {
            repo,
            id,
            sha: sha.to_string(),
            log,
            rel: format!("checks/{name}.log"),
            tree: cache::tree(&repo.main_root, sha),
            slot: None,
        })
    }

    /// The key of a gate that may be cached.
    fn key(&self, g: &str) -> Option<cache::Key> {
        let config = &self.repo.config;
        self.tree
            .as_deref()
            .filter(|_| !config.gates_no_cache.iter().any(|n| n.trim() == g.trim()))
            .map(|t| {
                let cmds: Vec<String> = config.gates.iter().map(|g| g.cmd().to_string()).collect();
                cache::Key::new(t, g, &cmds)
            })
    }

    /// The passing run that serves `g`, if any. Never one whose log is the
    /// file this bench is writing (a round run again): that log was just
    /// truncated, so it no longer holds that run's output.
    fn hit(&self, key: Option<&cache::Key>) -> Option<cache::CachedRun> {
        let own = format!("items/{}/{}", self.id, self.rel);
        cache::passed(&self.repo.hall, key?).filter(|h| h.log.as_deref() != Some(own.as_str()))
    }

    /// How many leading gates must run. A served gate does nothing in the
    /// slot, so every gate before one that has to run runs too (a generator
    /// or an install its successors depend on); only the gates after the
    /// last one that runs are served.
    fn forced(&self, gates: &[Gate]) -> usize {
        gates
            .iter()
            .rposition(|g| self.hit(self.key(g.cmd()).as_ref()).is_none())
            .map_or(0, |i| i + 1)
    }

    /// One gate's result: from the cache when it passed on this tree
    /// before and `force` is off, else by running it.
    fn gate(&mut self, g: &Gate, force: bool) -> Result<CheckRun> {
        let cmd = g.cmd();
        let timeout = self.repo.config.gate_timeout_secs(g);
        let key = self.key(cmd);
        let hit = if force { None } else { self.hit(key.as_ref()) };
        if let Some(hit) = hit {
            writeln!(
                self.log,
                "\n$ {cmd}\n[cached: passed on tree {} for {} at {}; log {}]",
                short(&hit.tree),
                hit.item,
                short(&hit.sha),
                hit.log.as_deref().unwrap_or("none")
            )?;
            return Ok(CheckRun {
                command: cmd.to_string(),
                exit_code: 0,
                duration_ms: hit.duration_ms,
                log: hit.log,
                cached: true,
            });
        }
        let wt = match &self.slot {
            Some(s) => s.path.clone(),
            None => {
                // A slot of the assay pool, locked until the checks end.
                let slot = super::checkout_checks(self.repo, &self.sha)?;
                writeln!(self.log, "# in {}", crate::paths::display(&slot.path))?;
                let wt = slot.path.clone();
                self.slot = Some(slot);
                wt
            }
        };
        writeln!(self.log, "\n$ {cmd}")?;
        self.log.flush()?;
        let (exit_code, duration_ms) = run_gate(cmd, &wt, &self.log, timeout)?;
        writeln!(
            self.log,
            "\n[exit {exit_code} after {:.1}s]",
            duration_ms as f64 / 1000.0
        )?;
        if let Some(k) = key {
            let run = cache::CachedRun {
                key: k.id(),
                tree: k.tree,
                command: k.command,
                gates: k.gates,
                os: k.os,
                item: self.id.to_string(),
                sha: self.sha.clone(),
                at: chrono::Utc::now(),
                exit_code,
                duration_ms,
                log: Some(format!("items/{}/{}", self.id, self.rel)),
            };
            // The cache only saves time: failing to write it fails nothing.
            if let Err(e) = cache::record(&self.repo.hall, &run) {
                self.repo
                    .hall
                    .live(&format!("{} checks cache not written: {e}", self.id));
            }
        }
        Ok(CheckRun {
            command: cmd.to_string(),
            exit_code,
            duration_ms,
            log: Some(self.rel.clone()),
            cached: false,
        })
    }
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
    let _ = crate::process::command("sh")
        .arg("-c")
        .arg(format!("kill -KILL -- -{pid}"))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    #[cfg(windows)]
    let _ = crate::process::command("taskkill")
        .args(["/T", "/F", "/PID", &pid])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    let _ = child.kill();
    let _ = child.wait();
}
