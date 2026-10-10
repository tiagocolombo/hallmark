//! Errors are data: a stable code, a message, the commands that make sense
//! next, and a documented exit code.

use schemars::JsonSchema;
use serde::Serialize;

/// Stable error codes. The exit code is derived from the code, so agents can
/// branch on either.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ErrorCode {
    /// Bad arguments.
    Usage,
    /// The current directory is not inside a git repository.
    NotARepo,
    /// The repository has not run `hall init`.
    NotInitialized,
    /// No item with that ID.
    ItemNotFound,
    /// No ID was given and the current directory is not an item's worktree.
    NoItem,
    /// No question with that ID.
    QuestionNotFound,
    /// The action is not allowed in the item's current state.
    WrongState,
    /// The item is in the review loop at a different commit.
    ItemInReview,
    /// The worktree has uncommitted changes.
    DirtyWorktree,
    /// The question was already answered differently.
    AlreadyAnswered,
    /// Denied by the gate.
    GateDenied,
    /// The checks failed.
    ChecksFailed,
    /// Claimed by another agent.
    Claimed,
    /// A parent item still has open child items.
    ChildrenOpen,
    /// Merging a child item into its parent conflicted.
    MergeConflict,
    /// A budget cap stops the work.
    BudgetExceeded,
    /// A verdict doesn't match `plugin/schemas/verdict.json`.
    InvalidVerdict,
    /// A model run failed (timeout, budget, no structured output, error).
    RunFailed,
    /// `gh` is not installed or not logged in.
    GhUnavailable,
    /// A configured command (worktree, git, gh, claude) failed.
    CommandFailed,
    /// A bug or an I/O failure.
    Internal,
}

impl ErrorCode {
    pub const ALL: [ErrorCode; 21] = [
        ErrorCode::Usage,
        ErrorCode::NotARepo,
        ErrorCode::NotInitialized,
        ErrorCode::ItemNotFound,
        ErrorCode::NoItem,
        ErrorCode::QuestionNotFound,
        ErrorCode::WrongState,
        ErrorCode::ItemInReview,
        ErrorCode::DirtyWorktree,
        ErrorCode::AlreadyAnswered,
        ErrorCode::GateDenied,
        ErrorCode::ChecksFailed,
        ErrorCode::Claimed,
        ErrorCode::ChildrenOpen,
        ErrorCode::MergeConflict,
        ErrorCode::BudgetExceeded,
        ErrorCode::InvalidVerdict,
        ErrorCode::RunFailed,
        ErrorCode::GhUnavailable,
        ErrorCode::CommandFailed,
        ErrorCode::Internal,
    ];

    /// The documented process exit code for this error.
    pub fn exit_code(self) -> i32 {
        match self {
            ErrorCode::Internal
            | ErrorCode::CommandFailed
            | ErrorCode::GhUnavailable
            | ErrorCode::RunFailed => 1,
            ErrorCode::Usage | ErrorCode::InvalidVerdict => 2,
            ErrorCode::NotARepo
            | ErrorCode::NotInitialized
            | ErrorCode::ItemNotFound
            | ErrorCode::NoItem
            | ErrorCode::QuestionNotFound => 3,
            ErrorCode::WrongState
            | ErrorCode::ItemInReview
            | ErrorCode::DirtyWorktree
            | ErrorCode::AlreadyAnswered
            | ErrorCode::ChildrenOpen
            | ErrorCode::MergeConflict
            | ErrorCode::BudgetExceeded => 4,
            ErrorCode::GateDenied => 5,
            ErrorCode::ChecksFailed => 6,
            ErrorCode::Claimed => 7,
        }
    }

    pub fn as_str(self) -> &'static str {
        match serde_json::to_value(self) {
            Ok(serde_json::Value::String(s)) => Box::leak(s.into_boxed_str()),
            _ => "INTERNAL",
        }
    }
}

/// Exit codes, as documented in `hall help --json`.
pub const EXIT_CODES: [(i32, &str); 8] = [
    (0, "ok"),
    (1, "internal error or a failed external command"),
    (2, "bad arguments"),
    (3, "not found (repo, item, question)"),
    (4, "not allowed in the item's current state"),
    (5, "denied by the gate"),
    (6, "checks failed"),
    (7, "claimed by another agent"),
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HallError {
    pub code: ErrorCode,
    pub message: String,
    pub next: Vec<String>,
}

pub type Result<T> = std::result::Result<T, HallError>;

impl HallError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        HallError {
            code,
            message: message.into(),
            next: Vec::new(),
        }
    }

    pub fn next<I, S>(mut self, next: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.next = next.into_iter().map(Into::into).collect();
        self
    }

    pub fn internal(message: impl Into<String>) -> Self {
        HallError::new(ErrorCode::Internal, message)
    }

    pub fn usage(message: impl Into<String>) -> Self {
        HallError::new(ErrorCode::Usage, message).next(["hall help --json"])
    }

    pub fn envelope(&self) -> ErrorEnvelope {
        ErrorEnvelope {
            error: ErrorBody {
                code: self.code,
                message: self.message.clone(),
                next: self.next.clone(),
            },
        }
    }
}

impl std::fmt::Display for HallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code.as_str(), self.message)
    }
}

impl std::error::Error for HallError {}

/// `{"error": {"code", "message", "next"}}`
#[derive(Debug, Serialize, JsonSchema)]
pub struct ErrorEnvelope {
    pub error: ErrorBody,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ErrorBody {
    pub code: ErrorCode,
    pub message: String,
    pub next: Vec<String>,
}

macro_rules! internal_from {
    ($($t:ty),*) => {$(
        impl From<$t> for HallError {
            fn from(e: $t) -> Self {
                HallError::internal(e.to_string())
            }
        }
    )*};
}

internal_from!(
    std::io::Error,
    serde_json::Error,
    rusqlite::Error,
    toml::ser::Error
);

impl From<toml::de::Error> for HallError {
    fn from(e: toml::de::Error) -> Self {
        HallError::new(ErrorCode::Usage, format!(".hallmark.toml: {e}"))
            .next(["hall schema config", "hall doctor"])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_code_has_a_documented_exit_code() {
        let documented: Vec<i32> = EXIT_CODES.iter().map(|(c, _)| *c).collect();
        for code in ErrorCode::ALL {
            assert!(documented.contains(&code.exit_code()), "{code:?}");
        }
    }

    #[test]
    fn codes_serialize_screaming() {
        assert_eq!(ErrorCode::ItemInReview.as_str(), "ITEM_IN_REVIEW");
        assert_eq!(ErrorCode::NoItem.as_str(), "NO_ITEM");
    }
}
