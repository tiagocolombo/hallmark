//! Command-line definition and dispatch.

use clap::{Args, Parser, Subcommand, ValueEnum};

use crate::commands;
use crate::config::CaptureMode;
use crate::error::{ErrorCode, HallError, Result};
use crate::hooks::HookEvent;
use crate::index::UsageBy;
use crate::output::{self, Mode, Response};

#[derive(Debug, Parser)]
#[command(
    name = "hall",
    version,
    about = "Hallmark: agentic development · automated review · a full record",
    long_about = "Hallmark runs a local, agent-first development loop. Agents are its main \
users: output is JSON whenever stdout isn't a terminal, every reply lists `next` commands, and \
errors are {\"error\":{\"code\",\"message\",\"next\"}} with documented exit codes. Run \
`hall help --json` for the full contract.",
    disable_help_subcommand = true
)]
pub struct Cli {
    /// Print JSON even on a terminal.
    #[arg(long, global = true)]
    pub json: bool,

    #[command(subcommand)]
    pub cmd: Option<Cmd>,
}

#[derive(Debug, Subcommand)]
pub enum Cmd {
    /// Set up this repository: .hallmark.toml, journal, CLAUDE.md section.
    Init(InitArgs),
    /// Check the install and name the fix for each problem.
    Doctor,
    /// Create an item and its worktree.
    New(NewArgs),
    /// Turn the current branch and worktree into an item.
    Adopt(AdoptArgs),
    /// Items by state, or one item in detail.
    Status(StatusArgs),
    /// An item's journal.
    Log(LogArgs),
    /// Everything waiting for you, oldest first.
    Inbox,
    /// Freeze the commit and send the item to review.
    Ready(ReadyArgs),
    /// Stop review and return the item to development.
    PullBack(PullBackArgs),
    /// Abandon an item, keeping its journal and branch.
    Drop(DropArgs),
    /// Allow another fix round for an escalated item.
    Retry(RetryArgs),
    /// The dev agent says the work looks done (moves it to Awaiting you).
    ProposeReady(ProposeReadyArgs),
    /// Ask the human a question; it lands in the inbox.
    Ask(AskArgs),
    /// Answer a question from the inbox.
    Answer(AnswerArgs),
    /// Tokens and list-price cost, grouped.
    Usage(UsageArgs),
    /// Rebuild the SQLite index from the journal.
    Reindex,
    /// Commands, arguments, exit codes and error codes.
    Help(HelpArgs),
    /// The JSON Schema of a command's output (or error, event, config).
    Schema(SchemaArgs),
    /// Entry point for Claude Code hooks; reads the hook JSON on stdin.
    Hook(HookArgs),
    /// One-time machine setup.
    Setup(SetupArgs),
}

#[derive(Debug, Args)]
pub struct InitArgs {
    /// How the edit gate treats edits outside an item's worktree.
    #[arg(long, value_enum)]
    pub capture: Option<CaptureMode>,
    /// The base branch (detected when omitted).
    #[arg(long)]
    pub base: Option<String>,
    /// Don't add the Hallmark section to CLAUDE.md or AGENTS.md.
    #[arg(long)]
    pub no_claude_md: bool,
}

#[derive(Debug, Args)]
pub struct NewArgs {
    /// The request, in your words. Becomes the first journal entry.
    pub request: Option<String>,
    /// A short title (defaults to the request or the issue title).
    #[arg(long)]
    pub title: Option<String>,
    /// Start from a GitHub issue (uses gh).
    #[arg(long)]
    pub issue: Option<u64>,
    /// Open Claude Code in the new worktree (on a terminal).
    #[arg(long)]
    pub open: bool,
    /// Override the branch name.
    #[arg(long)]
    pub branch: Option<String>,
}

#[derive(Debug, Args)]
pub struct AdoptArgs {
    #[arg(long)]
    pub title: Option<String>,
}

#[derive(Debug, Args)]
pub struct StatusArgs {
    /// An item ID; omit to list items (or inside a worktree, its item).
    pub id: Option<String>,
    /// Include closed and dropped items.
    #[arg(long)]
    pub all: bool,
}

#[derive(Debug, Args)]
pub struct LogArgs {
    pub id: Option<String>,
    /// Only these events (comma-separated): prompt, commit, ready, verdict…
    /// `review` means verdict.
    #[arg(long, value_delimiter = ',')]
    pub kind: Vec<String>,
    /// Only the last N events.
    #[arg(long)]
    pub limit: Option<usize>,
}

#[derive(Debug, Args)]
pub struct ReadyArgs {
    pub id: Option<String>,
    /// A note for the reviewer and the journal.
    #[arg(long)]
    pub note: Option<String>,
}

#[derive(Debug, Args)]
pub struct PullBackArgs {
    pub id: Option<String>,
    #[arg(long)]
    pub reason: Option<String>,
}

#[derive(Debug, Args)]
pub struct DropArgs {
    pub id: Option<String>,
    /// Why it's dropped. Required.
    #[arg(long)]
    pub reason: String,
    /// Keep the worktree on disk.
    #[arg(long)]
    pub keep_worktree: bool,
}

#[derive(Debug, Args)]
pub struct RetryArgs {
    pub id: Option<String>,
    #[arg(long)]
    pub reason: String,
}

