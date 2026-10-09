//! Self-description: `hall help --json` and `hall schema <name>`.

use clap::CommandFactory;
use schemars::{JsonSchema, schema_for};
use serde::Serialize;

use super::ask::{AnswerOut, AskOut};
use super::budget::BudgetOut;
use super::daemon::DaemonOut;
use super::doctor::DoctorOut;
use super::fix::FixedOut;
use super::flow::FlowOut;
use super::init::InitOut;
use super::library::LibraryOut;
use super::merge::MergeOut;
use super::new::NewOut;
use super::next::NextOut;
use super::notify::{NotifyOut, StatuslineOut};
use super::record::{ExportOut, HistoryOut};
use super::review::{FindingsOut, VerdictOut};
use super::setup::SetupOut;
use super::status::{InboxOut, LogOut, ReindexOut, StatusItem, StatusList, UsageOut};
use crate::cli::{Cli, HelpArgs, SchemaArgs};
use crate::config::Config;
use crate::error::{EXIT_CODES, ErrorCode, ErrorEnvelope, HallError, Result};
use crate::event::Event;
use crate::hooks::HookOutput;
use crate::item::Item;
use crate::output::{Mode, Out, Render, Response};

#[derive(Debug, Serialize, JsonSchema)]
pub struct HelpOut {
    pub name: String,
    pub version: String,
    pub about: String,
    pub contract: Vec<String>,
    pub exit_codes: Vec<ExitCodeHelp>,
    pub error_codes: Vec<ErrorCodeHelp>,
    pub commands: Vec<CommandHelp>,
    #[serde(skip)]
    #[schemars(skip)]
    text: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ExitCodeHelp {
    pub code: i32,
    pub meaning: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ErrorCodeHelp {
    pub code: ErrorCode,
    pub exit: i32,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct CommandHelp {
    pub name: String,
    pub about: String,
    pub usage: String,
    pub args: Vec<ArgHelp>,
    /// The command that prints this command's output schema.
    pub schema: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ArgHelp {
    pub name: String,
    /// `--flag` or `<positional>`.
    pub syntax: String,
    pub required: bool,
    pub takes_value: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub help: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub values: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
}

impl Render for HelpOut {
    fn render(&self) -> String {
        self.text.clone()
    }
}

pub fn contract() -> Vec<String> {
    vec![
        "Output is JSON when stdout is not a terminal; --json forces it.".into(),
        "Every reply has `next`: the commands that make sense now.".into(),
        "Errors are {\"error\":{\"code\",\"message\",\"next\"}} with a documented exit code."
            .into(),
        "Commands never prompt; destructive ones need explicit flags such as --reason.".into(),
        "Commands are idempotent: repeating one returns the current result.".into(),
        "Inside an item's worktree the item ID is optional.".into(),
        "`hall schema <command>` prints the JSON Schema of a command's output.".into(),
    ]
}

pub fn help(a: HelpArgs, _mode: Mode) -> Result<Response> {
    let mut root = Cli::command();
    root.build();
    let mut commands = Vec::new();
    // Top-level commands, and one level of nested ones (`daemon start`).
    let mut all: Vec<(String, String, &clap::Command)> = Vec::new();
    for sc in root.get_subcommands() {
        let name = sc.get_name().to_string();
        all.push((name.clone(), name.clone(), sc));
        for sub in sc.get_subcommands() {
            all.push((format!("{name} {}", sub.get_name()), name.clone(), sub));
        }
    }
    for (name, group, sc) in all {
        if let Some(only) = &a.command
            && *only != name
            && *only != group
        {
            continue;
        }
        let args = sc
            .get_arguments()
            .filter(|x| !matches!(x.get_id().as_str(), "help" | "version" | "json"))
            .map(|x| {
                let takes_value = x.get_action().takes_values();
                let syntax = if x.is_positional() {
                    format!("<{}>", x.get_id())
                } else {
                    let long = x.get_long().map(|l| format!("--{l}")).unwrap_or_default();
                    if takes_value {
                        format!("{long} <{}>", x.get_id())
                    } else {
                        long
                    }
                };
                ArgHelp {
                    name: x.get_id().to_string(),
                    syntax,
                    required: x.is_required_set(),
                    takes_value,
                    help: x.get_help().map(|h| h.to_string()),
                    values: x
                        .get_possible_values()
                        .iter()
                        .map(|v| v.get_name().to_string())
                        .collect(),
                    default: x
                        .get_default_values()
                        .first()
                        .map(|d| d.to_string_lossy().to_string()),
                }
            })
            .collect();
        let usage = sc
            .clone()
            .render_usage()
            .to_string()
            .trim_start_matches("Usage: ")
            .to_string();
        // Nested usage renders as `hall-daemon start`; show `hall daemon start`.
        let usage = if name != group {
            usage.replacen(&format!("{group} "), "", 1).replacen(
                "hall ",
                &format!("hall {group} "),
                1,
            )
        } else {
            usage
        };
        commands.push(CommandHelp {
            usage,
            about: sc.get_about().map(|s| s.to_string()).unwrap_or_default(),
            schema: format!("hall schema {group}"),
            name,
            args,
        });
    }
    if let Some(only) = &a.command
        && commands.is_empty()
    {
        return Err(HallError::usage(format!("no command `{only}`")));
    }
    let text = match &a.command {
        Some(c) => root
            .find_subcommand_mut(c)
            .map(|s| s.render_long_help().to_string())
            .unwrap_or_default(),
        None => root.render_long_help().to_string(),
    };
    let out = HelpOut {
        name: "hall".into(),
        version: env!("CARGO_PKG_VERSION").into(),
        about: root.get_about().map(|s| s.to_string()).unwrap_or_default(),
        contract: contract(),
        exit_codes: EXIT_CODES
            .iter()
            .map(|(c, m)| ExitCodeHelp {
                code: *c,
                meaning: m.to_string(),
            })
            .collect(),
        error_codes: ErrorCode::ALL
            .iter()
            .map(|c| ErrorCodeHelp {
                code: *c,
                exit: c.exit_code(),
            })
            .collect(),
        commands,
        text,
    };
    Ok(Response::new(
        out,
        vec!["hall schema <command>".into(), "hall status".into()],
    ))
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct SchemaOut {
    pub name: String,
    pub schema: serde_json::Value,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub available: Vec<String>,
}

impl Render for SchemaOut {
    fn render(&self) -> String {
        serde_json::to_string_pretty(&self.schema).unwrap_or_default()
    }
}

#[derive(Serialize, JsonSchema)]
#[serde(untagged)]
#[allow(dead_code, clippy::large_enum_variant)]
enum StatusReply {
    List(Out<StatusList>),
    Item(Out<StatusItem>),
}

pub const SCHEMA_NAMES: &[&str] = &[
    "init",
    "doctor",
    "new",
    "adopt",
    "status",
    "log",
    "inbox",
    "ready",
    "pull-back",
    "drop",
    "retry",
    "propose-ready",
    "ask",
    "answer",
    "usage",
    "reindex",
    "help",
    "schema",
    "hook",
    "setup",
    "daemon",
    "next",
    "findings",
    "fixed",
    "verdict",
    "merge",
    "history",
    "export",
    "budget",
    "notify",
    "statusline",
    "library",
    "error",
    "event",
    "config",
    "item",
    "verdict-input",
];

pub fn schema_value(name: &str) -> Option<serde_json::Value> {
    let s = match name {
        "init" => schema_for!(Out<InitOut>),
        "doctor" => schema_for!(Out<DoctorOut>),
        "new" | "adopt" => schema_for!(Out<NewOut>),
        "status" => schema_for!(StatusReply),
        "log" => schema_for!(Out<LogOut>),
        "inbox" => schema_for!(Out<InboxOut>),
        "ready" | "pull-back" | "drop" | "retry" | "propose-ready" => schema_for!(Out<FlowOut>),
        "ask" => schema_for!(Out<AskOut>),
        "answer" => schema_for!(Out<AnswerOut>),
        "usage" => schema_for!(Out<UsageOut>),
        "reindex" => schema_for!(Out<ReindexOut>),
        "help" => schema_for!(Out<HelpOut>),
        "schema" => schema_for!(Out<SchemaOut>),
        "setup" => schema_for!(Out<SetupOut>),
        "hook" => schema_for!(HookOutput),
        "daemon" => schema_for!(Out<DaemonOut>),
        "next" => schema_for!(Out<NextOut>),
        "findings" => schema_for!(Out<FindingsOut>),
        "fixed" => schema_for!(Out<FixedOut>),
        "verdict" => schema_for!(Out<VerdictOut>),
        "merge" => schema_for!(Out<MergeOut>),
        "history" => schema_for!(Out<HistoryOut>),
        "export" => schema_for!(Out<ExportOut>),
        "budget" => schema_for!(Out<BudgetOut>),
        "notify" => schema_for!(Out<NotifyOut>),
        "statusline" => schema_for!(StatuslineOut),
        "library" => schema_for!(Out<LibraryOut>),
        // What the principal engineer returns (plugin/schemas/verdict.json).
        "verdict-input" => schema_for!(crate::findings::ModelVerdict),
        "error" => schema_for!(ErrorEnvelope),
        "event" => schema_for!(Event),
        "config" => schema_for!(Config),
        "item" => schema_for!(Item),
        _ => return None,
    };
    serde_json::to_value(s).ok()
}

pub fn schema(a: SchemaArgs) -> Result<Response> {
    let available: Vec<String> = SCHEMA_NAMES.iter().map(|s| s.to_string()).collect();
    let Some(name) = a.name else {
        let out = SchemaOut {
            name: String::new(),
            schema: serde_json::Value::Null,
            available,
        };
        return Ok(Response::new(out, vec!["hall schema status".into()]));
    };
    let schema = schema_value(&name).ok_or_else(|| {
        HallError::usage(format!(
            "no schema named `{name}`; try one of: {}",
            SCHEMA_NAMES.join(", ")
        ))
    })?;
    let out = SchemaOut {
        name,
        schema,
        available: vec![],
    };
    Ok(Response::new(out, vec!["hall help --json".into()]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_schema_builds() {
        for n in SCHEMA_NAMES {
            let v = schema_value(n).unwrap_or_else(|| panic!("{n}"));
            assert!(v.is_object(), "{n}");
        }
    }

    #[test]
    fn every_command_has_a_schema() {
        let root = Cli::command();
        for sc in root.get_subcommands() {
            assert!(schema_value(sc.get_name()).is_some(), "{}", sc.get_name());
        }
    }

    #[test]
    fn command_outputs_include_next() {
        let v = schema_value("ready").unwrap();
        let req = v["required"].as_array().unwrap();
        assert!(req.iter().any(|r| r == "next"), "{v}");
    }
}
