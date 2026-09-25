//! The Plan: an Intent broken into Tasks, each with the Checks that decide it.
//!
//! Written once by a planning Session, validated here before anything runs,
//! committed as the Run's first commit, and never edited by the model after
//! that. Status lives in the Run's state, not here: the Plan is the contract,
//! and a contract the worker can amend is not one.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::check::{CheckKind, CheckSpec};
use crate::toolchain::{Counter, Toolchain};

/// Enough for an evening's intent. A plan longer than this is a plan for
/// several Runs, and should say so rather than try.
pub const MAX_TASKS: usize = 12;

/// How much a research question deserves. Research scales with the question
/// rather than being capped: the first real Run spent an hour on each
/// question because every one got the full adversarial method, not because
/// there were too many — and one of those notes caught a real bug.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Depth {
    /// One exact fact from one authoritative page.
    Lookup,
    /// Choose between options, from a few primary sources.
    #[default]
    Decision,
    /// Open or contested: worth the full method.
    Deep,
}

impl Depth {
    /// The research turn's length.
    pub fn minutes(self) -> u64 {
        match self {
            Depth::Lookup => 8,
            Depth::Decision => 20,
            Depth::Deep => 45,
        }
    }

    /// Tool calls before the note must exist on disk.
    pub fn draft_by_step(self) -> usize {
        match self {
            Depth::Lookup => 6,
            Depth::Decision => 12,
            Depth::Deep => 20,
        }
    }

    /// Whether the model thinks before each step. A lookup is many quick
    /// fetch-and-quote actions; measured on the same question at the same
    /// time, thinking made 6 of them in eight minutes and verified 1 finding,
    /// not thinking made 61 and verified 6. On a decision, thinking found the
    /// fact that settled it and not thinking did not (2026-09-25, report §9).
    pub fn thinks(self) -> bool {
        !matches!(self, Depth::Lookup)
    }

    /// The Skill whose procedure the research Session follows.
    pub fn procedure(self) -> &'static str {
        match self {
            Depth::Lookup | Depth::Decision => "pointed-research",
            Depth::Deep => "research",
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Depth::Lookup => "lookup",
            Depth::Decision => "decision",
            Depth::Deep => "deep",
        }
    }
}

/// A question for research: a plain string, or with its depth and reason.
///
/// ```toml
/// research = ["Is P1W2D legal?", { question = "…", depth = "deep", why = "…" }]
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ResearchItem {
    Plain(String),
    Detailed {
        question: String,
        #[serde(default)]
        depth: Depth,
        #[serde(default)]
        why: String,
    },
}

impl ResearchItem {
    pub fn question(&self) -> &str {
        match self {
            ResearchItem::Plain(q) | ResearchItem::Detailed { question: q, .. } => q,
        }
    }

