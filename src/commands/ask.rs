//! Questions: `hall ask` records one for the inbox, `hall answer` settles it.
//! Stage 1 records only; the daemon will resume the asking session with
//! `claude -p --resume <session> "<answer>"`.

use std::path::Path;

use chrono::{DateTime, Duration, Utc};
use schemars::JsonSchema;
use serde::Serialize;

use crate::cli::{AnswerArgs, AskArgs};
use crate::error::{ErrorCode, HallError, Result};
use crate::event::Body;
use crate::index::QuestionRow;
use crate::item::ItemId;
use crate::journal::ev;
use crate::output::{Render, Response};
use crate::repo::{Repo, actor};

#[derive(Debug, Serialize, JsonSchema)]
pub struct AskOut {
    pub qid: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub item: Option<ItemId>,
    pub text: String,
    pub options: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deadline: Option<DateTime<Utc>>,
    /// Always `waiting`: end your run and let the human answer.
    pub status: String,
    /// False when the same open question already existed.
    pub created: bool,
}

impl Render for AskOut {
    fn render(&self) -> String {
        format!("{} is in the inbox: \"{}\"\n", self.qid, self.text)
    }
}

pub fn ask(cwd: &Path, a: AskArgs) -> Result<Response> {
    let repo = Repo::discover(cwd)?;
    repo.require_init()?;
    if a.question.trim().is_empty() {
        return Err(HallError::usage("the question is empty"));
    }
    let item = match &a.item {
        Some(id) => Some(repo.resolve(Some(id))?.id),
        None => repo.current_item().map(|i| i.id),
    };
    if let Some(d) = &a.default
        && !a.options.is_empty()
        && !a.options.iter().any(|o| o == d)
    {
        return Err(HallError::usage(format!(
            "--default {d} is not one of the options ({})",
            a.options.join(", ")
        )));
    }
    let deadline = match &a.wait {
        Some(w) => {
            let secs = crate::config::parse_duration(w).ok_or_else(|| {
                HallError::new(
                    ErrorCode::Usage,
                    format!("--wait {w}: use a duration like 4h or 30m"),
                )
            })?;
            Some(Utc::now() + Duration::seconds(secs))
        }
        None => None,
    };
    let ix = repo.index()?;
    if let Some(q) = ix.open_question_like(item, a.question.trim())? {
        return Ok(reply(q_out(&q, false)));
    }
    let qid = repo.hall.next_question_id()?;
    let session = a.session.clone().or_else(env_session);
    let e = ev(
        &actor(),
        item,
        Body::Question {
            qid: qid.clone(),
            text: a.question.trim().to_string(),
            options: a.options.clone(),
            default: a.default.clone(),
            wait: a.wait.clone(),
            deadline,
        },
    )
    .with_session(session);
    match item {
        Some(id) => repo.hall.append_item(id, &e)?,
        None => repo.hall.append(&repo.hall.repo_events(), &e)?,
    }
    Ok(reply(AskOut {
        qid,
        item,
        text: a.question.trim().to_string(),
        options: a.options,
        default: a.default,
        deadline,
        status: "waiting".into(),
        created: true,
    }))
}

fn q_out(q: &QuestionRow, created: bool) -> AskOut {
    AskOut {
        qid: q.qid.clone(),
        item: q.item,
        text: q.text.clone(),
        options: q.options.clone(),
        default: q.default.clone(),
        deadline: q.deadline,
        status: if q.answer.is_some() {
            "answered".into()
        } else {
            "waiting".into()
        },
        created,
    }
}

fn reply(out: AskOut) -> Response {
    let next = vec![
        format!("hall answer {} <answer>", out.qid),
        "hall inbox".into(),
    ];
    Response::new(out, next)
}

