//! Findings and verdicts (ADR 0015).
//!
//! The principal engineer returns a [`ModelVerdict`], shaped by
//! `plugin/schemas/verdict.json` and enforced with `--json-schema`. Hallmark
//! never trusts the model's opinion of itself: it numbers the findings
//! (`F1`, `F2`, … per item), folds them with earlier rounds, and decides
//! approval from the result: **0 open blockers and 0 open majors**.
//!
//! From round 2 the review is incremental. The reviewer sees the open
//! findings and reports each one's status in `prior`; anything it leaves out
//! stays open.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::error::{ErrorCode, HallError, Result};
use crate::event::{Body, Event};

/// The seven lenses every review applies.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum Lens {
    /// Is this the simplest change, in the right layer?
    FirstPrinciples,
    /// Edge cases, errors, concurrency, idempotency, retries.
    Correctness,
    /// Authorization, isolation, secrets, injection.
    SecurityTenancy,
    /// Reversible and safe on live data?
    DataMigrations,
    /// Do the tests prove it? Did the test count drop?
    Evidence,
    /// Can someone on call see it fail and recover?
    Operability,
    /// CLAUDE.md, AGENTS.md and the rule files they list.
    RepoRules,
}

impl Lens {
    pub const ALL: [Lens; 7] = [
        Lens::FirstPrinciples,
        Lens::Correctness,
        Lens::SecurityTenancy,
        Lens::DataMigrations,
        Lens::Evidence,
        Lens::Operability,
        Lens::RepoRules,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Lens::FirstPrinciples => "first_principles",
            Lens::Correctness => "correctness",
            Lens::SecurityTenancy => "security_tenancy",
            Lens::DataMigrations => "data_migrations",
            Lens::Evidence => "evidence",
            Lens::Operability => "operability",
            Lens::RepoRules => "repo_rules",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Lens::FirstPrinciples => "First principles",
            Lens::Correctness => "Correctness",
            Lens::SecurityTenancy => "Security & tenancy",
            Lens::DataMigrations => "Data & migrations",
            Lens::Evidence => "Evidence",
            Lens::Operability => "Operability",
            Lens::RepoRules => "Repo rules",
        }
    }

    pub fn parse(s: &str) -> Option<Lens> {
        let k = s.trim().to_ascii_lowercase().replace(['-', ' ', '&'], "_");
        let k = k
            .split('_')
            .filter(|p| !p.is_empty())
            .collect::<Vec<_>>()
            .join("_");
        Lens::ALL
            .into_iter()
            .find(|l| l.as_str() == k || l.as_str().split('_').next() == Some(k.as_str()))
    }
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Blocker,
    Major,
    Minor,
    Nit,
}

impl Severity {
    pub const ALL: [Severity; 4] = [
        Severity::Blocker,
        Severity::Major,
        Severity::Minor,
        Severity::Nit,
    ];

    /// Blockers and majors stop approval.
    pub fn blocks(self) -> bool {
        matches!(self, Severity::Blocker | Severity::Major)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Severity::Blocker => "blocker",
            Severity::Major => "major",
            Severity::Minor => "minor",
            Severity::Nit => "nit",
        }
    }
}

/// A finding as the model reports it (no ID yet).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ModelFinding {
    pub lens: Lens,
    pub severity: Severity,
    /// Repo-relative path.
    pub file: String,
    /// 1-based; 0 means the whole file.
    pub line: u32,
    /// One line.
    pub title: String,
    /// The concrete case that breaks: inputs or state, then the wrong result.
    pub scenario: String,
    /// How to fix it.
    pub fix: String,
    /// The problem was there before this change and the change doesn't
    /// make it worse: recorded and shown, never blocking (ADR 0059).
    #[serde(default)]
    pub pre_existing: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PriorStatus {
    /// The new commit fixes it.
    Fixed,
    /// Still broken.
    Open,
    /// The reviewer accepts the fixer's dispute, or was wrong.
    Withdrawn,
}

