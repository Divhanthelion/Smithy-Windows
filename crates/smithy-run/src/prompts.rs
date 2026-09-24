//! What the runner says to the model. One place, so the wording can be read
//! and tuned without reading the runner.

use crate::check::CheckOutcome;
use crate::plan::{Plan, Task};
use crate::toolchain::Toolchain;

const UNATTENDED: &str = "This is an unattended Run. Nobody will answer questions until it is \
over, so make every decision yourself and keep going; asking for confirmation ends nothing and \
wastes the turn.";

/// The first message of an Attempt.
#[allow(clippy::too_many_arguments)]
pub fn task_prompt(
    plan: &Plan,
    task: &Task,
    done: &std::collections::BTreeSet<String>,
    toolchain: &Toolchain,
    notes: &[(String, String)],
    handoff: Option<&str>,
    attempt: usize,
    max_attempts: usize,
) -> String {
    let mut out =
        format!(
        "{UNATTENDED}\n\n## The intent\n\n{}\n\n## The plan\n\n{}\n## Your task: {} — {}\n\n{}\n\n\
         Attempt {attempt} of {max_attempts}.\n\n\
         ## Done means these pass\n\nThe runner runs them itself after you answer; your word \
         that they pass is not enough.\n\n{}\n\nThe full suite (`{}`) must also keep passing, and \
         tests that existed before this Run may not be weakened, skipped or deleted.\n",
        plan.intent.trim(),
        plan.render(done),
        task.id,
        task.title,
        if task.why.trim().is_empty() { "" } else { task.why.trim() },
        task.render_checks(),
        toolchain.test,
    );
    if !notes.is_empty() {
        out.push_str("\n## Research on file\n\nRead these before building on facts from outside the Project:\n\n");
        for (path, question) in notes {
            out.push_str(&format!("- `{path}` — {question}\n"));
        }
    }
    if let Some(h) = handoff.filter(|h| !h.trim().is_empty()) {
        out.push_str(&format!(
            "\n## What the previous attempt left you\n\nThe files are as it left them.\n\n{}\n",
            h.trim()
        ));
    }
    out.push_str(
        "\n## How to work\n\n\
         - This task only. Later tasks have their own turn. The runner runs this task's checks \
         as you go and ends the turn the first time they pass, so build what they need and nothing \
         more.\n\
         - Do not use git to change anything (commit, checkout, reset, stash…): the runner commits \
         when the checks pass. Read-only git is fine.\n\
         - Do not edit `.smithy/runs/`.\n\
         - Run the checks yourself before you answer. When they pass, answer with a few lines on \
         what you changed.\n\
         - If something outside the Project is genuinely required (credentials, a service, a \
         decision only a person can make), say exactly what and stop.\n",
    );
    out
}

/// After a failed round of Checks.
pub fn feedback(round: usize, max_rounds: usize, failed: &[&CheckOutcome], extra: &str) -> String {
    let mut out = format!("The checks do not pass yet (round {round} of {max_rounds}).\n\n");
    for c in failed {
        out.push_str(&format!(
            "`{}` — {}\n```\n{}\n```\n\n",
            c.spec.run,
            c.verdict,
            c.excerpt.trim()
        ));
    }
    if !extra.trim().is_empty() {
        out.push_str(extra.trim());
        out.push_str("\n\n");
    }
    out.push_str("Fix the cause, run the failing check yourself, and answer when it passes.");
    out
}

pub const HANDOFF_REQUEST: &str = "Stop here: this attempt is ending and a fresh one will take \
over with only your notes and the files as they are. Write those notes now, without calling \
tools: what you tried, what you learned (including what did not work and why), what is left, and \
what you would try next. Be specific — paths, errors, names.";

/// The research Session's message: the procedure, then the pinned question.
pub fn research_prompt(
    procedure: &str,
    question: &str,
    task: Option<(&str, &str)>,
    note_path: &str,
) -> String {
    let task_line = task
        .map(|(id, title)| format!("Task: {id} — {title}\n"))
        .unwrap_or_default();
    format!(
        "{procedure}\n\n---\n\n{UNATTENDED} The question is already pinned; do not wait for a yes.\n\n\
         {task_line}Pinned question: {question}\n\n\
         Write the note to `{note_path}`{} as soon as you have a few sources — a draft with the \
         Pin, the findings so far and the open questions — and update it as you learn more: the \
         turn has a time limit, and a note that was never written is lost. Run `cite_check` on it \
         and fix or drop every finding \
         that fails until it passes. Then answer with the note's path and its implication in two \
         or three sentences.",
        task.map(|(id, _)| format!(" with `**Task:** {id}` in its header"))
            .unwrap_or_default()
    )
}

/// The research question for a Task that listed none.
pub fn implied_question(task: &Task) -> String {
    format!(
        "What must be known exactly, from primary sources, to build this correctly: {}{}?",
        task.title.trim(),
        if task.why.trim().is_empty() {
            String::new()
        } else {
            format!(" ({})", task.why.trim())
        }
    )
}

/// The research question when a build keeps failing on an outside fact.
pub fn failure_question(task: &Task, failure: &CheckOutcome) -> String {
    let first = failure
        .excerpt
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or(&failure.verdict);
    format!(
        "Building \"{}\", this keeps failing: {}. What do the primary sources (docs, spec, source) \
         say is the correct way?",
        task.title.trim(),
        first.trim()
    )
}

/// What a note's file is called: date, task, and a slug of the question.
pub fn note_path(date: &str, task: Option<&str>, question: &str) -> String {
    let slug: String = question
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.len() > 2)
        .take(6)
        .collect::<Vec<_>>()
        .join("-");
    let task = task
        .map(|t| format!("{}-", t.to_lowercase()))
        .unwrap_or_default();
    format!(
        "{}/{date}-{task}{slug}.md",
        smithy_tools::research::NOTES_DIR
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn note_paths_are_dated_tasked_and_slugged() {
        assert_eq!(
            note_path(
                "2026-09-23",
                Some("T1"),
                "Which ISO 8601 designators may carry a fraction?"
            ),
            ".smithy/research/2026-09-23-t1-which-iso-8601-designators-may-carry.md"
        );
    }

    #[test]
    fn the_task_prompt_says_what_done_means_and_what_not_to_touch() {
        let plan = Plan::parse(
            "intent = \"durations\"\ntoolchain = \"rust\"\n[[task]]\nid = \"T1\"\ntitle = \"Days\"\n[[task.check]]\nkind = \"test\"\nrun = \"cargo test days\"\nmin_tests = 2\n",
        )
        .unwrap();
        let p = task_prompt(
            &plan,
            &plan.tasks[0],
            &Default::default(),
            &Toolchain::rust(),
            &[(".smithy/research/n.md".into(), "q?".into())],
            Some("tried X"),
            2,
            3,
        );
        for needle in [
            "unattended Run",
            "T1 — Days",
            "at least 2",
            "Attempt 2 of 3",
            "n.md",
            "tried X",
            "Do not use git",
        ] {
            assert!(p.contains(needle), "missing {needle}:\n{p}");
        }
    }
}