fn env_session() -> Option<String> {
    ["CLAUDE_SESSION_ID", "CLAUDE_CODE_SESSION_ID"]
        .iter()
        .find_map(|k| std::env::var(k).ok().filter(|v| !v.is_empty()))
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct AnswerOut {
    pub qid: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub item: Option<ItemId>,
    pub question: String,
    pub answer: String,
    /// False when this exact answer was already recorded.
    pub changed: bool,
    /// The command that resumes the asking session (run by the daemon in Stage 2).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resume: Option<String>,
}

impl Render for AnswerOut {
    fn render(&self) -> String {
        let mut s = format!("{}: \"{}\" → {}\n", self.qid, self.question, self.answer);
        if let Some(r) = &self.resume {
            s.push_str(&format!("The asking session resumes with: {r}\n"));
        }
        s
    }
}

pub fn answer(cwd: &Path, a: AnswerArgs) -> Result<Response> {
    let repo = Repo::discover(cwd)?;
    repo.require_init()?;
    let ix = repo.index()?;
    let q = ix.question(&a.qid)?.ok_or_else(|| {
        HallError::new(
            ErrorCode::QuestionNotFound,
            format!("no question {}", a.qid),
        )
        .next(["hall inbox"])
    })?;
    let answer = resolve_answer(&q, &a.answer)?;
    if let Some(prev) = &q.answer {
        if *prev == answer {
            return Ok(answer_reply(&q, answer, false));
        }
        return Err(HallError::new(
            ErrorCode::AlreadyAnswered,
            format!("{} was already answered: \"{prev}\"", q.qid),
        )
        .next(["hall inbox"]));
    }
    let e = ev(
        &actor(),
        q.item,
        Body::Answer {
            qid: q.qid.clone(),
            answer: answer.clone(),
            defaulted: false,
        },
    );
    match q.item {
        Some(id) => repo.hall.append_item(id, &e)?,
        None => repo.hall.append(&repo.hall.repo_events(), &e)?,
    }
    Ok(answer_reply(&q, answer, true))
}

fn answer_reply(q: &QuestionRow, answer: String, changed: bool) -> Response {
    let resume = q.session.as_ref().map(|s| {
        format!(
            "claude -p --resume {s} {}",
            crate::commands::new::shell_quote(&answer)
        )
    });
    let mut next = vec!["hall inbox".to_string()];
    if let Some(id) = q.item {
        next.push(format!("hall status {id}"));
    }
    let out = AnswerOut {
        qid: q.qid.clone(),
        item: q.item,
        question: q.text.clone(),
        answer,
        changed,
        resume,
    };
    Response::new(out, next)
}

/// An option's text (any case) or letter, or free text when there are no
/// options.
fn resolve_answer(q: &QuestionRow, given: &str) -> Result<String> {
    let g = given.trim();
    if q.options.is_empty() {
        if g.is_empty() {
            return Err(HallError::usage("the answer is empty"));
        }
        return Ok(g.to_string());
    }
    if let Some(o) = q.options.iter().find(|o| o.eq_ignore_ascii_case(g)) {
        return Ok(o.clone());
    }
    if g.len() == 1 {
        let c = g.chars().next().unwrap_or(' ').to_ascii_lowercase();
        if c.is_ascii_lowercase()
            && let Some(o) = q.options.get((c as u8 - b'a') as usize)
        {
            return Ok(o.clone());
        }
    }
    Err(
        HallError::usage(format!("answer one of: {}", q.options.join(", "))).next(
            q.options
                .iter()
                .map(|o| format!("hall answer {} {o}", q.qid)),
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q(options: &[&str]) -> QuestionRow {
        QuestionRow {
            qid: "Q-1".into(),
            item: None,
            text: "billed or raw?".into(),
            options: options.iter().map(|s| s.to_string()).collect(),
            default: None,
            wait: None,
            deadline: None,
            asked_at: Utc::now(),
            session: None,
            answer: None,
            answered_at: None,
        }
    }

    #[test]
    fn answers_by_letter_or_text() {
        let q = q(&["billed", "raw"]);
        assert_eq!(resolve_answer(&q, "a").unwrap(), "billed");
        assert_eq!(resolve_answer(&q, "B").unwrap(), "raw");
        assert_eq!(resolve_answer(&q, "Raw").unwrap(), "raw");
        assert!(resolve_answer(&q, "c").is_err());
        assert!(resolve_answer(&q, "maybe").is_err());
    }

    #[test]
    fn free_text_without_options() {
        assert_eq!(
            resolve_answer(&q(&[]), " use the cache ").unwrap(),
            "use the cache"
        );
    }
}