/// The reviewer's ruling on a finding from an earlier round.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PriorRuling {
    pub id: String,
    pub status: PriorStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// Whether a Library learning from the briefing was used, and whether it held.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LearningUse {
    /// `L-0017`.
    pub id: String,
    /// False when this change shows the learning is wrong or stale.
    pub held: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// What the principal engineer returns: `structured_output` of a run with
/// `--json-schema plugin/schemas/verdict.json`. It has no approve/reject
/// field on purpose: approval is computed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ModelVerdict {
    /// Two or three sentences for the review comment and the journal.
    pub summary: String,
    /// New findings in this round.
    pub findings: Vec<ModelFinding>,
    /// A ruling on each open finding from earlier rounds.
    #[serde(default)]
    pub prior: Vec<PriorRuling>,
    /// The briefing's learnings that informed this review.
    #[serde(default)]
    pub learnings: Vec<LearningUse>,
    /// Why a person should approve this change before it merges, even if
    /// no configured path asks for one (ADR 0063). None when the change is
    /// ordinary.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub needs_person: Option<String>,
}

impl ModelVerdict {
    /// Parses and checks a verdict. Unknown fields, bad enums and empty
    /// required text are `INVALID_VERDICT`.
    pub fn parse(v: &serde_json::Value) -> Result<ModelVerdict> {
        let mv: ModelVerdict = serde_json::from_value(v.clone()).map_err(|e| {
            HallError::new(ErrorCode::InvalidVerdict, format!("verdict: {e}"))
                .next(["hall schema verdict-input"])
        })?;
        mv.check()?;
        Ok(mv)
    }

    pub fn check(&self) -> Result<()> {
        let bad = |m: String| {
            Err(HallError::new(ErrorCode::InvalidVerdict, m).next(["hall schema verdict-input"]))
        };
        if self.summary.trim().is_empty() {
            return bad("verdict: summary is empty".into());
        }
        for (i, f) in self.findings.iter().enumerate() {
            if f.file.trim().is_empty() || f.title.trim().is_empty() || f.scenario.trim().is_empty()
            {
                return bad(format!(
                    "verdict: finding {} needs file, title and scenario",
                    i + 1
                ));
            }
        }
        Ok(())
    }
}

/// A finding as recorded: numbered per item.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Finding {
    /// `F1`, `F2`, … unique within the item.
    pub id: String,
    /// The review round that raised it.
    pub round: u32,
    pub lens: Lens,
    pub severity: Severity,
    pub file: String,
    pub line: u32,
    pub title: String,
    pub scenario: String,
    pub fix: String,
    /// Not introduced by this change: it never blocks approval, and the
    /// fixer may leave it (ADR 0059).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub pre_existing: bool,
}

/// A finding the verifier showed is not real, left out of the verdict
/// (ADR 0062).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct DroppedFinding {
    pub lens: Lens,
    pub severity: Severity,
    pub file: String,
    pub line: u32,
    pub title: String,
    /// What makes the scenario impossible, with the file and line checked.
    pub reason: String,
}

/// A fixer's dispute of a finding, with its reason.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Dispute {
    pub id: String,
    pub reason: String,
}

/// Open findings by severity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
pub struct Counts {
    pub blocker: u32,
    pub major: u32,
    pub minor: u32,
    pub nit: u32,
}

impl Counts {
    pub fn add(&mut self, s: Severity) {
        match s {
            Severity::Blocker => self.blocker += 1,
            Severity::Major => self.major += 1,
            Severity::Minor => self.minor += 1,
            Severity::Nit => self.nit += 1,
        }
    }

    /// The approval rule: 0 blockers and 0 majors.
    pub fn approves(&self) -> bool {
        self.blocker == 0 && self.major == 0
    }

