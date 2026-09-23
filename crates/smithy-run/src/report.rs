//! The morning report.
//!
//! `.smithy/runs/<id>/REPORT.md`, rewritten from the Run's state and its
//! decision log at every checkpoint — so a Run that died at 3 a.m. still has
//! one, as true as its last checkpoint. Read top to bottom it answers, in
//! order: did it work, does anything need me, what got done, what did not and
//! why, what did it learn, and what did it decide on my behalf.

use std::collections::BTreeMap;

use crate::decisions::{Decision, Outcome};
use crate::plan::Plan;
use crate::state::{human_duration, utc_minute, RunState, TaskStatus, Verdict};

/// One research Note, as the Report lists it.
#[derive(Debug, Clone, PartialEq)]
pub struct NoteLine {
    pub path: String,
    pub question: String,
    pub verified: usize,
    pub findings: usize,
    pub answered: Option<f64>,
}

pub fn render(
    state: &RunState,
    plan: Option<&Plan>,
    decisions: &[Decision],
    outcomes: &BTreeMap<usize, Vec<Outcome>>,
    notes: &[NoteLine],
) -> String {
    let mut out = String::new();
    let verdict = state
        .verdict
        .as_ref()
        .map(Verdict::headline)
        .unwrap_or_else(|| "still running (or stopped without finishing — resume it)".into());
    out.push_str(&format!("# Run {}\n\n**{verdict}**\n\n", state.id));
    out.push_str(&format!(
        "> {}\n\n",
        state.intent.trim().replace('\n', "\n> ")
    ));

    let short_base: String = state.base.chars().take(10).collect();
    out.push_str(&format!(
        "| | |\n|---|---|\n| Branch | `{}` (from `{short_base}`) |\n| Started | {} |\n| Last update | {} |\n| Time spent | {} of {}h |\n| Toolchain | {} |\n| Model requests | {} ({} prompt tokens, {}% from cache) |\n\n",
        state.branch,
        utc_minute(state.started),
        utc_minute(state.updated),
        human_duration(state.elapsed),
        state.ceilings.hours,
        state.toolchain.name,
        state.usage.requests,
        state.usage.prompt_tokens,
        if state.usage.prompt_tokens > 0 {
            state.usage.cached_tokens * 100 / state.usage.prompt_tokens
        } else {
            0
        },
    ));
    out.push_str(&format!(
        "Review: `git log --stat {short_base}..{b}` · diff: `git diff {short_base}...{b}` · \
         keep: `git merge {b}` · discard: `git branch -D {b}` · resume: `smithy-agent run --resume {id}`\n\n",
        b = state.branch,
        id = state.id,
    ));

    // Needs you — first, because it is why you were woken.
    if !state.flags.is_empty() {
        out.push_str("## Needs you\n\n");
        for f in &state.flags {
            let p = f
                .probability
                .map(|p| format!(" ({:.0}%)", p * 100.0))
                .unwrap_or_default();
            out.push_str(&format!(
                "- **{}** on `{}`{p} at {}: {}\n",
                f.kind,
                f.subject,
                utc_minute(f.at),
                f.text.trim()
            ));
            if f.kind == "guardrail" {
                out.push_str(&format!(
                    "  - If this is fine: `smithy-agent run --resume {} --allow {}`\n",
                    state.id, f.subject
                ));
            }
        }
        out.push('\n');
    }

    out.push_str("## Tasks\n\n");
    match plan {
        None => out.push_str("No plan yet.\n\n"),
        Some(plan) => {
            out.push_str(
                "| | Task | Attempts | Checks | Commit | Time |\n|---|---|---|---|---|---|\n",
            );
            for task in &plan.tasks {
                let ts = state.tasks.get(&task.id).cloned().unwrap_or_default();
                let mark = match ts.status {
                    TaskStatus::Done => "✅",
                    TaskStatus::Blocked => "⛔",
                    TaskStatus::Flagged => "🚩",
                    TaskStatus::Active => "▶",
                    TaskStatus::Pending => "·",
                };
                let checks = if ts.last_checks.is_empty() {
                    "—".to_string()
                } else {
                    ts.last_checks
                        .iter()
                        .map(|c| format!("{} {}", if c.passed { "✓" } else { "✗" }, c.verdict))
                        .collect::<Vec<_>>()
                        .join("; ")
                };
                let commit = ts
                    .commit
                    .as_deref()
                    .map(|c| format!("`{}`", &c[..c.len().min(10)]))
                    .unwrap_or_else(|| "—".into());
                out.push_str(&format!(
                    "| {mark} | **{}** {} | {} | {} | {commit} | {} |\n",
                    task.id,
                    escape_cell(&task.title),
                    ts.attempts,
                    escape_cell(&checks),
                    human_duration(ts.seconds)
                ));
            }
            out.push('\n');

            let blocked: Vec<_> = plan
                .tasks
                .iter()
                .filter_map(|t| {
                    let ts = state.tasks.get(&t.id)?;
                    matches!(ts.status, TaskStatus::Blocked | TaskStatus::Flagged)
                        .then_some((t, ts))
                })
                .collect();
            if !blocked.is_empty() {
                out.push_str("## Blocked\n\n");
                for (task, ts) in blocked {
                    out.push_str(&format!(
                        "### {} — {}\n\n{}\n\n",
                        task.id,
                        task.title,
                        ts.blocker.as_deref().unwrap_or("(no reason recorded)")
                    ));
                    if let Some(failed) = ts.last_checks.iter().find(|c| !c.passed) {
                        out.push_str(&format!(
                            "Last failing check `{}` — {}:\n\n```\n{}\n```\n\n",
                            failed.spec.run,
                            failed.verdict,
                            clip_lines(&failed.excerpt, 40)
                        ));
                    }
                    if let Some(h) = &ts.handoff {
                        out.push_str(&format!("Last attempt's handoff:\n\n{}\n\n", quote(h)));
                    }
                }
            }
        }
    }

    if let Some(b) = &state.baseline {
        out.push_str("## Baseline\n\n");
        match &b.unavailable {
            Some(why) => out.push_str(&format!("The suite did not run at the start: {why}\n\n")),
            None => out.push_str(&format!(
                "{} passed, {} failed when the Run began. No test that passed then may fail now.\n\n",
                b.passed, b.failed
            )),
        }
    }

    if !notes.is_empty() {
        out.push_str("## Research\n\n| Note | Question | Verified findings | Answered |\n|---|---|---|---|\n");
        for n in notes {
            out.push_str(&format!(
                "| `{}` | {} | {}/{} | {} |\n",
                n.path,
                escape_cell(&n.question),
                n.verified,
                n.findings,
                n.answered
                    .map(|p| format!("{p:.2}"))
                    .unwrap_or_else(|| "—".into())
            ));
        }
        out.push('\n');
    }

    if !state.denied.is_empty() {
        out.push_str(
            "## Would have asked\n\nNobody was at the keyboard, so these were refused:\n\n",
        );
        for d in &state.denied {
            out.push_str(&format!(
                "- {}`{}` — {}\n",
                d.task
                    .as_deref()
                    .map(|t| format!("{t}: "))
                    .unwrap_or_default(),
                d.command.replace('`', "'"),
                d.why
            ));
        }
        out.push('\n');
    }

    if !state.stashes.is_empty() {
        out.push_str(
            "## Stashed\n\nWork from interrupted attempts, kept rather than discarded:\n\n",
        );
        for s in &state.stashes {
            out.push_str(&format!("- `{s}` — `git stash list | grep {s}`\n"));
        }
        out.push('\n');
    }

    out.push_str("## Decisions\n\n");
    if decisions.is_empty() {
        out.push_str("None.\n");
    } else {
        let mut by_kind: BTreeMap<&str, (usize, usize, usize)> = BTreeMap::new();
        for d in decisions {
            let e = by_kind.entry(&d.kind).or_default();
            e.0 += 1;
            for o in outcomes.get(&d.seq).into_iter().flatten() {
                match o.right {
                    Some(true) => e.1 += 1,
                    Some(false) => e.2 += 1,
                    None => {}
                }
            }
        }
        out.push_str(
            "| Kind | Made | Later shown right | Later shown wrong |\n|---|---|---|---|\n",
        );
        for (kind, (n, r, w)) in &by_kind {
            out.push_str(&format!("| {kind} | {n} | {r} | {w} |\n"));
        }
        out.push_str(&format!(
            "\n<details><summary>All {} decisions</summary>\n\n| # | When | Task | Kind | Answer | Threshold | Action | Outcome |\n|---|---|---|---|---|---|---|---|\n",
            decisions.len()
        ));
        for d in decisions {
            let outcome = outcomes
                .get(&d.seq)
                .map(|os| {
                    os.iter()
                        .map(|o| {
                            let mark = match o.right {
                                Some(true) => "✓ ",
                                Some(false) => "✗ ",
                                None => "",
                            };
                            format!("{mark}{}", o.note)
                        })
                        .collect::<Vec<_>>()
                        .join("; ")
                })
                .unwrap_or_default();
            out.push_str(&format!(
                "| {} | {} | {} | {} | {} | {} | {} | {} |\n",
                d.seq,
                utc_minute(d.at).trim_end_matches(" UTC"),
                d.task.as_deref().unwrap_or("—"),
                d.kind,
                escape_cell(&d.answer.short()),
                d.threshold.map(|t| format!("{t:.2}")).unwrap_or_default(),
                escape_cell(&d.action),
                escape_cell(&outcome)
            ));
        }
        out.push_str(&format!(
            "\nFull states are in `decisions.jsonl`; replay them with \
             `cargo run -p smithy-agent --example jev replay .smithy/runs/{}/decisions.jsonl`.\n\n</details>\n",
            state.id
        ));
    }
    out
}

