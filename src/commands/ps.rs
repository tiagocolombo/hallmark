//! `hall ps` and `hall kill`: the halls that keep running, and stopping
//! them (ADR 0072). `hall -l` is `hall ps`; `hall web -l` and `hall web
//! --kill` are the same, for the web app only.

use schemars::JsonSchema;
use serde::Serialize;

use crate::cli::{KillArgs, PsArgs};
use crate::error::{HallError, Result};
use crate::instances::{self, Instance, Kind, Stopped};
use crate::output::{Render, Response};

#[derive(Debug, Serialize, JsonSchema)]
pub struct PsOut {
    /// The kind asked for, when one was.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<Kind>,
    /// The running halls, oldest first.
    pub instances: Vec<Instance>,
}

impl Render for PsOut {
    fn render(&self) -> String {
        let what = self.kind.map_or("hall processes", |k| match k {
            Kind::Web => "hall web servers",
            Kind::Daemon => "daemons",
            Kind::Watch => "hall watch processes",
            Kind::Trace => "hall trace --follow processes",
            Kind::App => "terminal apps",
        });
        if self.instances.is_empty() {
            return format!("No {what} running.");
        }
        let mut s = format!("Running {what}:\n");
        for i in &self.instances {
            let since = i
                .started_at
                .map(|t| {
                    t.with_timezone(&chrono::Local)
                        .format(" since %Y-%m-%d %H:%M")
                        .to_string()
                })
                .unwrap_or_default();
            s.push_str(&format!(
                "  {:>7}  {:<6} hall {}{since}\n",
                i.pid,
                i.kind.as_str(),
                i.args.join(" ")
            ));
            if let Some(url) = &i.url {
                s.push_str(&format!("           {url}\n"));
            } else if !i.cwd.is_empty() {
                s.push_str(&format!("           in {}\n", i.cwd));
            }
        }
        s
    }
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct KillOut {
    /// What happened to each process it was asked to stop.
    pub stopped: Vec<Stopped>,
    /// Whether anything was stopped.
    pub changed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

impl Render for KillOut {
    fn render(&self) -> String {
        let mut s = String::new();
        for st in &self.stopped {
            s.push_str(&format!(
                "{} {} (pid {}){}\n",
                if st.stopped {
                    "Stopped"
                } else {
                    "Couldn't stop"
                },
                st.kind.as_str(),
                st.pid,
                st.note
                    .as_deref()
                    .map(|n| format!(": {n}"))
                    .unwrap_or_default()
            ));
        }
        if let Some(n) = &self.note {
            s.push_str(n);
            s.push('\n');
        }
        s
    }
}

fn ps_next(out: &PsOut) -> Vec<String> {
    let mut next = Vec::new();
    if let Some(i) = out.instances.first() {
        next.push(format!("hall kill {}", i.pid));
        next.push(match out.kind {
            Some(k) => format!("hall kill --kind {}", k.as_str()),
            None => "hall kill --all".into(),
        });
    } else if out.kind.is_none_or(|k| k == Kind::Web) {
        next.push("hall web -d".into());
    }
    next
}

pub fn ps(a: PsArgs) -> Result<Response> {
    let out = PsOut {
        kind: a.kind,
        instances: instances::list(a.kind),
    };
    let next = ps_next(&out);
    Ok(Response::new(out, next))
}

pub fn kill(a: KillArgs) -> Result<Response> {
    if a.pids.is_empty() && !a.all && a.kind.is_none() {
        return Err(HallError::usage(
            "name the processes to stop: pids from `hall ps`, --kind <kind> or --all",
        )
        .next(["hall ps"]));
    }
    let running = instances::list(a.kind);
    let (targets, missing): (Vec<&Instance>, Vec<u32>) = if a.pids.is_empty() {
        (running.iter().collect(), vec![])
    } else {
        let t = running.iter().filter(|i| a.pids.contains(&i.pid)).collect();
        let m = a
            .pids
            .iter()
            .copied()
            .filter(|p| !running.iter().any(|i| i.pid == *p))
            .collect();
        (t, m)
    };
    let stopped: Vec<Stopped> = targets.into_iter().map(instances::stop).collect();
    let changed = stopped.iter().any(|s| s.stopped);
    let note = if !missing.is_empty() {
        let pids: Vec<String> = missing.iter().map(u32::to_string).collect();
        Some(format!(
            "no running hall{} with pid {}: `hall ps` lists them",
            a.kind
                .map(|k| format!(" {}", k.as_str()))
                .unwrap_or_default(),
            pids.join(", ")
        ))
    } else if stopped.is_empty() {
        Some(format!(
            "no {} running",
            a.kind.map_or("hall processes".to_string(), |k| format!(
                "hall {}",
                k.as_str()
            ))
        ))
    } else {
        None
    };
    let left = stopped.iter().any(|s| !s.stopped);
    let next = if left {
        vec!["hall ps".into()]
    } else {
        vec!["hall ps".into(), "hall web -d".into()]
    };
    Ok(Response::new(
        KillOut {
            stopped,
            changed,
            note,
        },
        next,
    ))
}

/// `hall web -l`.
pub fn ps_web() -> Result<Response> {
    ps(PsArgs {
        kind: Some(Kind::Web),
    })
}

/// `hall web --kill [<pid>]`.
pub fn kill_web(pid: Option<u32>) -> Result<Response> {
    kill(KillArgs {
        pids: pid.into_iter().collect(),
        all: false,
        kind: Some(Kind::Web),
    })
}