    pub fn total(&self) -> u32 {
        self.blocker + self.major + self.minor + self.nit
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Open,
    Fixed,
    Withdrawn,
}

/// A finding and what has happened to it since.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct Tracked {
    #[serde(flatten)]
    pub finding: Finding,
    pub status: Status,
    /// The commit whose `hall fixed` claims to fix it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub claimed_by: Option<String>,
    /// The fixer's dispute, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dispute: Option<String>,
    /// The round whose review settled it (fixed or withdrawn).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub settled_round: Option<u32>,
    /// The reviewer's note when settling it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// Every finding of an item, in the order raised.
#[derive(Debug, Clone, Default, PartialEq, Serialize, JsonSchema)]
pub struct Ledger {
    pub findings: Vec<Tracked>,
}

impl Ledger {
    /// Folds an item's journal.
    pub fn from_events(events: &[Event]) -> Ledger {
        let mut l = Ledger::default();
        for e in events {
            l.apply(&e.body);
        }
        l
    }

    pub fn apply(&mut self, body: &Body) {
        match body {
            Body::Verdict {
                round,
                findings,
                prior,
                ..
            } => {
                for p in prior {
                    if let Some(t) = self.get_mut(&p.id) {
                        t.status = match p.status {
                            PriorStatus::Fixed => Status::Fixed,
                            PriorStatus::Open => Status::Open,
                            PriorStatus::Withdrawn => Status::Withdrawn,
                        };
                        if t.status != Status::Open {
                            t.settled_round = Some(*round);
                        }
                        t.note = p.note.clone();
                    }
                }
                for f in findings {
                    if self.get_mut(&f.id).is_none() {
                        self.findings.push(Tracked {
                            finding: f.clone(),
                            status: Status::Open,
                            claimed_by: None,
                            dispute: None,
                            settled_round: None,
                            note: None,
                        });
                    }
                }
            }
            Body::FixSubmitted {
                sha,
                fixes,
                disputes,
            } => {
                for id in fixes {
                    if let Some(t) = self.get_mut(id) {
                        t.claimed_by = Some(sha.clone());
                    }
                }
                for d in disputes {
                    if let Some(t) = self.get_mut(&d.id) {
                        t.dispute = Some(d.reason.clone());
                    }
                }
            }
            _ => {}
        }
    }

    fn get_mut(&mut self, id: &str) -> Option<&mut Tracked> {
        self.findings.iter_mut().find(|t| t.finding.id == id)
    }

    pub fn get(&self, id: &str) -> Option<&Tracked> {
        self.findings.iter().find(|t| t.finding.id == id)
    }

    pub fn open(&self) -> Vec<&Tracked> {
        self.findings
            .iter()
            .filter(|t| t.status == Status::Open)
            .collect()
    }

    /// Open findings by severity, leaving out the pre-existing ones: they
    /// never count toward approval (ADR 0059).
    pub fn open_counts(&self) -> Counts {
        let mut c = Counts::default();
        for t in self.open() {
            if !t.finding.pre_existing {
                c.add(t.finding.severity);
            }
        }
        c
    }

    /// Open findings the change didn't introduce.
    pub fn open_pre_existing(&self) -> Vec<&Tracked> {
        self.open()
            .into_iter()
            .filter(|t| t.finding.pre_existing)
            .collect()
    }

    /// The next free `F<n>`.
    pub fn next_number(&self) -> u32 {
        self.findings
            .iter()
            .filter_map(|t| t.finding.id.strip_prefix('F')?.parse::<u32>().ok())
            .max()
            .unwrap_or(0)
            + 1
    }