    pub fn depth(&self) -> Depth {
        match self {
            ResearchItem::Plain(_) => Depth::Decision,
            ResearchItem::Detailed { depth, .. } => *depth,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Plan {
    pub intent: String,
    pub toolchain: String,
    #[serde(rename = "task")]
    pub tasks: Vec<Task>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Task {
    pub id: String,
    pub title: String,
    /// Which part of the Intent this serves.
    #[serde(default)]
    pub why: String,
    #[serde(default)]
    pub depends: Vec<String>,
    /// Questions to answer from outside the Project before building.
    #[serde(default)]
    pub research: Vec<ResearchItem>,
    #[serde(rename = "check", default)]
    pub checks: Vec<CheckSpec>,
}

impl Plan {
    pub fn parse(text: &str) -> Result<Plan, String> {
        toml::from_str(text).map_err(|e| format!("not a valid plan: {e}"))
    }

    pub fn to_toml(&self) -> String {
        toml::to_string_pretty(self).unwrap_or_default()
    }

    pub fn task(&self, id: &str) -> Option<&Task> {
        self.tasks.iter().find(|t| t.id == id)
    }

    /// Every problem, not the first: the planner fixes them in one go.
    pub fn validate(&self, toolchain: &Toolchain) -> Result<(), Vec<String>> {
        let mut errors = Vec::new();
        if self.intent.trim().is_empty() {
            errors.push("`intent` is empty".to_string());
        }
        if self.tasks.is_empty() {
            errors.push("the plan has no [[task]]".into());
        }
        if self.tasks.len() > MAX_TASKS {
            errors.push(format!(
                "{} tasks; at most {MAX_TASKS}. Merge small steps, or cut the intent down",
                self.tasks.len()
            ));
        }
        for task in &self.tasks {
            if task.research.iter().any(|r| r.question().trim().is_empty()) {
                errors.push(format!("task {}: a research item has no question", task.id));
            }
        }
        let programs = allowed_programs(toolchain);
        let mut seen = BTreeSet::new();
        for task in &self.tasks {
            let at = format!("task {}", if task.id.is_empty() { "?" } else { &task.id });
            if !valid_id(&task.id) {
                errors.push(format!(
                    "{at}: id must be T followed by a number (T1, T2, …)"
                ));
            }
            if !seen.insert(task.id.clone()) {
                errors.push(format!("{at}: id used twice"));
            }
            if task.title.trim().is_empty() {
                errors.push(format!("{at}: no title"));
            }
            for dep in &task.depends {
                if !seen.contains(dep) || dep == &task.id {
                    errors.push(format!(
                        "{at}: depends on {dep}, which is not an earlier task"
                    ));
                }
            }
            if task.checks.is_empty() {
                errors.push(format!("{at}: no [[task.check]]; every task needs one"));
            }
            if !task.checks.iter().any(|c| c.kind == CheckKind::Test) {
                errors.push(format!(
                    "{at}: no `test` check; every task needs at least one test that proves it"
                ));
            }
            for check in &task.checks {
                if let Err(e) = check_command(&check.run, &programs) {
                    errors.push(format!("{at}: check `{}`: {e}", check.run));
                }
                if check.kind == CheckKind::Test && toolchain.counter == Counter::None {
                    errors.push(format!(
                        "{at}: a `test` check needs a toolchain that can count tests; this one \
                         ({}) cannot — set `counter` in .smithy/checks.toml",
                        toolchain.name
                    ));
                }
                if check.min_tests == Some(0) {
                    errors.push(format!("{at}: min_tests = 0 would pass with no tests"));
                }
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }

    /// The Plan as the model reads it in a Task prompt.
    pub fn render(&self, done: &BTreeSet<String>) -> String {
        let mut out = String::new();
        for task in &self.tasks {
            let mark = if done.contains(&task.id) { "x" } else { " " };
            out.push_str(&format!("- [{mark}] {} — {}\n", task.id, task.title));
        }
        out
    }
}

impl Task {
    /// The checks as the model reads them.
    pub fn render_checks(&self) -> String {
        self.checks
            .iter()
            .map(|c| match (c.kind, c.min_tests) {
                (CheckKind::Test, Some(n)) => {
                    format!("- `{}` (test, at least {n} must run and pass)", c.run)
                }
                (CheckKind::Test, None) => {
                    format!("- `{}` (test, at least 1 must run and pass)", c.run)
                }
                (kind, _) => format!("- `{}` ({})", c.run, format!("{kind:?}").to_lowercase()),
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

fn valid_id(id: &str) -> bool {
    id.strip_prefix('T')
        .is_some_and(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
}

/// The programs a Check may start with: what the toolchain's own commands
/// start with. A check is how the runner measures, so it may not be anything
/// the model likes.
fn allowed_programs(toolchain: &Toolchain) -> BTreeSet<String> {
    let mut programs: BTreeSet<String> = [
        &toolchain.build,
        &Some(toolchain.test.clone()),
        &toolchain.lint,
    ]
    .into_iter()
    .flatten()
    .flat_map(|cmd| {
        cmd.split("&&")
            .map(|p| first_word(p).to_string())
            .collect::<Vec<_>>()
    })
    .filter(|p| !p.is_empty())
    .collect();
    match toolchain.name.as_str() {
        "python" => {
            programs.extend(["python", "py", "pytest"].map(String::from));
        }
        "cmake" => {
            programs.extend(["cmake", "ctest"].map(String::from));
        }
        _ => {}
    }
    programs
}

fn first_word(s: &str) -> &str {
    s.split_whitespace().next().unwrap_or("")
}

/// `&&` chains only. Everything that can turn a failure into a success, or
/// hide an exit status, is refused: `||`, `;`, a pipe (the pipeline's status
/// is its last command's), backgrounding, and substitution.
fn check_command(run: &str, programs: &BTreeSet<String>) -> Result<(), String> {
    if run.trim().is_empty() {
        return Err("empty command".into());
    }
    for bad in ["||", ";", "`", "$(", "\n"] {
        if run.contains(bad) {
            return Err(format!(
                "`{}` is not allowed in a check; chain with `&&` only",
                bad.escape_debug()
            ));
        }
    }
    let without_and = run.replace("&&", " ");
    if without_and.contains('|') {
        return Err("a pipe hides the exit status of everything but its last command".into());
    }
    if without_and.contains('&') {
        return Err("backgrounding is not allowed in a check".into());
    }
    for part in run.split("&&") {
        let program = first_word(part);
        if !programs.contains(program) {
            return Err(format!(
                "starts with `{program}`; checks may start with {}",
                programs
                    .iter()
                    .map(|p| format!("`{p}`"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
    }
    Ok(())
}

/// The planning prompt. The planner is a full Session: it may read the
/// Project and research before answering.
pub fn planner_prompt(intent: &str, toolchain: &Toolchain, notes: &str) -> String {
    let lint = toolchain.lint.as_deref().unwrap_or("(none)");
    let build = toolchain.build.as_deref().unwrap_or("(none)");
    format!(
        "You are planning an unattended Run. Nobody will answer questions until it is over, so \
make every decision yourself and write it down.

## Intent (verbatim from the user)

{intent}

## What you produce

A plan in TOML, in a single ```toml fenced block in your final answer. Read the Project first \
(ls, read, grep, explore) so the plan fits what is already there. Do not write any files and do \
not start building; the runner does that task by task.

```toml
intent = \"<the intent, verbatim>\"
toolchain = \"{name}\"

[[task]]
id = \"T1\"
title = \"<short imperative>\"
why = \"<which part of the intent this serves>\"
depends = []                     # ids of earlier tasks it needs
research = [{{ question = \"<what must be known>\", depth = \"lookup\", why = \"<what goes wrong without it>\" }}]   # or []

[[task.check]]
kind = \"test\"                    # build | test | lint | command
run = \"<command>\"
min_tests = 3                    # how many tests this check must run and pass
```

## Rules the runner enforces (a plan that breaks one is sent back)

- 1 to {max} tasks, in the order they should be built; ids T1, T2, …
- Every task has at least one `test` check, and its tests prove that task's slice of the intent. \
Name the tests the task will add, via a filter, so the check fails until they exist: a filter that \
matches no test fails.
- `min_tests` is the number of tests the task adds or relies on — not 1 when you mean 5.
- Checks start with the toolchain's programs ({programs}) and chain only with `&&`. No `||`, `;`, \
pipes, or `$(…)`.
- Size the plan to the work. A small library or tool is 1 to 3 tasks; a large system may need \
all {max}, and one bigger than that is several Runs. Split where a task would otherwise be too \
big to finish and test in one go, not for its own sake. Each task ends with its own tests passing.
- Research what you cannot get right from what you already know and the Project — an exact \
detail of a specification, file format, protocol or external API, where a mistake would make the \
code wrong. Ask as many questions as the work genuinely needs, and none you could answer now. \
Give each a depth, which sets how long it gets: `lookup` (one exact fact from one authoritative \
page), `decision` (choose between options from a few primary sources), `deep` (open or contested; \
worth a full investigation). Say `why` it matters. Research already on file (below) needs none.
- Refuse, instead of planning, if the intent is illegal or would clearly harm others: answer \
`REFUSE: <why>` and nothing else.

## This Project's toolchain

build: `{build}`
full test suite: `{test}` (the runner also runs this after every task; nothing that passed \
before may fail)
lint: `{lint}`

## Research already on file

{notes}",
        name = toolchain.name,
        max = MAX_TASKS,
        programs = allowed_programs(toolchain)
            .iter()
            .map(|p| format!("`{p}`"))
            .collect::<Vec<_>>()
            .join(", "),
        test = toolchain.test,
        notes = if notes.trim().is_empty() {
            "(none)"
        } else {
            notes
        },
    )
}

/// What the planner's answer amounts to.
#[derive(Debug, PartialEq, Eq)]
pub enum PlannerReply {
    Plan(Plan),
    Refused(String),
    Invalid(String),
}

/// Find the plan in the planner's answer: the last ```toml block, or the
/// whole answer if it has none and parses.
pub fn read_planner_reply(answer: &str) -> PlannerReply {
    let trimmed = answer.trim();
    if let Some(why) = trimmed.strip_prefix("REFUSE:") {
        return PlannerReply::Refused(why.trim().to_string());
    }
    let block = last_fenced(trimmed, "toml").unwrap_or(trimmed);
    match Plan::parse(block) {
        Ok(plan) => PlannerReply::Plan(plan),
        Err(e) => PlannerReply::Invalid(e),
    }
}

fn last_fenced<'a>(text: &'a str, lang: &str) -> Option<&'a str> {
    let open = format!("```{lang}");
    let start = text.rfind(&open)? + open.len();
    let rest = &text[start..];
    let end = rest.find("```")?;
    Some(rest[..end].trim())
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOOD: &str = r#"
intent = "parse ISO 8601 durations"
toolchain = "rust"

[[task]]
id = "T1"
title = "Parse the date part"
why = "PnYnMnD"
research = ["Which designators may a duration carry?"]

[[task.check]]
kind = "test"
run = "cargo test --lib duration::date"
min_tests = 3

[[task]]
id = "T2"
title = "Parse the time part"
depends = ["T1"]

[[task.check]]
kind = "build"
run = "cargo build --all-targets"

[[task.check]]
kind = "test"
run = "cargo test --lib duration::time"
min_tests = 4
"#;

    #[test]
    fn a_good_plan_parses_validates_and_round_trips() {
        let plan = Plan::parse(GOOD).unwrap();
        assert_eq!(plan.tasks.len(), 2);
        assert_eq!(plan.tasks[1].checks[1].min_tests, Some(4));
        plan.validate(&Toolchain::rust()).unwrap();
        assert_eq!(Plan::parse(&plan.to_toml()).unwrap(), plan);
    }

    fn errors_for(edit: impl Fn(&mut Plan)) -> Vec<String> {
        let mut plan = Plan::parse(GOOD).unwrap();
        edit(&mut plan);
        plan.validate(&Toolchain::rust()).unwrap_err()
    }

    #[test]
    fn checks_that_could_lie_are_refused() {
        for run in [
            "cargo test || true",
            "cargo test; exit 0",
            "cargo test | tail -5",
            "cargo test $(echo --lib)",
            "cargo test &",
            "true",
            "echo ok && cargo test",
        ] {
            let errs = errors_for(|p| p.tasks[0].checks[0].run = run.into());
            assert!(!errs.is_empty(), "{run} was accepted");
        }
        assert!(Plan::parse(GOOD)
            .map(|mut p| {
                p.tasks[0].checks[0].run = "cargo build && cargo test --lib x".into();
                p.validate(&Toolchain::rust())
            })
            .unwrap()
            .is_ok());
    }

    /// A question says how much it deserves; a plain string is a decision.
    #[test]
    fn only_a_lookup_researches_without_thinking() {
        assert!(!Depth::Lookup.thinks());
        assert!(Depth::Decision.thinks());
        assert!(Depth::Deep.thinks());
    }

    #[test]
    fn research_items_carry_a_depth_and_default_to_decision() {
        let plan = Plan::parse(&GOOD.replace(
            "research = [\"Which designators may a duration carry?\"]",
            "research = [\"Plain question?\", { question = \"Is P1W2D legal?\", depth = \"lookup\", why = \"the week rule\" }, { question = \"Which date library?\", depth = \"deep\" }]",
        ))
        .unwrap();
        let r = &plan.tasks[0].research;
        assert_eq!(r.len(), 3);
        assert_eq!(
            (r[0].question(), r[0].depth()),
            ("Plain question?", Depth::Decision)
        );
        assert_eq!(
            (r[1].question(), r[1].depth()),
            ("Is P1W2D legal?", Depth::Lookup)
        );
        assert_eq!(r[2].depth(), Depth::Deep);
        assert_eq!(Depth::Deep.procedure(), "research");
        assert!(Depth::Lookup.minutes() < Depth::Decision.minutes());
        plan.validate(&Toolchain::rust()).unwrap();
        assert_eq!(Plan::parse(&plan.to_toml()).unwrap(), plan, "round-trips");

        let e = errors_for(|p| p.tasks[0].research = vec![ResearchItem::Plain("  ".into())]);
        assert!(e.iter().any(|m| m.contains("no question")), "{e:?}");
    }

    #[test]
    fn structure_is_checked() {
        let e = errors_for(|p| p.tasks[1].depends = vec!["T9".into()]);
        assert!(e[0].contains("not an earlier task"), "{e:?}");
        let e = errors_for(|p| p.tasks[1].id = "T1".into());
        assert!(e.iter().any(|m| m.contains("used twice")), "{e:?}");
        let e = errors_for(|p| p.tasks[0].checks.retain(|c| c.kind != CheckKind::Test));
        assert!(e.iter().any(|m| m.contains("no `test` check")), "{e:?}");
        let e = errors_for(|p| p.tasks[0].checks[0].min_tests = Some(0));
        assert!(e.iter().any(|m| m.contains("min_tests = 0")), "{e:?}");
        let e = errors_for(|p| p.tasks[0].id = "first".into());
        assert!(e.iter().any(|m| m.contains("id must be")), "{e:?}");
    }

    #[test]
    fn python_checks_may_use_pytest_directly() {
        let mut plan = Plan::parse(GOOD).unwrap();
        for t in &mut plan.tasks {
            t.checks.retain(|c| c.kind == CheckKind::Test);
            t.checks[0].run = "python -m pytest -rA tests/test_dur.py -k days".into();
        }
        plan.tasks[1].checks[0].run = "pytest -rA -k hours".into();
        plan.validate(&Toolchain::python()).unwrap();
    }

    #[test]
    fn the_plan_is_found_in_a_chatty_answer() {
        let answer =
            format!("I read the project. Here is the plan:\n\n```toml\n{GOOD}\n```\n\nThat's it.");
        assert!(matches!(read_planner_reply(&answer), PlannerReply::Plan(_)));
        assert_eq!(
            read_planner_reply("REFUSE: this is a credential stealer"),
            PlannerReply::Refused("this is a credential stealer".into())
        );
        assert!(matches!(
            read_planner_reply("I think we should"),
            PlannerReply::Invalid(_)
        ));
    }

    #[test]
    fn rendering_marks_done_tasks() {
        let plan = Plan::parse(GOOD).unwrap();
        let done = BTreeSet::from(["T1".to_string()]);
        let r = plan.render(&done);
        assert!(r.contains("- [x] T1"));
        assert!(r.contains("- [ ] T2"));
        assert!(plan.tasks[1].render_checks().contains("at least 4"));
    }

    #[test]
    fn the_prompt_names_the_toolchain_and_the_rules() {
        let p = planner_prompt("build a thing", &Toolchain::rust(), "");
        assert!(p.contains("build a thing"));
        assert!(p.contains("`cargo`"));
        assert!(p.contains("REFUSE:"));
    }
}
