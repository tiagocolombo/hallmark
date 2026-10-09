//! `hall next --role <r>`: claim work with a lease (ADR 0014). The daemon
//! uses the same leases, so an agent and the daemon never take one item.

use std::path::Path;

use chrono::Duration;
use schemars::JsonSchema;
use serde::Serialize;

use super::ItemView;
use crate::cli::{NextArgs, WorkRole};
use crate::error::{ErrorCode, HallError, Result};
use crate::lease::{self, Lease};
use crate::output::{Render, Response};
use crate::repo::{Repo, actor};

#[derive(Debug, Serialize, JsonSchema)]
pub struct NextOut {
    pub role: String,
    /// The claimed item, or null when there is no work.
    pub work: Option<Work>,
    /// True when --release removed a lease.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub released: bool,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct Work {
    #[serde(flatten)]
    pub item: ItemView,
    pub lease: Lease,
}

impl Render for NextOut {
    fn render(&self) -> String {
        if self.released {
            return "Released.\n".into();
        }
        match &self.work {
            None => format!("No work for the {}.\n", self.role),
            Some(w) => format!(
                "Claimed {} for the {} until {}.\n\n{}",
                w.item.id,
                self.role,
                w.lease.expires.format("%H:%M:%S"),
                w.item.render()
            ),
        }
    }
}

fn holder(a: &NextArgs) -> String {
    a.holder
        .clone()
        .unwrap_or_else(|| format!("{}:{}", actor(), std::process::id()))
}

pub fn next(cwd: &Path, a: NextArgs) -> Result<Response> {
    let repo = Repo::discover(cwd)?;
    repo.require_init()?;
    let role = a.role.as_str();
    let ttl = crate::config::parse_duration(&a.ttl)
        .map(Duration::seconds)
        .ok_or_else(|| HallError::usage(format!("--ttl {}: use a duration like 30m", a.ttl)))?;
    let who = holder(&a);
    let only = a
        .item
        .as_deref()
        .map(|i| repo.resolve(Some(i)))
        .transpose()?;
    if a.release {
        let id = only.map(|i| i.id).expect("clap requires --item");
        let released = lease::release(&repo.hall, id, &who)?;
        let out = NextOut {
            role: role.into(),
            work: None,
            released,
        };
        return Ok(Response::new(out, vec![format!("hall next --role {role}")]));
    }
    let claimed = lease::claim_next(&repo, role, &who, ttl, only.as_ref().map(|i| i.id))?;
    if claimed.is_none()
        && let Some(i) = &only
        && let Some(l) = lease::current(&repo.hall, i.id)
        && l.holder != who
    {
        return Err(HallError::new(
            ErrorCode::Claimed,
            format!(
                "{} is claimed by {} until {}",
                i.id,
                l.holder,
                l.expires.format("%H:%M:%S")
            ),
        )
        .next([format!("hall status {}", i.id)]));
    }
    let next = match &claimed {
        None => vec!["hall status".to_string(), "hall inbox".into()],
        Some((item, _)) => match a.role {
            WorkRole::Fixer => vec![
                format!("hall findings {}", item.id),
                format!("hall fixed <finding ids> --item {}", item.id),
                format!(
                    "hall next --role fixer --item {} --release --holder {who}",
                    item.id
                ),
            ],
            WorkRole::Assayer => vec![
                format!("hall findings {}", item.id),
                format!("hall verdict {} --file <verdict.json>", item.id),
            ],
            WorkRole::Shipper => vec![format!("hall export {}", item.id)],
        },
    };
    let out = NextOut {
        role: role.into(),
        work: claimed.map(|(item, lease)| Work {
            item: ItemView::from(&item),
            lease,
        }),
        released: false,
    };
    Ok(Response::new(out, next))
}