    /// Applies a model verdict for `round`: rules on prior findings (ones it
    /// leaves out stay open; unknown IDs are ignored), numbers the new ones,
    /// and returns the recorded findings and the open counts afterwards.
    ///
    /// `max_nits` caps the nits recorded from this verdict (ADR 0058): the
    /// first `max_nits` in the reviewer's order are kept, the rest are
    /// counted in `dropped_nits` and never recorded.
    pub fn judge(&self, round: u32, mv: &ModelVerdict, max_nits: u32) -> Judged {
        let open_ids: Vec<String> = self.open().iter().map(|t| t.finding.id.clone()).collect();
        let prior: Vec<PriorRuling> = mv
            .prior
            .iter()
            .filter(|p| open_ids.contains(&p.id))
            .cloned()
            .collect();
        let mut n = self.next_number();
        let mut nits = 0u32;
        let mut dropped_nits = 0u32;
        let findings: Vec<Finding> = mv
            .findings
            .iter()
            .filter(|f| {
                if f.severity != Severity::Nit {
                    return true;
                }
                nits += 1;
                if nits > max_nits {
                    dropped_nits += 1;
                    return false;
                }
                true
            })
            .map(|f| {
                let id = format!("F{n}");
                n += 1;
                Finding {
                    id,
                    round,
                    lens: f.lens,
                    severity: f.severity,
                    file: f.file.clone(),
                    line: f.line,
                    title: f.title.clone(),
                    scenario: f.scenario.clone(),
                    fix: f.fix.clone(),
                    pre_existing: f.pre_existing,
                }
            })
            .collect();
        let mut after = self.clone();
        after.apply(&Body::Verdict {
            sha: String::new(),
            round,
            verdict: crate::event::VerdictKind::Changes,
            escalated: false,
            findings: findings.clone(),
            prior: prior.clone(),
            counts: Counts::default(),
            summary: None,
            report: None,
            reviewer: None,
            learnings: vec![],
            base: None,
            since: None,
            needs_person: None,
        });
        let counts = after.open_counts();
        Judged {
            findings,
            prior,
            counts,
            dropped_nits,
        }
    }