#[derive(Debug, Args)]
pub struct ProposeReadyArgs {
    pub id: Option<String>,
    /// What was done, for the human deciding.
    #[arg(long)]
    pub summary: String,
}

#[derive(Debug, Args)]
pub struct AskArgs {
    pub question: String,
    /// The item it's about (defaults to the current worktree's item).
    #[arg(long)]
    pub item: Option<String>,
    /// Allowed answers, comma-separated. Answer with the text or a letter.
    #[arg(long, value_delimiter = ',')]
    pub options: Vec<String>,
    /// Used if nobody answers within --wait.
    #[arg(long)]
    pub default: Option<String>,
    /// How long to wait for an answer, such as 4h.
    #[arg(long)]
    pub wait: Option<String>,
    /// The Claude Code session to resume with the answer.
    #[arg(long)]
    pub session: Option<String>,
}

#[derive(Debug, Args)]
pub struct AnswerArgs {
    /// The question ID, such as Q-12.
    pub qid: String,
    /// An option's text or letter (a, b, …), or free text.
    pub answer: String,
}

#[derive(Debug, Args)]
pub struct UsageArgs {
    #[arg(long, value_enum, default_value = "room")]
    pub by: UsageBy,
    /// Only usage newer than this, such as 7d or 24h.
    #[arg(long)]
    pub since: Option<String>,
    #[arg(long)]
    pub item: Option<String>,
}

#[derive(Debug, Args)]
pub struct HelpArgs {
    /// Help for one command.
    pub command: Option<String>,
}

#[derive(Debug, Args)]
pub struct SchemaArgs {
    /// A command name, or error, event, config, item.
    pub name: Option<String>,
}

#[derive(Debug, Args)]
pub struct HookArgs {
    #[arg(value_enum)]
    pub event: HookEvent,
}

#[derive(Debug, Args)]
pub struct SetupArgs {
    #[arg(value_enum)]
    pub target: SetupTarget,
    /// Print what would be done without doing it.
    #[arg(long)]
    pub dry_run: bool,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum SetupTarget {
    /// Install the Hallmark plugin for Claude Code (user level).
    Claude,
}

/// Parses arguments, runs the command and returns the exit code.
pub fn run() -> i32 {
    let args: Vec<String> = std::env::args().collect();
    let force_json = args.iter().skip(1).any(|a| a == "--json");
    let mode = Mode::detect(force_json);
    let cli = match Cli::try_parse_from(&args) {
        Ok(c) => c,
        Err(e) => {
            use clap::error::ErrorKind;
            if matches!(e.kind(), ErrorKind::DisplayHelp | ErrorKind::DisplayVersion) {
                let _ = e.print();
                return 0;
            }
            if mode == Mode::Text {
                let _ = e.print();
                return 2;
            }
            let msg = e.to_string();
            let first = msg
                .lines()
                .next()
                .unwrap_or("bad arguments")
                .trim_start_matches("error: ");
            let err = HallError::new(ErrorCode::Usage, first.to_string())
                .next(["hall help --json", "hall help <command>"]);
            output::print_error(mode, &err);
            return 2;
        }
    };
    let cwd = match std::env::current_dir() {
        Ok(c) => c,
        Err(e) => {
            output::print_error(mode, &HallError::internal(e.to_string()));
            return 1;
        }
    };
    let is_hook = matches!(cli.cmd, Some(Cmd::Hook(_)));
    match dispatch(cli.cmd, &cwd, mode) {
        Ok(resp) => {
            resp.print(mode);
            resp.exit
        }
        // Hooks never fail Claude Code: errors are logged and swallowed.
        Err(_) if is_hook => 0,
        Err(e) => {
            output::print_error(mode, &e);
            e.code.exit_code()
        }
    }
}

fn dispatch(cmd: Option<Cmd>, cwd: &std::path::Path, mode: Mode) -> Result<Response> {
    use commands::*;
    match cmd {
        None => status::status(cwd, None, false, true),
        Some(Cmd::Init(a)) => init::init(cwd, a),
        Some(Cmd::Doctor) => doctor::doctor(cwd),
        Some(Cmd::New(a)) => new::new(cwd, a, mode),
        Some(Cmd::Adopt(a)) => new::adopt(cwd, a),
        Some(Cmd::Status(a)) => status::status(cwd, a.id.as_deref(), a.all, false),
        Some(Cmd::Log(a)) => status::log(cwd, a),
        Some(Cmd::Inbox) => status::inbox(cwd),
        Some(Cmd::Ready(a)) => flow::ready(cwd, a),
        Some(Cmd::PullBack(a)) => flow::pull_back(cwd, a),
        Some(Cmd::Drop(a)) => flow::drop(cwd, a),
        Some(Cmd::Retry(a)) => flow::retry(cwd, a),
        Some(Cmd::ProposeReady(a)) => flow::propose_ready(cwd, a),
        Some(Cmd::Ask(a)) => ask::ask(cwd, a),
        Some(Cmd::Answer(a)) => ask::answer(cwd, a),
        Some(Cmd::Usage(a)) => status::usage(cwd, a),
        Some(Cmd::Reindex) => status::reindex(cwd),
        Some(Cmd::Help(a)) => meta::help(a, mode),
        Some(Cmd::Schema(a)) => meta::schema(a),
        Some(Cmd::Hook(a)) => crate::hooks::run(a.event, cwd),
        Some(Cmd::Setup(a)) => setup::setup(a),
    }
}
