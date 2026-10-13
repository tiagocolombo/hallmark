//! Command-line definition and dispatch.

use std::path::PathBuf;

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
    version = env!("HALL_VERSION"),
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
    /// Turn the current branch, or an open PR, into an item; reopens a shipped one.
    Adopt(AdoptArgs),
    /// Items by state, or one item in detail.
    Status(StatusArgs),
    /// An item's journal.
    Log(LogArgs),
    /// Everything waiting for you, oldest first; `hall inbox <n>` explains
    /// one, `--dismiss` hides it and `--clear` hides them all.
    Inbox(InboxArgs),
    /// Follow an item live until it leaves the loop or needs you.
    Watch(WatchArgs),
    /// Freeze the commit and send the item to review.
    Ready(ReadyArgs),
    /// Stop review and return the item to development.
    PullBack(PullBackArgs),
    /// Abandon an item, keeping its journal and branch.
    Drop(DropArgs),
    /// Allow another fix round for an escalated item, or release a held
    /// one. A budget hold also grants the item more tokens; the budget
    /// itself is unchanged.
    Retry(RetryArgs),
    /// Move an item on from wherever it waits: release a hold, retry an
    /// escalation, send it to review, or reopen its Claude session.
    Continue(ContinueArgs),
    /// The dev agent says the work looks done (moves it to Awaiting you).
    ProposeReady(ProposeReadyArgs),
    /// Ask the human a question; it lands in the inbox.
    Ask(AskArgs),
    /// Answer a question from the inbox.
    Answer(AnswerArgs),
    /// Tokens and list-price cost, grouped.
    Usage(UsageArgs),
    /// Rebuild the SQLite index from the journal.
    Reindex(ReindexArgs),
    /// Commands, arguments, exit codes and error codes.
    Help(HelpArgs),
    /// The JSON Schema of a command's output (or error, event, config).
    Schema(SchemaArgs),
    /// Entry point for Claude Code hooks; reads the hook JSON on stdin.
    Hook(HookArgs),
    /// One-time machine setup.
    Setup(SetupArgs),
    /// Read and change settings: this repo's .hallmark.toml, or your user
    /// config (tools, notifications) with --user. `hall config list` shows
    /// every key with its value and where it comes from.
    Config(ConfigArgs),
    /// Install the latest hall from main; does nothing when it's current.
    Update(UpdateArgs),
    /// Run the review loop in the background for every registered repo.
    Daemon(DaemonArgs),
    /// Claim the next piece of work for a role, with a lease (agents).
    Next(NextArgs),
    /// Open findings and failing checks for an item (fixer).
    Findings(FindingsArgs),
    /// Record which findings a commit fixes, or dispute one (fixer).
    Fixed(FixedArgs),
    /// Record a schema-checked verdict for an item in review (assayer).
    Verdict(VerdictArgs),
    /// Merge a parallel part (child item) into its parent's branch.
    Merge(MergeArgs),
    /// Closed and dropped items, and why things were rejected.
    History(HistoryArgs),
    /// An item's whole record as Markdown.
    Export(ExportArgs),
    /// Show or set the budget caps, with today's spend.
    Budget(BudgetArgs),
    /// Notifications: `hall notify test` sends one to every channel.
    Notify(NotifyArgs),
    /// One line for Claude Code's status line (reads its JSON on stdin).
    Statusline,
    /// The Library: learnings from past reviews.
    Library(LibraryArgs),
    /// Skill packs by other authors, with their credits: which of Hallmark's
    /// roles follow them here. `hall extensions enable mattpocock` turns one on.
    Extensions(ExtensionsArgs),
    /// Everything the Claude Code heads-up display shows, in one call.
    Hud(HudArgs),
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
    /// The request, in your words. Becomes the first journal entry. The
    /// Front Desk splits it into parallel parts when it can.
    pub request: Option<String>,
    /// Never split the request into parallel parts.
    #[arg(long)]
    pub no_split: bool,
    /// Make this item a parallel part of another item (merges into it).
    #[arg(long)]
    pub parent: Option<String>,
    /// Start from feedback on a shipped PR (a URL or number).
    #[arg(long)]
    pub follow_up: Option<String>,
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
    /// Take over an open PR (a URL or number), Hallmark's or not: its branch
    /// is checked out in a worktree if none has it, and the item updates the
    /// PR instead of opening one. A PR Hallmark shipped reopens its item.
    #[arg(long)]
    pub pr: Option<String>,
    /// The branch to review against and target, to stack the item on
    /// another PR (default: the PR's base, else the configured base).
    #[arg(long)]
    pub base: Option<String>,
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
pub struct InboxArgs {
    /// One entry to explain: its number in the list, a question ID (Q-3)
    /// or an item ID.
    pub entry: Option<String>,
    /// Dismiss that entry: it stays hidden until it changes. Nothing is
    /// answered, retried or dropped.
    #[arg(long, requires = "entry", conflicts_with = "clear")]
    pub dismiss: bool,
    /// Dismiss every entry shown now.
    #[arg(long, conflicts_with = "entry")]
    pub clear: bool,
    /// Show dismissed entries too.
    #[arg(long)]
    pub all: bool,
    /// List notices instead: what the loop is handling on its own, or news
    /// (stuck items, waiting sessions, CI being fixed, proposed learnings).
    /// They never reach your office and are never notified.
    #[arg(long, conflicts_with_all = ["entry", "clear", "all"])]
    pub notices: bool,
}