    /// Open findings by file, for prompts.
    pub fn open_by_file(&self) -> BTreeMap<String, Vec<&Tracked>> {
        let mut m: BTreeMap<String, Vec<&Tracked>> = BTreeMap::new();
        for t in self.open() {
            m.entry(t.finding.file.clone()).or_default().push(t);
        }
        m
    }
}

/// The result of [`Ledger::judge`].
#[derive(Debug, Clone, PartialEq)]
pub struct Judged {
    pub findings: Vec<Finding>,
    pub prior: Vec<PriorRuling>,
    /// Open findings after this verdict.
    pub counts: Counts,
    /// Nits the reviewer reported beyond `max_nits`, not recorded.
    pub dropped_nits: u32,
}

impl Judged {
    pub fn approves(&self) -> bool {
        self.counts.approves()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::VerdictKind;
    use serde_json::json;

    fn mf(sev: Severity, title: &str) -> ModelFinding {
        ModelFinding {
            lens: Lens::Correctness,
            severity: sev,
            file: "src/retry.rs".into(),
            line: 12,
            title: title.into(),
            scenario: "two workers retry at once → double delivery".into(),
            fix: "take a row lock".into(),
            pre_existing: false,
        }
    }

    #[test]
    fn a_pre_existing_finding_is_recorded_but_never_blocks() {
        let mut l = Ledger::default();
        let v = ModelVerdict {
            summary: "s".into(),
            findings: vec![
                ModelFinding {
                    pre_existing: true,
                    ..mf(Severity::Blocker, "old race")
                },
                mf(Severity::Minor, "naming"),
            ],
            prior: vec![],
            learnings: vec![],
        };
        let j = l.judge(1, &v, 10);
        assert!(j.approves(), "{:?}", j.counts);
        assert_eq!(j.counts.blocker, 0);
        assert_eq!(j.counts.minor, 1);
        assert!(j.findings[0].pre_existing);
        record(&mut l, 1, &j);
        assert_eq!(l.open().len(), 2, "it is tracked like any finding");
        assert_eq!(l.open_pre_existing().len(), 1);
        assert_eq!(l.open_pre_existing()[0].finding.id, "F1");
        // The flag is written only when set, so old journals read as before.
        let json = serde_json::to_value(&j.findings).unwrap();
        assert_eq!(json[0]["pre_existing"], true);
        assert!(json[1].get("pre_existing").is_none());
        let back: Finding = serde_json::from_value(json[1].clone()).unwrap();
        assert!(!back.pre_existing);
        // A verdict without the field still parses.
        let mv = ModelVerdict::parse(&serde_json::json!({
            "summary": "s", "findings": [{"lens": "correctness", "severity": "major",
            "file": "a.rs", "line": 1, "title": "t", "scenario": "s", "fix": "f"}],
            "prior": [], "learnings": []
        }))
        .unwrap();
        assert!(!mv.findings[0].pre_existing);
    }

    fn record(l: &mut Ledger, round: u32, j: &Judged) {
        l.apply(&Body::Verdict {
            sha: "x".into(),
            round,
            verdict: if j.approves() {
                VerdictKind::Approve
            } else {
                VerdictKind::Changes
            },
            escalated: false,
            findings: j.findings.clone(),
            prior: j.prior.clone(),
            counts: j.counts,
            summary: None,
            report: None,
            reviewer: None,
            learnings: vec![],
            base: None,
            since: None,
            needs_person: None,
        });
    }

    #[test]
    fn nits_beyond_the_cap_are_dropped_in_the_reviewers_order() {
        let l = Ledger::default();
        let v = ModelVerdict {
            summary: "s".into(),
            findings: vec![
                mf(Severity::Nit, "n1"),
                mf(Severity::Major, "m1"),
                mf(Severity::Nit, "n2"),
                mf(Severity::Nit, "n3"),
                mf(Severity::Minor, "mi"),
                mf(Severity::Nit, "n4"),
            ],
            prior: vec![],
            learnings: vec![],
        };
        let j = l.judge(1, &v, 2);
        let titles: Vec<&str> = j.findings.iter().map(|f| f.title.as_str()).collect();
        assert_eq!(titles, ["n1", "m1", "n2", "mi"]);
        assert_eq!(
            j.findings.iter().map(|f| f.id.as_str()).collect::<Vec<_>>(),
            ["F1", "F2", "F3", "F4"],
            "numbering has no gaps"
        );
        assert_eq!(j.dropped_nits, 2);
        assert_eq!(j.counts.nit, 2);
        // A cap of zero records no nits; a large cap drops none.
        assert_eq!(l.judge(1, &v, 0).dropped_nits, 4);
        assert_eq!(l.judge(1, &v, 10).dropped_nits, 0);
        assert_eq!(l.judge(1, &v, 10).findings.len(), 6);
    }

    #[test]
    fn approval_is_computed_from_open_findings() {
        let mut l = Ledger::default();
        let v1 = ModelVerdict {
            summary: "s".into(),
            findings: vec![mf(Severity::Blocker, "race"), mf(Severity::Minor, "naming")],
            prior: vec![],
            learnings: vec![],
            needs_person: None,
        };
        let j1 = l.judge(1, &v1, 3);
        assert!(!j1.approves());
        assert_eq!(j1.findings[0].id, "F1");
        assert_eq!(j1.findings[1].id, "F2");
        record(&mut l, 1, &j1);

        // The fixer claims F1 and disputes F2.
        l.apply(&Body::FixSubmitted {
            sha: "b".into(),
            fixes: vec!["F1".into()],
            disputes: vec![Dispute {
                id: "F2".into(),
                reason: "matches the module's convention".into(),
            }],
        });
        assert_eq!(l.get("F1").unwrap().claimed_by.as_deref(), Some("b"));
        assert!(l.get("F2").unwrap().dispute.is_some());

        // Round 2: F1 fixed, F2 left out (stays open, minor), one new major.
        let v2 = ModelVerdict {
            summary: "s".into(),
            findings: vec![mf(Severity::Major, "no test")],
            prior: vec![
                PriorRuling {
                    id: "F1".into(),
                    status: PriorStatus::Fixed,
                    note: None,
                },
                PriorRuling {
                    id: "F99".into(),
                    status: PriorStatus::Fixed,
                    note: None,
                },
            ],
            learnings: vec![],
            needs_person: None,
        };
        let j2 = l.judge(2, &v2, 3);
        assert_eq!(j2.prior.len(), 1, "unknown IDs are ignored");
        assert_eq!(j2.findings[0].id, "F3");
        assert_eq!(
            j2.counts,
            Counts {
                blocker: 0,
                major: 1,
                minor: 1,
                nit: 0
            }
        );
        assert!(!j2.approves());
        record(&mut l, 2, &j2);

        // Round 3: F3 fixed; a minor stays open, which doesn't block.
        let v3 = ModelVerdict {
            summary: "s".into(),
            findings: vec![],
            prior: vec![PriorRuling {
                id: "F3".into(),
                status: PriorStatus::Fixed,
                note: None,
            }],
            learnings: vec![],
            needs_person: None,
        };
        let j3 = l.judge(3, &v3, 3);
        assert!(j3.approves());
        record(&mut l, 3, &j3);
        assert_eq!(l.open().len(), 1);
        assert_eq!(l.get("F1").unwrap().settled_round, Some(2));
    }

    #[test]
    fn model_verdicts_are_checked() {
        let ok = json!({"summary": "fine", "findings": [], "prior": [], "learnings": []});
        assert!(ModelVerdict::parse(&ok).is_ok());
        let extra = json!({"summary": "x", "findings": [], "verdict": "approve"});
        assert_eq!(
            ModelVerdict::parse(&extra).unwrap_err().code,
            ErrorCode::InvalidVerdict
        );
        let bad_lens = json!({"summary": "x", "findings": [{"lens": "vibes", "severity": "nit",
            "file": "a", "line": 1, "title": "t", "scenario": "s", "fix": "f"}]});
        assert!(ModelVerdict::parse(&bad_lens).is_err());
        let empty = json!({"summary": " ", "findings": []});
        assert!(ModelVerdict::parse(&empty).is_err());
    }

    #[test]
    fn lenses_parse_loosely() {
        assert_eq!(Lens::parse("security"), Some(Lens::SecurityTenancy));
        assert_eq!(
            Lens::parse("Security & tenancy"),
            Some(Lens::SecurityTenancy)
        );
        assert_eq!(Lens::parse("repo-rules"), Some(Lens::RepoRules));
        assert_eq!(Lens::parse("evidence"), Some(Lens::Evidence));
        assert_eq!(Lens::parse("nope"), None);
    }

    #[test]
    fn the_plugin_schema_matches_the_types() {
        let schema: serde_json::Value =
            serde_json::from_str(include_str!("../plugin/schemas/verdict.json")).unwrap();
        let f = &schema["properties"]["findings"]["items"];
        let lenses: Vec<&str> = f["properties"]["lens"]["enum"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert_eq!(lenses, Lens::ALL.map(Lens::as_str));
        let sev: Vec<&str> = f["properties"]["severity"]["enum"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert_eq!(sev, Severity::ALL.map(Severity::as_str));
        let mut req: Vec<&str> = f["required"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        req.sort();
        assert_eq!(
            req,
            [
                "file", "fix", "lens", "line", "scenario", "severity", "title"
            ]
        );
        // The properties are ModelFinding's fields; pre_existing is a
        // boolean the reviewer may leave out.
        let mut props: Vec<&str> = f["properties"]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        props.sort();
        assert_eq!(
            props,
            [
                "file",
                "fix",
                "lens",
                "line",
                "pre_existing",
                "scenario",
                "severity",
                "title"
            ]
        );
        assert_eq!(f["properties"]["pre_existing"]["type"], "boolean");
        assert!(!req.contains(&"pre_existing"));
        // The schema offers exactly the fields the type accepts.
        let mut props: Vec<String> = schema["properties"]
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect();
        props.sort();
        let generated = serde_json::to_value(schemars::schema_for!(ModelVerdict)).unwrap();
        let mut want: Vec<String> = generated["properties"]
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect();
        want.sort();
        assert_eq!(props, want);
        assert!(schema["examples"][0]["needs_person"].is_string());
        // The example in the schema's description round-trips.
        let example = &schema["examples"][0];
        ModelVerdict::parse(example).unwrap();
    }
}