fn escape_cell(s: &str) -> String {
    s.replace('|', "\\|").replace('\n', " ")
}

fn quote(s: &str) -> String {
    s.lines()
        .map(|l| format!("> {l}"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn clip_lines(s: &str, n: usize) -> String {
    let lines: Vec<&str> = s.lines().collect();
    if lines.len() <= n {
        s.to_string()
    } else {
        format!(
            "{}\n[… {} more lines]",
            lines[..n].join("\n"),
            lines.len() - n
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::check::{CheckKind, CheckOutcome, CheckSpec};
    use crate::decisions::Answer;
    use crate::state::Flag;
    use crate::toolchain::Toolchain;

    fn plan() -> Plan {
        Plan::parse(
            r#"
intent = "durations"
toolchain = "rust"
[[task]]
id = "T1"
title = "Date part"
[[task.check]]
kind = "test"
run = "cargo test date"
[[task]]
id = "T2"
title = "Time | part"
[[task.check]]
kind = "test"
run = "cargo test time"
"#,
        )
        .unwrap()
    }

    #[test]
    fn a_blocked_run_says_why_and_how_to_carry_on() {
        let mut s = RunState::new(
            "r1",
            "parse durations",
            "smithy/run-r1",
            "0123456789abcdef",
            Toolchain::rust(),
        );
        s.task("T1").status = TaskStatus::Done;
        s.task("T1").commit = Some("feedfacecafe".into());
        let t2 = s.task("T2");
        t2.status = TaskStatus::Blocked;
        t2.attempts = 3;
        t2.blocker = Some("the same test failed in three attempts".into());
        t2.last_checks = vec![CheckOutcome {
            spec: CheckSpec {
                kind: CheckKind::Test,
                run: "cargo test time".into(),
                min_tests: None,
            },
            passed: false,
            verdict: "exit 101 (0 passed, 1 failed)".into(),
            tests: None,
            excerpt: "---- time::hours stdout ----\npanicked at src/time.rs:9".into(),
            seconds: 3,
        }];
        s.verdict = Some(Verdict::Blocked("T2 used its 3 attempts".into()));
        s.flags.push(Flag {
            at: 0,
            subject: "T2".into(),
            kind: "escalate".into(),
            probability: Some(0.5),
            text: "same failure after a handoff".into(),
        });
        let decisions = vec![Decision {
            seq: 0,
            at: 0,
            task: Some("T2".into()),
            attempt: Some(3),
            kind: "next".into(),
            state: "…".into(),
            answer: Answer::Probability(0.9),
            threshold: Some(0.85),
            action: "escalate".into(),
        }];
        let r = render(&s, Some(&plan()), &decisions, &BTreeMap::new(), &[]);

        assert!(r.contains("**blocked — T2 used its 3 attempts**"), "{r}");
        let needs = r.find("## Needs you").unwrap();
        assert!(needs < r.find("## Tasks").unwrap(), "flags come first");
        assert!(r.contains("`feedfaceca`"), "{r}");
        assert!(r.contains("Time \\| part"), "pipes in titles are escaped");
        assert!(r.contains("panicked at src/time.rs:9"));
        assert!(r.contains("git branch -D smithy/run-r1"));
        assert!(r.contains("| next | 1 | 0 | 0 |"), "{r}");
    }

    #[test]
    fn a_run_with_no_plan_still_reports() {
        let s = RunState::new("r2", "x", "smithy/run-r2", "abc", Toolchain::rust());
        let r = render(&s, None, &[], &BTreeMap::new(), &[]);
        assert!(r.contains("No plan yet."));
        assert!(r.contains("still running"));
    }
}