#[derive(Debug, Args)]
pub struct WatchArgs {
    pub id: Option<String>,
    /// Stop watching after this long, such as 30m.
    #[arg(long, default_value = "30m")]
    pub timeout: String,
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
    /// For a budget hold: how many more tokens this item may spend, such as
    /// 2M or 800k. Defaults to another `per_item`.
    #[arg(long)]
    pub tokens: Option<String>,
}

#[derive(Debug, Args)]
pub struct ContinueArgs {
    pub id: Option<String>,
    /// Why (recorded when it releases a hold or retries an escalation).
    #[arg(long)]
    pub reason: Option<String>,
}

#[derive(Debug, Args)]
pub struct ProposeReadyArgs {
    pub id: Option<String>,
    /// What was done, for the human deciding. It becomes the What of the PR
    /// description.
    #[arg(long)]
    pub summary: String,
    /// Why the change is needed: the PR description's Why. Without it, the
    /// request (or the issue) is used.
    #[arg(long)]
    pub why: Option<String>,
    /// How it was done: the approach and the notable choices, for the PR
    /// description's How. Without it, the commits are listed.
    #[arg(long)]
    pub how: Option<String>,
    /// Something needs the human's confirmation: wait for their call
    /// instead of going straight to review.
    #[arg(long)]
    pub ask: bool,
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
pub struct ReindexArgs {
    /// Rebuild even when a newer hall owns the index, and take it over.
    #[arg(long)]
    pub force: bool,
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
    /// Only this part. Without one, `hall setup` does everything: the
    /// Claude Code plugin and status line, this repo, the daemon at login,
    /// a phone topic, then `hall doctor`. Safe to repeat; re-run it after
    /// upgrading hall.
    #[arg(value_enum)]
    pub target: Option<SetupTarget>,
    /// Print what would be done without doing it.
    #[arg(long)]
    pub dry_run: bool,
    /// Also set Claude Code's status line to `hall statusline` in
    /// ~/.claude/settings.json.
    #[arg(long)]
    pub statusline: bool,
    /// Replace a status line that isn't Hallmark's.
    #[arg(long)]
    pub force: bool,
    /// `hall setup`: leave Claude Code's status line alone.
    #[arg(long)]
    pub no_statusline: bool,
    /// `hall setup`: don't start the daemon or install its login service.
    #[arg(long)]
    pub no_daemon: bool,
    /// `hall setup`: don't create a phone topic.
    #[arg(long)]
    pub no_notify: bool,
    /// `hall setup`: ask about every setting first: base branch, gates and
    /// the shell they run in, how work is shipped, models, budget, tools
    /// and notifications. Needs a terminal; `hall config` sets the same
    /// keys without questions.
    #[arg(long, short = 'i')]
    pub interactive: bool,
}

#[derive(Debug, Args)]
pub struct ConfigArgs {
    /// What to do; `list` when omitted.
    #[command(subcommand)]
    pub action: Option<ConfigAction>,
}

#[derive(Debug, Subcommand)]
pub enum ConfigAction {
    /// Every key with its value and where it comes from (default, repo,
    /// user, env, path).
    List {
        /// Only keys under this prefix, such as `budget` or `notify.route`.
        prefix: Option<String>,
        /// Your user config instead of the repo's.
        #[arg(long)]
        user: bool,
    },
    /// One key's value.
    Get {
        /// A dotted key, such as `budget.daily`.
        key: String,
        /// Your user config instead of the repo's.
        #[arg(long)]
        user: bool,
    },
    /// Set a key; the file's comments and layout are kept. A list takes
    /// several values, or one TOML array ('["a", "b"]').
    Set {
        /// A dotted key, such as `budget.daily` or `tools.gh`.
        key: String,
        /// The value. Put `--` before values that start with `-`.
        #[arg(required = true, num_args = 1..)]
        value: Vec<String>,
        /// Your user config instead of the repo's (`tools.*` is always
        /// there).
        #[arg(long)]
        user: bool,
    },
    /// Remove a key, so its default applies again.
    Unset {
        /// A dotted key.
        key: String,
        /// Your user config instead of the repo's.
        #[arg(long)]
        user: bool,
    },
}

#[derive(Debug, Args)]
pub struct UpdateArgs {
    /// Only say whether an update is available; install nothing.
    #[arg(long)]
    pub check: bool,
    /// The git repository to install from.
    #[arg(long, env = "HALL_UPDATE_SOURCE", default_value = commands::update::SOURCE)]
    pub source: String,
    /// The branch whose latest commit is installed.
    #[arg(long, default_value = "main")]
    pub branch: String,
    /// Don't refresh the Claude Code plugin or restart the daemon afterwards.
    #[arg(long)]
    pub no_setup: bool,
}

#[derive(Debug, Args)]
pub struct DaemonArgs {
    #[command(subcommand)]
    pub action: DaemonAction,
}

#[derive(Debug, Subcommand)]
pub enum DaemonAction {
    /// Start the daemon in the background.
    Start {
        /// Also start it at login (launchd, systemd, Task Scheduler).
        #[arg(long)]
        at_login: bool,
    },
    /// Stop the daemon.
    Stop {
        /// Also remove the login service.
        #[arg(long)]
        at_login: bool,
    },
    /// Is it running, paused, and what is it doing?
    Status,
    /// Lift a pause now (a rate limit or the daily budget), before its time.
    Resume,
    /// Run the loop in the foreground (what `start` launches).
    Run {
        /// One pass over the repos, then exit.
        #[arg(long)]
        once: bool,
        /// Only these workers (comma-separated): watchman, resume, assayer,
        /// fixer, shipper, delivery, delivery-watch, ci, sweeper, librarian,
        /// notify.
        #[arg(long, value_delimiter = ',')]
        only: Vec<String>,
        /// Only this repository (default: every registered repo; with
        /// --once, the current one).
        #[arg(long)]
        repo: Option<String>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum WorkRole {
    /// Checks and review: items in checks or review.
    Assayer,
    /// Items in fixing.
    Fixer,
    /// Hallmarked items.
    Shipper,
}

impl WorkRole {
    pub fn as_str(self) -> &'static str {
        match self {
            WorkRole::Assayer => "assayer",
            WorkRole::Fixer => "fixer",
            WorkRole::Shipper => "shipper",
        }
    }
}

#[derive(Debug, Args)]
pub struct NextArgs {
    #[arg(long, value_enum)]
    pub role: WorkRole,
    /// Claim this item rather than the next one.
    #[arg(long)]
    pub item: Option<String>,
    /// Who is claiming (default: HALL_ACTOR and the process ID).
    #[arg(long)]
    pub holder: Option<String>,
    /// How long the lease lasts, such as 30m.
    #[arg(long, default_value = "30m")]
    pub ttl: String,
    /// Release the lease on --item instead of claiming.
    #[arg(long, requires = "item")]
    pub release: bool,
}

#[derive(Debug, Args)]
pub struct FindingsArgs {
    pub id: Option<String>,
    /// Include fixed and withdrawn findings.
    #[arg(long)]
    pub all: bool,
}

#[derive(Debug, Args)]
pub struct FixedArgs {
    /// Findings the committed HEAD fixes (F1 F2 …). None when only failing
    /// checks were fixed.
    pub ids: Vec<String>,
    #[arg(long)]
    pub item: Option<String>,
    /// A finding you dispute instead of fixing (repeatable; needs --reason).
    #[arg(long, requires = "reason")]
    pub dispute: Vec<String>,
    /// Why the disputed findings are wrong.
    #[arg(long)]
    pub reason: Option<String>,
}

#[derive(Debug, Args)]
pub struct VerdictArgs {
    pub id: Option<String>,
    /// The verdict JSON (plugin/schemas/verdict.json); `-` reads stdin.
    #[arg(long)]
    pub file: String,
    /// The reviewed commit (default: the item's frozen commit).
    #[arg(long)]
    pub sha: Option<String>,
}

#[derive(Debug, Args)]
pub struct MergeArgs {
    /// The child item (default: the current worktree's item).
    pub id: Option<String>,
    /// Keep the child's worktree on disk.
    #[arg(long)]
    pub keep_worktree: bool,
}

#[derive(Debug, Args)]
pub struct HistoryArgs {
    /// Only items with at least one rejected review.
    #[arg(long)]
    pub rejected: bool,
    /// Only findings under this lens, such as security or correctness.
    #[arg(long)]
    pub lens: Option<String>,
    /// Only items that ended within this long, such as 7d.
    #[arg(long)]
    pub since: Option<String>,
    /// Include items that are still open.
    #[arg(long)]
    pub all: bool,
    #[arg(long)]
    pub limit: Option<usize>,
}

#[derive(Debug, Args)]
pub struct ExportArgs {
    pub id: Option<String>,
    /// Write to this file instead of the item's journal directory.
    #[arg(long)]
    pub out: Option<String>,
}

#[derive(Debug, Args)]
pub struct BudgetArgs {
    /// Set a cap in .hallmark.toml, such as daily=4M, per_item=800k,
    /// review.max_usd=3, review.daily=2M (repeatable).
    #[arg(long, num_args = 1..)]
    pub set: Vec<String>,
    /// Let the loop go past the daily budget today without changing it:
    /// today's cap becomes `daily` plus this many tokens (default: another
    /// `daily`), and a daily-budget pause lifts now. Repeating it with the
    /// same amount changes nothing; it ends at 00:00 UTC.
    #[arg(long, num_args = 0..=1, default_missing_value = "daily", value_name = "TOKENS")]
    pub grant: Option<String>,
    /// Why, for the journal (with --grant).
    #[arg(long, requires = "grant")]
    pub reason: Option<String>,
}

#[derive(Debug, Args)]
pub struct HudArgs {
    /// The Claude Code session, for its item when the directory has none.
    #[arg(long)]
    pub session: Option<String>,
    /// Lines of the daemon's Live log to include.
    #[arg(long, default_value_t = 6)]
    pub lines: usize,
}

#[derive(Debug, Args)]
pub struct NotifyArgs {
    #[command(subcommand)]
    pub action: NotifyAction,
}

#[derive(Debug, Subcommand)]
pub enum NotifyAction {
    /// Send a test notification to every configured channel.
    Test {
        /// Only this channel: desktop, phone, slack.
        #[arg(long)]
        channel: Option<String>,
    },
}

#[derive(Debug, Args)]
pub struct LibraryArgs {
    #[command(subcommand)]
    pub action: LibraryAction,
}

#[derive(Debug, Subcommand)]
pub enum LibraryAction {
    /// Search the repo's learnings.
    Search {
        query: String,
        #[arg(long, default_value_t = 10)]
        limit: usize,
        /// Only this kind: rule, false-positive, map, decision.
        #[arg(long)]
        kind: Option<String>,
        /// Include proposed learnings, not only accepted ones.
        #[arg(long)]
        proposed: bool,
    },
    /// List proposed learnings, or accept and reject them.
    Review {
        /// Learnings to accept (L-0001 …); a retired one is reinstated.
        #[arg(long, value_delimiter = ',')]
        accept: Vec<String>,
        /// Learnings to reject; an update gives back the version it replaced.
        #[arg(long, value_delimiter = ',')]
        reject: Vec<String>,
        #[arg(long)]
        reason: Option<String>,
    },
    /// Retire a learning, keeping its history.
    Retire {
        id: String,
        #[arg(long)]
        reason: String,
    },
    /// Run the librarian on one closed or escalated item now.
    Distill { id: String },
    /// Add learnings to the local Library: committed ones, a directory or an export.
    Import {
        /// A directory of learning files or an export file. Default: `[library] dir`
        /// (.hallmark/knowledge) on the base branch.
        source: Option<PathBuf>,
    },
    /// Write every learning in the local Library to one JSON file.
    Export {
        /// The file to write. Without it, the reply carries the learnings.
        #[arg(long)]
        out: Option<PathBuf>,
    },
}

#[derive(Debug, Args)]
pub struct ExtensionsArgs {
    #[command(subcommand)]
    pub action: Option<ExtensionsAction>,
}

#[derive(Debug, Subcommand)]
pub enum ExtensionsAction {
    /// Every extension, its credits, and the roles it serves here (the default).
    List,
    /// Turn an extension on for every role it serves, or only for --role.
    Enable {
        /// The extension: `hall extensions` lists them.
        name: String,
        /// Only this role (repeatable): planner, developer, reviewer, fixer.
        #[arg(long = "role", value_name = "ROLE", value_enum)]
        roles: Vec<crate::extensions::Role>,
    },
    /// Turn an extension off for every role, or only for --role.
    Disable {
        /// The extension.
        name: String,
        /// Only this role (repeatable).
        #[arg(long = "role", value_name = "ROLE", value_enum)]
        roles: Vec<crate::extensions::Role>,
    },
    /// An extension's credits and license, or one of its skills as agents read it.
    Show {
        /// The extension.
        name: String,
        /// The skill to print.
        skill: Option<String>,
    },
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
    let is_hook = matches!(cli.cmd, Some(Cmd::Hook(_) | Cmd::Statusline));
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

/// Runs `hall <args>` in this process for the terminal app, which shows the
/// reply. Only commands that return at once and never take the terminal
/// are allowed: the item flow, answers, the readers and the daemon's
/// start, stop and status. Anything else is `USAGE`.
pub fn run_in_app(args: &[String], cwd: &std::path::Path) -> Result<Response> {
    let cli = Cli::try_parse_from(std::iter::once("hall".to_string()).chain(args.iter().cloned()))
        .map_err(|e| {
            let msg = e.to_string();
            HallError::usage(
                msg.lines()
                    .next()
                    .unwrap_or("bad arguments")
                    .trim_start_matches("error: ")
                    .to_string(),
            )
        })?;
    let allowed = match &cli.cmd {
        Some(
            Cmd::Ready(_)
            | Cmd::PullBack(_)
            | Cmd::Drop(_)
            | Cmd::Retry(_)
            | Cmd::Answer(_)
            | Cmd::Merge(_)
            | Cmd::Status(_)
            | Cmd::Log(_)
            | Cmd::Inbox(_)
            | Cmd::Findings(_)
            | Cmd::History(_)
            | Cmd::Export(_)
            | Cmd::Usage(_),
        ) => true,
        Some(Cmd::New(a)) => a.follow_up.is_some() && !a.open,
        Some(Cmd::Daemon(a)) => !matches!(a.action, DaemonAction::Run { .. }),
        _ => false,
    };
    if !allowed {
        return Err(HallError::usage(format!(
            "`hall {}` doesn't run inside the terminal app; run it in a terminal",
            args.join(" ")
        )));
    }
    dispatch(cli.cmd, cwd, Mode::Json)
}

fn dispatch(cmd: Option<Cmd>, cwd: &std::path::Path, mode: Mode) -> Result<Response> {
    use commands::*;
    match cmd {
        None if mode == Mode::Text => crate::tui::run(cwd),
        None => status::status(cwd, None, false, true),
        Some(Cmd::Init(a)) => init::init(cwd, a),
        Some(Cmd::Doctor) => doctor::doctor(cwd),
        Some(Cmd::New(a)) => new::new(cwd, a, mode),
        Some(Cmd::Adopt(a)) => new::adopt(cwd, a),
        Some(Cmd::Status(a)) => status::status(cwd, a.id.as_deref(), a.all, false),
        Some(Cmd::Log(a)) => status::log(cwd, a),
        Some(Cmd::Inbox(a)) => status::inbox(cwd, a),
        Some(Cmd::Watch(a)) => watch::watch(cwd, a, mode),
        Some(Cmd::Ready(a)) => flow::ready(cwd, a),
        Some(Cmd::PullBack(a)) => flow::pull_back(cwd, a),
        Some(Cmd::Drop(a)) => flow::drop(cwd, a),
        Some(Cmd::Retry(a)) => flow::retry(cwd, a),
        Some(Cmd::Continue(a)) => flow::continue_item(cwd, a, mode),
        Some(Cmd::ProposeReady(a)) => flow::propose_ready(cwd, a),
        Some(Cmd::Ask(a)) => ask::ask(cwd, a),
        Some(Cmd::Answer(a)) => ask::answer(cwd, a),
        Some(Cmd::Usage(a)) => status::usage(cwd, a),
        Some(Cmd::Reindex(a)) => status::reindex(cwd, a.force),
        Some(Cmd::Help(a)) => meta::help(a, mode),
        Some(Cmd::Schema(a)) => meta::schema(a),
        Some(Cmd::Hook(a)) => crate::hooks::run(a.event, cwd),
        Some(Cmd::Setup(a)) => setup::setup(cwd, a, mode),
        Some(Cmd::Config(a)) => config::config(cwd, a),
        Some(Cmd::Update(a)) => update::update(cwd, a),
        Some(Cmd::Daemon(a)) => daemon::daemon(cwd, a),
        Some(Cmd::Next(a)) => next::next(cwd, a),
        Some(Cmd::Findings(a)) => review::findings(cwd, a),
        Some(Cmd::Fixed(a)) => fix::fixed(cwd, a),
        Some(Cmd::Verdict(a)) => review::verdict(cwd, a),
        Some(Cmd::Merge(a)) => merge::merge(cwd, a),
        Some(Cmd::History(a)) => record::history(cwd, a),
        Some(Cmd::Export(a)) => record::export(cwd, a),
        Some(Cmd::Budget(a)) => budget::budget(cwd, a),
        Some(Cmd::Notify(a)) => notify::notify(cwd, a),
        Some(Cmd::Statusline) => notify::statusline(cwd),
        Some(Cmd::Library(a)) => library::library(cwd, a),
        Some(Cmd::Extensions(a)) => extensions::extensions(cwd, a),
        Some(Cmd::Hud(a)) => hud::hud(cwd, a),
    }
}
