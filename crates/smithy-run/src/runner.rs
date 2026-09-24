//! The Run itself: intent → plan → tasks → checkpoints → report.
//!
//! Everything the runner needs from outside is behind a trait, so a whole Run
//! can be driven in a test with a scripted model and a scripted Jev:
//! [`Agents`] builds Sessions, [`Judge`] answers Jev's questions, and
//! [`Notifier`] wakes the user.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use smithy_agent::jev::{self, Choice, Jev, NextMove};
use smithy_agent::{Outcome, Session};
use smithy_tools::research::{self, SourceStore};

use crate::check::{run_check, CheckKind, CheckOutcome, CheckSpec, CHECK_TIMEOUT};
use crate::decisions::{self, Answer, DecisionLog, Draft};
use crate::git::Git;
use crate::plan::{planner_prompt, read_planner_reply, Depth, Plan, PlannerReply, Task};
use crate::prompts;
use crate::report::{self, NoteLine};
use crate::runlog::{LogLevel, RunLog, SessionLabel};
use crate::state::{
    run_dir, unix_now, utc_date, write_atomic, Baseline, Ceilings, Flag, InFlight, NoteRecord,
    RunState, TaskStatus, Verdict,
};
use crate::toolchain::Toolchain;
use crate::unattended::DeniedLog;

/// Where the runner's own files live, relative to the Project.
pub const RUNS_DIR: &str = ".smithy/runs/";

/// Never stashed with an interrupted attempt: the Run's records and its
/// research Notes are worth keeping whatever became of the code.
const KEEP_ON_STASH: &str = ".smithy/";

/// What a Session is for. The [`Agents`] implementation decides its tools
/// and hooks from this (see [`crate::unattended`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Purpose {
    /// Reads the Project, may research; writes only research Notes.
    Plan,
    /// Builds one Task.
    Build { task: String },
    /// Researches one question; writes only under `.smithy/research/`.
    Research { task: Option<String>, depth: Depth },
}

#[async_trait]
pub trait Agents: Send + Sync {
    /// A fresh Session for `purpose`, whose hooks record refusals in `denied`.
    async fn session(&self, purpose: &Purpose, denied: DeniedLog) -> Result<Session, String>;
}

/// Jev's questions, as the runner asks them. [`Jev`] is the real one.
#[async_trait]
pub trait Judge: Send + Sync {
    async fn guardrail(&self, state: &str) -> Result<f64, String>;
    async fn needs_research(&self, state: &str) -> Result<f64, String>;
    async fn next_move(&self, state: &str, allowed: &[NextMove]) -> Result<Choice, String>;
    async fn weakened_tests(&self, state: &str) -> Result<f64, String>;
    async fn answered(&self, state: &str) -> Result<f64, String>;
}

#[async_trait]
impl Judge for Jev {
    async fn guardrail(&self, state: &str) -> Result<f64, String> {
        Jev::guardrail(self, state).await
    }
    async fn needs_research(&self, state: &str) -> Result<f64, String> {
        Jev::needs_research(self, state).await
    }
    async fn next_move(&self, state: &str, allowed: &[NextMove]) -> Result<Choice, String> {
        Jev::next_move(self, state, allowed).await
    }
    async fn weakened_tests(&self, state: &str) -> Result<f64, String> {
        Jev::weakened_tests(self, state).await
    }
    async fn answered(&self, state: &str) -> Result<f64, String> {
        Jev::answered(self, state).await
    }
}

/// No Jev: every question unanswered. The Guardrail fails closed on this, so
/// a Run without a key does not build.
pub struct NoJudge;

#[async_trait]
impl Judge for NoJudge {
    async fn guardrail(&self, _: &str) -> Result<f64, String> {
        Err("no Jev key".into())
    }
    async fn needs_research(&self, _: &str) -> Result<f64, String> {
        Err("no Jev key".into())
    }
    async fn next_move(&self, _: &str, _: &[NextMove]) -> Result<Choice, String> {
        Err("no Jev key".into())
    }
    async fn weakened_tests(&self, _: &str) -> Result<f64, String> {
        Err("no Jev key".into())
    }
    async fn answered(&self, _: &str) -> Result<f64, String> {
        Err("no Jev key".into())
    }
}

pub trait Notifier: Send + Sync {
    fn notify(&self, title: &str, body: &str);
}

/// Everything a Run borrows from its host.
#[derive(Clone)]
pub struct Deps {
    pub agents: Arc<dyn Agents>,
    pub judge: Arc<dyn Judge>,
    pub notifier: Arc<dyn Notifier>,
    /// Where saved pages are, for checking Notes. `None` skips verification.
    pub sources: Option<SourceStore>,
    /// Lines of progress for whoever is watching the terminal.
    pub progress: Arc<dyn Fn(&str) + Send + Sync>,
    /// How long the Guardrail keeps retrying an unreachable Jev before the
    /// Run fails closed.
    pub guardrail_patience: Duration,
    /// Watches every turn for loops and early stops, and logs each check to
    /// the Run's decisions. `None` without a key.
    pub supervisor: Option<Arc<Jev>>,
    /// How much of the Run to keep for review, and where. `None` is the
    /// default place outside the Project; see [`crate::runlog`].
    pub log_level: LogLevel,
    pub log_dir: Option<PathBuf>,
}

pub struct Runner {
    root: PathBuf,
    git: Git,
    state: RunState,
    plan: Option<Plan>,
    log: Arc<Mutex<DecisionLog>>,
    deps: Deps,
    denied: DeniedLog,
    started: Instant,
    elapsed_before: u64,
    /// Consecutive model-endpoint failures; two ends the Run.
    provider_failures: usize,
    runlog: Arc<RunLog>,
}

/// How a round of work ended.
enum RoundEnd {
    Passed,
    Failed(Vec<CheckOutcome>, String),
}

/// A Session and how much of its usage the Run has already counted.
struct Worker {
    session: Session,
    counted: smithy_agent::Usage,
    label: SessionLabel,
}

/// How an Attempt ended.
enum AttemptEnd {
    Done,
    /// Try again with a fresh Session.
    Handoff,
    Blocked(String),
    /// Stop the Run.
    Stop(Verdict),
}

impl Runner {
    /// Start a new Run of `intent` on its own branch.
    pub async fn start(
        root: &Path,
        intent: &str,
        ceilings: Ceilings,
        deps: Deps,
    ) -> Result<RunState, String> {
        let git = Git::open(root)?;
        let modified = git.modified()?;
        if !modified.is_empty() {
            return Err(format!(
                "the Project has uncommitted changes ({}); commit or stash them first — a Run \
                 branches from a clean tree so its commits are only its own",
                modified
                    .iter()
                    .take(5)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        let toolchain = Toolchain::for_project(root)?;
        let base = git.head()?;
        let id = new_run_id();
        let branch = format!("smithy/run-{id}");
        let preexisting = git.untracked()?;
        git.create_branch(&branch)?;

        let mut state = RunState::new(&id, intent, &branch, &base, toolchain);
        state.ceilings = ceilings;
        state.preexisting_untracked = preexisting;
        let dir = run_dir(root, &id);
        let log = DecisionLog::open(&dir.join("decisions.jsonl"));
        let mut runner = Runner {
            root: root.to_path_buf(),
            git,
            state,
            plan: None,
            log: Arc::new(Mutex::new(log)),
            deps,
            denied: Arc::new(Mutex::new(Vec::new())),
            started: Instant::now(),
            elapsed_before: 0,
            provider_failures: 0,
            runlog: Arc::new(RunLog::off()),
        };
        runner.open_log();
        runner.say(&format!("run {id} on {branch}"));
        runner.save();
        runner.drive().await;
        Ok(runner.state)
    }

    /// Carry on a Run: the newest one, or `id`. `allow` clears Guardrail flags.
    pub async fn resume(
        root: &Path,
        id: Option<&str>,
        allow: &[String],
        deps: Deps,
    ) -> Result<RunState, String> {
        let git = Git::open(root)?;
        let id = match id {
            Some(id) => id.to_string(),
            None => newest_run(&git).ok_or("no Run branch (smithy/run-*) to resume")?,
        };
        let branch = format!("smithy/run-{id}");
        if !git.branch_exists(&branch) {
            return Err(format!("no branch {branch}"));
        }
        if git.branch()? != branch {
            if !git.modified()?.is_empty() {
                return Err(format!(
                    "uncommitted changes on {}; commit or stash them before resuming {branch}",
                    git.branch()?
                ));
            }
            git.switch(&branch)?;
        }
        let dir = run_dir(root, &id);
        let mut state = RunState::load(&dir.join("state.json"))?;
        let plan = std::fs::read_to_string(dir.join("plan.toml"))
            .ok()
            .and_then(|t| Plan::parse(&t).ok());

        // Still in flight means the process died inside an Attempt. Its work
        // is kept, in a stash, and the Attempt counts.
        if let Some(inflight) = state.in_flight.take() {
            let label = format!(
                "smithy-run-{id}-crash-{}a{}",
                inflight.task, inflight.attempt
            );
            if git.stash(&label, &state.preexisting_untracked, KEEP_ON_STASH)? {
                state.stashes.push(label);
            }
            if let Some(t) = state.tasks.get_mut(&inflight.task) {
                t.status = TaskStatus::Pending;
            }
        }
        for a in allow {
            state.allowed.insert(a.clone());
            state.flags.retain(|f| &f.subject != a);
            // Naming a Task clears its flag, or gives a blocked one fresh
            // attempts: the user has looked at it and wants it tried again.
            if let Some(t) = state.tasks.get_mut(a) {
                if matches!(t.status, TaskStatus::Flagged | TaskStatus::Blocked) {
                    t.status = TaskStatus::Pending;
                    t.attempts = 0;
                    t.blocker = None;
                }
            }
        }
        state.verdict = None;
        state.consecutive_blocked = 0;
        let log = DecisionLog::open(&dir.join("decisions.jsonl"));
        let elapsed_before = state.elapsed;
        let mut runner = Runner {
            root: root.to_path_buf(),
            git,
            state,
            plan,
            log: Arc::new(Mutex::new(log)),
            deps,
            denied: Arc::new(Mutex::new(Vec::new())),
            started: Instant::now(),
            elapsed_before,
            provider_failures: 0,
            runlog: Arc::new(RunLog::off()),
        };
        runner.open_log();
        runner.say(&format!("resuming run {id}"));
        runner.save();
        runner.drive().await;
        Ok(runner.state)
    }

    // -----------------------------------------------------------------------
    // The Run
    // -----------------------------------------------------------------------

    async fn drive(&mut self) {
        let verdict = self.drive_inner().await;
        self.finish(verdict);
    }

    async fn drive_inner(&mut self) -> Verdict {
        if !self.state.allowed.contains("intent") {
            let state = jev::guardrail_state(&self.state.intent, None);
            if let Some(v) = self.guardrail("intent", &state, None).await {
                return v;
            }
        }
        if let Err(why) = preflight(&self.state.toolchain, &self.root) {
            return Verdict::Failed(why);
        }
        if self.state.baseline.is_none() {
            self.take_baseline();
            self.save();
        }
        if self.plan.is_none() {
            match self.make_plan().await {
                Ok(plan) => self.plan = Some(plan),
                Err(v) => return v,
            }
        }
        let plan = self.plan.clone().expect("planned");

        for task in &plan.tasks {
            let status = self.state.task(&task.id).status.clone();
            if matches!(status, TaskStatus::Done | TaskStatus::Blocked) {
                continue;
            }
            if let Some(v) = self.over_time() {
                return v;
            }
            if let Some(dep) = task.depends.iter().find(|d| {
                self.state
                    .tasks
                    .get(*d)
                    .is_none_or(|t| t.status != TaskStatus::Done)
            }) {
                let t = self.state.task(&task.id);
                t.status = TaskStatus::Blocked;
                t.blocker = Some(format!("depends on {dep}, which is not done"));
                self.say(&format!("{} blocked: depends on {dep}", task.id));
                self.save();
                continue;
            }
            if !self.state.allowed.contains(&task.id) {
                let state = jev::guardrail_state(&plan.intent, Some((&task.title, &task.why)));
                if let Some(v) = self.guardrail(&task.id, &state, Some(&task.id)).await {
                    self.state.task(&task.id).status = TaskStatus::Flagged;
                    return v;
                }
            }
            match self.work_task(&plan, task).await {
                Ok(()) => {}
                Err(v) => return v,
            }
            if self.state.consecutive_blocked >= self.state.ceilings.consecutive_blocked {
                return Verdict::Blocked(format!(
                    "{} tasks in a row could not be finished; the plan itself is the likelier \
                     problem",
                    self.state.consecutive_blocked
                ));
            }
        }
        let blocked: Vec<String> = plan
            .tasks
            .iter()
            .filter(|t| {
                self.state
                    .tasks
                    .get(&t.id)
                    .is_some_and(|s| s.status != TaskStatus::Done)
            })
            .map(|t| t.id.clone())
            .collect();
        if blocked.is_empty() {
            Verdict::Done
        } else {
            Verdict::Blocked(format!("{} not done", blocked.join(", ")))
        }
    }

    /// `Some(verdict)` stops the Run. Fails closed.
    async fn guardrail(
        &mut self,
        subject: &str,
        state: &str,
        task: Option<&str>,
    ) -> Option<Verdict> {
        let deadline = Instant::now() + self.deps.guardrail_patience;
        let answer = loop {
            match self.deps.judge.guardrail(state).await {
                Ok(p) => break Ok(p),
                Err(e) if Instant::now() >= deadline => break Err(e),
                Err(e) => {
                    self.say(&format!("guardrail unanswered ({e}); retrying"));
                    tokio::time::sleep(Duration::from_secs(30).min(self.deps.guardrail_patience))
                        .await;
                }
            }
        };
        let (flagged, p, action) = match &answer {
            Ok(p) if *p >= jev::GUARDRAIL_THRESHOLD => (true, Some(*p), "stop and wake"),
            Ok(p) => (false, Some(*p), "build"),
            Err(_) => (true, None, "stop: no answer, so no build"),
        };
        self.record(Draft {
            task,
            attempt: None,
            kind: "guardrail",
            state,
            answer: answer
                .clone()
                .map(Answer::Probability)
                .unwrap_or_else(Answer::Unavailable),
            threshold: Some(jev::GUARDRAIL_THRESHOLD),
            action,
        });
        if !flagged {
            return None;
        }
        let text = match &answer {
            Ok(p) => format!(
                "Jev judged this {:.0}% likely to be illegal or harmful to others. Nothing was built from here on.",
                p * 100.0
            ),
            Err(e) => format!("the guardrail could not be asked ({e}), and the Run does not build unchecked"),
        };
        self.state.flags.push(Flag {
            at: unix_now(),
            subject: subject.to_string(),
            kind: "guardrail".into(),
            probability: p,
            text: text.clone(),
        });
        Some(match answer {
            Ok(_) => Verdict::Flagged(format!("{subject}: {text}")),
            Err(_) => Verdict::Failed(text),
        })
    }

    fn take_baseline(&mut self) {
        let spec = CheckSpec {
            kind: CheckKind::Test,
            run: self.state.toolchain.test.clone(),
            min_tests: Some(0),
        };
        self.say("baseline: running the full suite");
        let out = run_check(&spec, &self.state.toolchain, &self.root, CHECK_TIMEOUT);
        self.log_check(None, &out);
        self.state.baseline = Some(match out.tests {
            Some(t) => Baseline {
                passed: t.passed,
                failed: t.failed,
                passed_names: t.passed_names,
                unavailable: None,
            },
            _ => Baseline {
                unavailable: Some(out.verdict),
                ..Default::default()
            },
        });
    }

    async fn make_plan(&mut self) -> Result<Plan, Verdict> {
        self.say("planning");
        let notes: String = research::list_notes(&self.root)
            .iter()
            .map(|n| format!("- `{}` — {}\n", n.path, n.question))
            .collect();
        let mut session = self.worker(&Purpose::Plan).await?;
        let mut message = planner_prompt(&self.state.intent, &self.state.toolchain, &notes);
        for round in 0..3 {
            let answer = match self.turn(&mut session, &message).await? {
                Some(a) => a,
                None => {
                    message = "Your turn ended before a plan. Answer now with the plan in one ```toml block.".into();
                    continue;
                }
            };
            match read_planner_reply(&answer) {
                PlannerReply::Refused(why) => {
                    self.state.flags.push(Flag {
                        at: unix_now(),
                        subject: "intent".into(),
                        kind: "planner refused".into(),
                        probability: None,
                        text: why.clone(),
                    });
                    return Err(Verdict::Flagged(format!("the planner refused: {why}")));
                }
                PlannerReply::Invalid(e) if round < 2 => {
                    message = format!("That plan could not be read: {e}\nAnswer again with the whole plan in one ```toml block.");
                }
                PlannerReply::Invalid(e) => {
                    return Err(Verdict::Failed(format!("no readable plan: {e}")))
                }
                PlannerReply::Plan(mut plan) => {
                    plan.intent = self.state.intent.clone();
                    plan.toolchain = self.state.toolchain.name.clone();
                    match plan.validate(&self.state.toolchain) {
                        Ok(()) => {
                            let dir = run_dir(&self.root, &self.state.id);
                            let _ = write_atomic(&dir.join("plan.toml"), &plan.to_toml());
                            self.state.planned = true;
                            for t in &plan.tasks {
                                self.state.task(&t.id);
                            }
                            self.plan = Some(plan.clone());
                            self.checkpoint(&format!(
                                "Plan: {}\n\n{}",
                                first_line(&self.state.intent),
                                plan.render(&Default::default())
                            ));
                            self.say(&format!("planned {} tasks", plan.tasks.len()));
                            return Ok(plan);
                        }
                        Err(errors) if round < 2 => {
                            message = format!(
                                "The plan breaks these rules:\n- {}\nFix all of them and answer with the whole plan again.",
                                errors.join("\n- ")
                            );
                        }
                        Err(errors) => {
                            return Err(Verdict::Failed(format!(
                                "the plan never validated: {}",
                                errors.join("; ")
                            )))
                        }
                    }
                }
            }
        }
        Err(Verdict::Failed("no plan after three tries".into()))
    }

    async fn work_task(&mut self, plan: &Plan, task: &Task) -> Result<(), Verdict> {
        let started = Instant::now();
        self.state.task(&task.id).status = TaskStatus::Active;
        self.save();

        // Research what the plan asked for. Jev does not decide that: its
        // "does this need outside sources?" is a probability, and it is
        // reported beside each Note (and, for a Task the plan gave no
        // questions, in the log and the Report) rather than turned into a
        // skip. The Run's research budget is what bounds research.
        let checks = task.render_checks();
        let mut questions: Vec<(String, Depth, Option<f64>)> = Vec::new();
        if task.research.is_empty() {
            let st = jev::research_state(&plan.intent, &task.title, &task.why, &checks);
            self.research_need(&task.id, &st, false).await;
        } else {
            for item in &task.research {
                let q = item.question().to_string();
                let need = if self.already_researched(&task.id, &q).is_some() {
                    None
                } else {
                    let why = format!("{}\nThe question: {q}", task.why);
                    let st = jev::research_state(&plan.intent, &task.title, &why, &checks);
                    self.research_need(&task.id, &st, true).await
                };
                questions.push((q, item.depth(), need));
            }
        }
        for (q, depth, need) in questions {
            // Researched by this Run already (a resume, or a crash after the
            // note was written): read it, whatever its status says. A second
            // pass cost the second real Run half an hour for nothing.
            if let Some(path) = self.already_researched(&task.id, &q) {
                self.say(&format!("research: already on file, {path}"));
                let t = self.state.task(&task.id);
                if !t.notes.contains(&path) {
                    t.notes.push(path);
                }
                continue;
            }
            if let Some(path) = self.research(&q, depth, need, Some(task)).await? {
                let t = self.state.task(&task.id);
                if !t.notes.contains(&path) {
                    t.notes.push(path);
                }
            }
        }

        let first_decision = self.next_seq();
        let mut end = AttemptEnd::Blocked("no attempt ran".into());
        while self.state.task(&task.id).attempts < self.state.ceilings.attempts_per_task {
            end = self.attempt(plan, task).await?;
            match end {
                AttemptEnd::Handoff => continue,
                _ => break,
            }
        }
        if matches!(end, AttemptEnd::Handoff) {
            end = AttemptEnd::Blocked(format!(
                "used its {} attempts",
                self.state.ceilings.attempts_per_task
            ));
        }
        let seconds = started.elapsed().as_secs();
        self.state.task(&task.id).seconds += seconds;
        match end {
            AttemptEnd::Done => {
                self.state.consecutive_blocked = 0;
                self.settle_outcomes(&task.id, first_decision, true);
                Ok(())
            }
            AttemptEnd::Blocked(why) => {
                self.say(&format!("{} blocked: {why}", task.id));
                let label = format!("smithy-run-{}-{}-blocked", self.state.id, task.id);
                if self
                    .git
                    .stash(&label, &self.state.preexisting_untracked, KEEP_ON_STASH)
                    .unwrap_or(false)
                {
                    self.state.stashes.push(label);
                }
                let t = self.state.task(&task.id);
                t.status = TaskStatus::Blocked;
                t.blocker = Some(why);
                self.state.consecutive_blocked += 1;
                self.settle_outcomes(&task.id, first_decision, false);
                self.checkpoint(&format!("{}: blocked — {}", task.id, task.title));
                Ok(())
            }
            AttemptEnd::Stop(v) => {
                let label = format!("smithy-run-{}-{}-stopped", self.state.id, task.id);
                if self
                    .git
                    .stash(&label, &self.state.preexisting_untracked, KEEP_ON_STASH)
                    .unwrap_or(false)
                {
                    self.state.stashes.push(label);
                }
                Err(v)
            }
            AttemptEnd::Handoff => unreachable!(),
        }
    }

    async fn attempt(&mut self, plan: &Plan, task: &Task) -> Result<AttemptEnd, Verdict> {
        let attempt = {
            let t = self.state.task(&task.id);
            t.attempts += 1;
            t.attempts
        };
        let max_attempts = self.state.ceilings.attempts_per_task;
        let last_attempt = attempt >= max_attempts;
        self.state.in_flight = Some(InFlight {
            task: task.id.clone(),
            attempt,
            since: unix_now(),
        });
        self.save();
        self.say(&format!(
            "{} attempt {attempt}/{max_attempts}: {}",
            task.id, task.title
        ));

        let task_start = self.git.head().map_err(Verdict::Failed)?;
        let mut session = self
            .worker(&Purpose::Build {
                task: task.id.clone(),
            })
            .await?;
        let own = self.state.task(&task.id).notes.clone();
        let notes: Vec<(String, String)> = self
            .state
            .notes
            .iter()
            .filter(|n| n.task.as_deref() == Some(task.id.as_str()) || own.contains(&n.path))
            .map(|n| (n.path.clone(), n.question.clone()))
            .collect();
        let handoff = self.state.task(&task.id).handoff.clone();
        let mut message = prompts::task_prompt(
            plan,
            task,
            &self.state.done(),
            &self.state.toolchain,
            &notes,
            handoff.as_deref(),
            attempt,
            max_attempts,
        );
        let mut rounds: Vec<(String, String)> = Vec::new();
        let mut signatures: Vec<String> = Vec::new();
        let mut researched = false;
        let mut cheat_warnings = 0;
        let max_rounds = self.state.ceilings.rounds_per_attempt;

        for round in 1..=max_rounds {
            if let Some(v) = self.over_time() {
                return Ok(AttemptEnd::Stop(v));
            }
            self.turn(&mut session, &message).await?;
            self.protect_plan();

            let (failed, note) = match self
                .round_checks(task, &task_start, &mut cheat_warnings)
                .await
            {
                RoundEnd::Passed => {
                    {
                        let t = self.state.task(&task.id);
                        t.status = TaskStatus::Done;
                    }
                    self.state.in_flight = None;
                    let sha = self.checkpoint(&format!(
                        "{}: {}\n\n{}\n\nSmithy run {} · attempt {attempt}",
                        task.id,
                        task.title,
                        self.state
                            .tasks
                            .get(&task.id)
                            .map(|t| t
                                .last_checks
                                .iter()
                                .map(|c| format!("✓ {} — {}", c.spec.run, c.verdict))
                                .collect::<Vec<_>>()
                                .join("\n"))
                            .unwrap_or_default(),
                        self.state.id
                    ));
                    // Recorded now, committed with the next checkpoint: a
                    // commit cannot contain its own hash.
                    self.state.task(&task.id).commit = sha;
                    self.save();
                    self.say(&format!("{} done", task.id));
                    return Ok(AttemptEnd::Done);
                }
                RoundEnd::Failed(failed, note) => (failed, note),
            };
            if cheat_warnings >= 2 {
                return Ok(AttemptEnd::Blocked(
                    "weakened tests that predate the Run, twice; the work was set aside, not landed".into(),
                ));
            }
            let first = failed.first().cloned();
            if let Some(f) = &first {
                rounds.push((format!("{} — {}", f.spec.run, f.verdict), f.excerpt.clone()));
                signatures.push(f.signature());
            }
            self.say(&format!(
                "{} round {round}: {}",
                task.id,
                first
                    .as_ref()
                    .map(|f| f.verdict.as_str())
                    .unwrap_or("failed")
            ));

            let ctx = context_percent(&session.session);
            let same_three = signatures.len() >= 3
                && signatures[signatures.len() - 3..]
                    .iter()
                    .all(|s| s == signatures.last().unwrap());
            let mv = self
                .next_move(
                    task,
                    attempt,
                    &rounds,
                    ctx,
                    same_three,
                    last_attempt,
                    researched,
                    round == max_rounds,
                )
                .await;
            self.say(&format!("{} next: {}", task.id, mv.name()));
            let failed_refs: Vec<&CheckOutcome> = failed.iter().collect();
            match mv {
                NextMove::Continue => {
                    message = prompts::feedback(round, max_rounds, &failed_refs, &note);
                }
                NextMove::Compact => {
                    let focus = format!(
                        "Task {} — {}; the failing checks and what has been tried",
                        task.id, task.title
                    );
                    let compacted = session.session.compact(&focus, None).await;
                    self.bill(&mut session);
                    if let Err(e) = compacted {
                        self.say(&format!("compact failed ({e}); handing off instead"));
                        return Ok(self.handoff(task, &mut session).await);
                    }
                    message = prompts::feedback(round, max_rounds, &failed_refs, &note);
                }
                NextMove::Research => {
                    researched = true;
                    let q = first
                        .as_ref()
                        .map(|f| prompts::failure_question(task, f))
                        .unwrap_or_else(|| prompts::implied_question(task));
                    let extra = match self.research(&q, Depth::Decision, None, Some(task)).await? {
                        Some(path) => {
                            self.state.task(&task.id).notes.push(path.clone());
                            format!("{note}\n\nResearch on this failure is in `{path}` — read it first.")
                        }
                        None => note.clone(),
                    };
                    message = prompts::feedback(round, max_rounds, &failed_refs, &extra);
                }
                NextMove::Handoff => return Ok(self.handoff(task, &mut session).await),
                NextMove::Block => {
                    return Ok(AttemptEnd::Blocked(format!(
                        "given up after {round} round(s): {}",
                        first.map(|f| f.verdict).unwrap_or_default()
                    )))
                }
                NextMove::Escalate => {
                    let why = format!(
                        "{} — {}: {}",
                        task.id,
                        task.title,
                        first
                            .map(|f| format!("{} ({})", f.verdict, f.spec.run))
                            .unwrap_or_default()
                    );
                    self.state.flags.push(Flag {
                        at: unix_now(),
                        subject: task.id.clone(),
                        kind: "escalate".into(),
                        probability: None,
                        text: why.clone(),
                    });
                    self.state.in_flight = None;
                    return Ok(AttemptEnd::Stop(Verdict::Escalated(why)));
                }
            }
        }
        Ok(self.handoff(task, &mut session).await)
    }

    /// Checks, the full suite against the Baseline, then the cheat rules.
    async fn round_checks(
        &mut self,
        task: &Task,
        task_start: &str,
        cheat_warnings: &mut usize,
    ) -> RoundEnd {
        let tc = self.state.toolchain.clone();
        let mut outcomes = Vec::new();
        for spec in &task.checks {
            let o = run_check(spec, &tc, &self.root, CHECK_TIMEOUT);
            self.log_check(Some(&task.id), &o);
            let passed = o.passed;
            outcomes.push(o);
            if !passed {
                break;
            }
        }
        let all_passed = outcomes.iter().all(|o| o.passed);
        if all_passed && !task.checks.iter().any(|c| c.run == tc.test) {
            let suite = CheckSpec {
                kind: CheckKind::Test,
                run: tc.test.clone(),
                min_tests: Some(0),
            };
            // Judged against the Baseline, not on its own: failures that were
            // there before the Run are not this Task's to fix.
            let mut o = run_check(&suite, &tc, &self.root, CHECK_TIMEOUT);
            match self.regression(&o) {
                Some(why) => {
                    o.passed = false;
                    o.verdict = why;
                }
                None => {
                    o.passed = true;
                    o.verdict = format!("{} (no regression from the Baseline)", o.verdict);
                }
            }
            self.log_check(Some(&task.id), &o);
            outcomes.push(o);
        }
        self.state.task(&task.id).last_checks = outcomes.clone();
        self.save();
        let failed: Vec<CheckOutcome> = outcomes.iter().filter(|o| !o.passed).cloned().collect();
        if !failed.is_empty() {
            return RoundEnd::Failed(failed, String::new());
        }

        // Cheat rules, then Jev on what the rules let through.
        let report = match crate::cheat::inspect(&self.git, &tc, &self.state.base, task_start) {
            Ok(r) => r,
            Err(e) => {
                self.say(&format!("cheat check could not run: {e}"));
                return RoundEnd::Passed;
            }
        };
        let mut problems = report.violations.clone();
        if problems.is_empty() && !report.suspicious.is_empty() {
            let st = jev::cheat_state(&format!("{} — {}", task.id, task.title), &report.diff);
            let answer = self.deps.judge.weakened_tests(&st).await;
            let weakened = matches!(answer, Ok(p) if p >= jev::CHEAT_THRESHOLD);
            self.record(Draft {
                task: Some(&task.id),
                attempt: None,
                kind: "cheat",
                state: &st,
                answer: answer
                    .map(Answer::Probability)
                    .unwrap_or_else(Answer::Unavailable),
                threshold: Some(jev::CHEAT_THRESHOLD),
                action: if weakened {
                    "refuse the round"
                } else {
                    "accept"
                },
            });
            if weakened {
                problems.push(format!(
                    "the changes to {} look like weakening them to pass",
                    report.suspicious.join(", ")
                ));
            }
        }
        if problems.is_empty() {
            return RoundEnd::Passed;
        }
        *cheat_warnings += 1;
        let note = format!(
            "The checks pass, but not honestly: {}. Tests that existed before this Run must keep \
             testing what they tested. Restore them (their original text is at commit {}; `git \
             show {}:<path>` is allowed) and make the code pass them instead.",
            problems.join("; "),
            &self.state.base[..self.state.base.len().min(10)],
            &self.state.base[..self.state.base.len().min(10)],
        );
        let flagged = CheckOutcome {
            spec: CheckSpec {
                kind: CheckKind::Command,
                run: "cheat check".into(),
                min_tests: None,
            },
            passed: false,
            verdict: problems.join("; "),
            tests: None,
            excerpt: report.diff.clone(),
            seconds: 0,
        };
        self.state.task(&task.id).last_checks.push(flagged.clone());
        RoundEnd::Failed(vec![flagged], note)
    }

    /// Why the full suite fails the Baseline, if it does.
    fn regression(&self, suite: &CheckOutcome) -> Option<String> {
        let baseline = self.state.baseline.as_ref()?;
        if baseline.unavailable.is_some() {
            // Nothing ran at the start, so everything that runs now is the
            // Run's own and must pass.
            return (!suite.passed).then(|| format!("the full suite fails: {}", suite.verdict));
        }
        let Some(tests) = &suite.tests else {
            return Some(format!("the full suite did not run: {}", suite.verdict));
        };
        let broken: Vec<&String> = baseline
            .passed_names
            .intersection(&tests.failed_names)
            .collect();
        if !broken.is_empty() {
            return Some(format!(
                "{} test(s) that passed before the Run now fail: {}",
                broken.len(),
                broken
                    .iter()
                    .take(5)
                    .map(|s| s.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        if tests.passed < baseline.passed {
            return Some(format!(
                "{} tests pass now, fewer than the {} that passed before the Run",
                tests.passed, baseline.passed
            ));
        }
        if tests.failed > baseline.failed {
            return Some(format!(
                "the full suite has {} failure(s), more than the {} before the Run",
                tests.failed, baseline.failed
            ));
        }
        None
    }

    #[allow(clippy::too_many_arguments)]
    async fn next_move(
        &mut self,
        task: &Task,
        attempt: usize,
        rounds: &[(String, String)],
        ctx: u32,
        same_three: bool,
        last_attempt: bool,
        researched: bool,
        last_round: bool,
    ) -> NextMove {
        let forced = if ctx >= 80 {
            Some((NextMove::Compact, format!("context {ctx}% full")))
        } else if last_round {
            Some(if last_attempt {
                (NextMove::Block, "last round of the last attempt".into())
            } else {
                (NextMove::Handoff, "last round of this attempt".into())
            })
        } else {
            None
        };
        let st = jev::next_state(
            &format!("{} — {}", task.id, task.title),
            attempt,
            self.state.ceilings.attempts_per_task,
            rounds,
            ctx,
        );
        if let Some((mv, why)) = forced {
            self.record(Draft {
                task: Some(&task.id),
                attempt: Some(attempt),
                kind: "next",
                state: &st,
                answer: Answer::Rule(why),
                threshold: None,
                action: mv.name(),
            });
            return mv;
        }
        let allowed: Vec<NextMove> = NextMove::ALL
            .into_iter()
            .filter(|m| match m {
                NextMove::Continue => !same_three,
                NextMove::Handoff => !last_attempt,
                NextMove::Research => !researched,
                NextMove::Compact => ctx >= 40,
                _ => true,
            })
            .collect();
        let default = if allowed.contains(&NextMove::Continue) {
            NextMove::Continue
        } else if allowed.contains(&NextMove::Handoff) {
            NextMove::Handoff
        } else {
            NextMove::Block
        };
        let answer = self.deps.judge.next_move(&st, &allowed).await;
        let mv = match &answer {
            Ok(c) if c.confidence >= jev::NEXT_CONFIDENCE_FLOOR => NextMove::from_name(&c.pick)
                .filter(|m| allowed.contains(m))
                .unwrap_or(default),
            _ => default,
        };
        self.record(Draft {
            task: Some(&task.id),
            attempt: Some(attempt),
            kind: "next",
            state: &st,
            answer: match answer {
                Ok(c) => Answer::Choice {
                    pick: c.pick,
                    confidence: c.confidence,
                    probabilities: c.probabilities,
                },
                Err(e) => Answer::Unavailable(e),
            },
            threshold: Some(jev::NEXT_CONFIDENCE_FLOOR),
            action: mv.name(),
        });
        mv
    }

    async fn handoff(&mut self, task: &Task, session: &mut Worker) -> AttemptEnd {
        let notes = self
            .turn(session, prompts::HANDOFF_REQUEST)
            .await
            .ok()
            .flatten()
            .unwrap_or_default();
        self.drain_denied();
        let t = self.state.task(&task.id);
        t.handoff = Some(notes);
        t.status = TaskStatus::Pending;
        self.state.in_flight = None;
        self.save();
        AttemptEnd::Handoff
    }

    /// Jev's "does this need outside sources?", recorded and returned. A
    /// report, not a gate: nothing is skipped or added on it.
    async fn research_need(&mut self, task: &str, state: &str, asked: bool) -> Option<f64> {
        let answer = self.deps.judge.needs_research(state).await;
        let p = answer.as_ref().ok().copied();
        if !asked {
            if let Some(p) = p.filter(|p| *p >= jev::RESEARCH_THRESHOLD) {
                self.say(&format!(
                    "{task}: the plan asked no research questions; Jev puts the need for \
                     outside sources at {p:.2}"
                ));
            }
        }
        self.record(Draft {
            task: Some(task),
            attempt: None,
            kind: "research",
            state,
            answer: answer
                .map(Answer::Probability)
                .unwrap_or_else(Answer::Unavailable),
            threshold: None,
            action: if asked {
                "report: researched as the plan asked"
            } else {
                "report: the plan asked for none"
            },
        });
        p
    }

    /// The Note this Run already wrote for `question` on `task`, if it is
    /// still on disk.
    fn already_researched(&self, task: &str, question: &str) -> Option<String> {
        self.state
            .notes
            .iter()
            .find(|n| {
                n.task.as_deref() == Some(task)
                    && n.question == question
                    && self.root.join(&n.path).is_file()
            })
            .map(|n| n.path.clone())
    }

    /// Research one question into a Note. `None` when no Note came of it.
    async fn research(
        &mut self,
        question: &str,
        depth: Depth,
        need: Option<f64>,
        task: Option<&Task>,
    ) -> Result<Option<String>, Verdict> {
        let task_id = task.map(|t| t.id.clone());
        // Something already on file may answer it.
        if let Some((overlap, meta)) = research::find_notes(&self.root, question, 1)
            .into_iter()
            .next()
        {
            if overlap >= 0.6 && meta.status.contains("verified") {
                if let Ok(text) = std::fs::read_to_string(self.root.join(&meta.path)) {
                    let st = jev::answered_state(question, &text);
                    let answer = self.deps.judge.answered(&st).await;
                    let reuse = matches!(answer, Ok(p) if p >= jev::ANSWERED_THRESHOLD);
                    self.record(Draft {
                        task: task_id.as_deref(),
                        attempt: None,
                        kind: "answered",
                        state: &st,
                        answer: answer
                            .clone()
                            .map(Answer::Probability)
                            .unwrap_or_else(Answer::Unavailable),
                        threshold: Some(jev::ANSWERED_THRESHOLD),
                        action: if reuse {
                            "reuse the note on file"
                        } else {
                            "research anew"
                        },
                    });
                    if reuse {
                        self.say(&format!("research: reusing {}", meta.path));
                        let check = self.check_note(&text);
                        self.state.notes.push(NoteRecord {
                            path: meta.path.clone(),
                            question: question.to_string(),
                            task: task_id.clone(),
                            verified: check.as_ref().map(|c| c.verified()).unwrap_or(0),
                            findings: check.as_ref().map(|c| c.findings.len()).unwrap_or(0),
                            answered: answer.ok(),
                            reused: true,
                            need,
                        });
                        return Ok(Some(meta.path));
                    }
                }
            }
        }

        let budget = self.state.ceilings.research_budget();
        if self.state.research_seconds >= budget {
            self.say(&format!(
                "research skipped: the Run's {} minutes of research are spent",
                budget / 60
            ));
            self.record(Draft {
                task: task_id.as_deref(),
                attempt: None,
                kind: "research",
                state: question,
                answer: Answer::Rule("research budget spent".into()),
                threshold: None,
                action: "build without it",
            });
            return Ok(None);
        }
        let started = Instant::now();
        let out = self.run_research(question, depth, need, task).await;
        self.state.research_seconds += started.elapsed().as_secs();
        self.save();
        out
    }

    async fn run_research(
        &mut self,
        question: &str,
        depth: Depth,
        need: Option<f64>,
        task: Option<&Task>,
    ) -> Result<Option<String>, Verdict> {
        let task_id = task.map(|t| t.id.clone());
        self.say(&format!("research ({}): {question}", depth.name()));
        let path = prompts::note_path(&utc_date(unix_now()), task_id.as_deref(), question);
        // The depth picks the method: pointed research for a lookup or a
        // decision, the full adversarial /research for what is open.
        let procedure = smithy_agent::load_skill(&self.root, depth.procedure())
            .or_else(|| smithy_agent::load_skill(&self.root, "research"))
            .map(|s| s.injection())
            .unwrap_or_default();
        let mut session = self
            .worker(&Purpose::Research {
                task: task_id.clone(),
                depth,
            })
            .await?;
        session
            .session
            .observe(Arc::new(crate::observers::WriteTheNote::new(
                self.root.join(&path),
                depth.draft_by_step(),
            )));
        let prompt = prompts::research_prompt(
            &procedure,
            question,
            task.map(|t| (t.id.as_str(), t.title.as_str())),
            &path,
        );
        self.turn(&mut session, &prompt).await?;

        let mut answered = None;
        for pass in 0..2 {
            let Ok(text) = std::fs::read_to_string(self.root.join(&path)) else {
                if pass == 0 {
                    self.turn(
                        &mut session,
                        &format!("There is no note at `{path}`. Write it now, then cite_check it."),
                    )
                    .await?;
                    continue;
                }
                self.say("research produced no note");
                return Ok(None);
            };
            if let Some(check) = self.check_note(&text) {
                let marked = research::annotate(&text, &check);
                if marked != text {
                    let _ = std::fs::write(self.root.join(&path), &marked);
                }
                if check.verified() == 0 {
                    self.say(&format!("research: no finding in {path} could be verified"));
                }
            }
            let text = std::fs::read_to_string(self.root.join(&path)).unwrap_or(text);
            let st = jev::answered_state(question, &text);
            let answer = self.deps.judge.answered(&st).await;
            answered = answer.clone().ok();
            self.record(Draft {
                task: task_id.as_deref(),
                attempt: None,
                kind: "answered",
                state: &st,
                answer: answer
                    .map(Answer::Probability)
                    .unwrap_or_else(Answer::Unavailable),
                threshold: None,
                // Reported beside the Note, not acted on: the Note is kept
                // either way, and the Task's own tests are the ground truth.
                action: "report",
            });
            break;
        }
        let text = std::fs::read_to_string(self.root.join(&path)).unwrap_or_default();
        let check = self.check_note(&text);
        self.state.notes.push(NoteRecord {
            path: path.clone(),
            question: question.to_string(),
            task: task_id,
            verified: check.as_ref().map(|c| c.verified()).unwrap_or(0),
            findings: check.as_ref().map(|c| c.findings.len()).unwrap_or(0),
            answered,
            reused: false,
            need,
        });
        self.save();
        Ok(Some(path))
    }

    fn check_note(&self, text: &str) -> Option<research::NoteCheck> {
        let store = self.deps.sources.as_ref()?;
        Some(research::check_note(text, store, &self.root))
    }

    // -----------------------------------------------------------------------
    // Plumbing
    // -----------------------------------------------------------------------

    async fn worker(&mut self, purpose: &Purpose) -> Result<Worker, Verdict> {
        let session = self
            .deps
            .agents
            .session(purpose, self.denied.clone())
            .await
            .map_err(|e| Verdict::Failed(format!("could not start a Session: {e}")))?;
        let mut session = session;
        if let Some(jev) = &self.deps.supervisor {
            let log = self.log.clone();
            let task = match purpose {
                Purpose::Build { task } => Some(task.clone()),
                Purpose::Research { task, .. } => task.clone(),
                Purpose::Plan => None,
            };
            let sink: Arc<jev::SupervisorLog> = Arc::new(move |e: jev::SupervisorEvent| {
                if let Ok(mut log) = log.lock() {
                    log.record(Draft {
                        task: task.as_deref(),
                        attempt: None,
                        kind: e.kind,
                        state: &e.state,
                        answer: e
                            .answer
                            .map(Answer::Probability)
                            .unwrap_or_else(Answer::Unavailable),
                        threshold: Some(e.threshold),
                        action: e.action,
                    });
                }
            });
            session.observe(Arc::new(jev::Supervisor::new(jev.clone()).with_log(sink)));
        }
        let name = match purpose {
            Purpose::Plan => "plan".to_string(),
            Purpose::Build { task } => format!(
                "build-{task}-a{}",
                self.state.tasks.get(task).map(|t| t.attempts).unwrap_or(0)
            ),
            Purpose::Research { task, depth } => format!(
                "research-{}-{}",
                task.as_deref().unwrap_or("run"),
                depth.name()
            ),
        };
        let label = self.runlog.session_started(&name);
        Ok(Worker {
            session,
            counted: smithy_agent::Usage::default(),
            label,
        })
    }

    /// The Run's log, in the directory the state already names (a resume
    /// keeps writing where the Run started), or the one `Deps` asks for.
    fn open_log(&mut self) {
        let dir = self
            .state
            .log_dir
            .clone()
            .map(PathBuf::from)
            .or_else(|| self.deps.log_dir.as_ref().map(|d| d.join(&self.state.id)))
            .or_else(|| RunLog::default_dir(&self.root, &self.state.id));
        let Some(dir) = dir else { return };
        let log = RunLog::new(self.deps.log_level, &dir);
        self.state.log_dir = log.dir().map(|d| d.to_string_lossy().into_owned());
        log.event(
            "run",
            serde_json::json!({ "id": self.state.id, "level": self.deps.log_level }),
        );
        self.runlog = Arc::new(log);
    }

    /// A Check's result in the log, with how long it took.
    fn log_check(&self, task: Option<&str>, o: &CheckOutcome) {
        self.runlog.event(
            "check",
            serde_json::json!({
                "task": task, "run": o.spec.run, "passed": o.passed, "verdict": o.verdict,
                "seconds": o.seconds,
                "tests_passed": o.tests.as_ref().map(|t| t.passed),
                "tests_failed": o.tests.as_ref().map(|t| t.failed),
            }),
        );
    }

    /// Add what this Session spent since it was last counted.
    fn bill(&mut self, w: &mut Worker) {
        let now = w.session.usage();
        let delta = smithy_agent::Usage {
            prompt_tokens: now.prompt_tokens - w.counted.prompt_tokens,
            completion_tokens: now.completion_tokens - w.counted.completion_tokens,
            cached_tokens: now.cached_tokens - w.counted.cached_tokens,
            reasoning_tokens: now.reasoning_tokens - w.counted.reasoning_tokens,
            requests: now.requests - w.counted.requests,
        };
        self.state.usage.add(&delta);
        w.counted = now;
    }

    /// One turn. `Ok(None)` when it stopped without an answer; `Err` ends the
    /// Run when the endpoint has failed twice running.
    async fn turn(
        &mut self,
        session: &mut Worker,
        message: &str,
    ) -> Result<Option<String>, Verdict> {
        let sink = self.runlog.sink(&session.label);
        let result = session.session.run_turn(message, Some(&sink)).await;
        self.runlog
            .save_session(&session.label, &session.session, &self.root);
        self.bill(session);
        self.drain_denied();
        match result {
            Ok(Outcome::Answer(a)) => {
                self.provider_failures = 0;
                Ok(Some(a))
            }
            Ok(Outcome::Stopped(reason)) => {
                self.provider_failures = 0;
                self.say(&format!("turn stopped: {reason}"));
                Ok(None)
            }
            Err(e) => {
                self.provider_failures += 1;
                self.say(&format!("model error: {e}"));
                if self.provider_failures >= 2 {
                    Err(Verdict::Failed(format!(
                        "the model endpoint stopped answering: {e}"
                    )))
                } else {
                    Ok(None)
                }
            }
        }
    }

    fn drain_denied(&mut self) {
        if let Ok(mut d) = self.denied.lock() {
            self.state.denied.append(&mut d);
        }
    }

    /// The plan is the contract: if the model touched it, put it back.
    fn protect_plan(&mut self) {
        let rel = format!("{RUNS_DIR}{}/plan.toml", self.state.id);
        if let (Some(committed), Ok(now)) = (
            self.git.show("HEAD", &rel),
            std::fs::read_to_string(self.root.join(&rel)),
        ) {
            if committed != now {
                let _ = std::fs::write(self.root.join(&rel), committed);
                self.say("the plan was edited during a turn; restored");
            }
        }
    }

    fn over_time(&self) -> Option<Verdict> {
        let spent = self.elapsed();
        (spent >= self.state.ceilings.hours * 3600)
            .then(|| Verdict::Ceiling(format!("{}h of work", self.state.ceilings.hours)))
    }

    fn elapsed(&self) -> u64 {
        self.elapsed_before + self.started.elapsed().as_secs()
    }

    fn record(&self, draft: Draft<'_>) -> usize {
        self.log.lock().map(|mut l| l.record(draft)).unwrap_or(0)
    }

    fn next_seq(&self) -> usize {
        let (d, _) = decisions::read(&self.decisions_path());
        d.iter().map(|d| d.seq + 1).max().unwrap_or(0)
    }

    /// What a Task's end says about the choices made during it.
    fn settle_outcomes(&self, task: &str, from: usize, done: bool) {
        let (ds, _) = decisions::read(&self.decisions_path());
        let Ok(mut log) = self.log.lock() else { return };
        for d in ds
            .iter()
            .filter(|d| d.seq >= from && d.task.as_deref() == Some(task))
        {
            match d.kind.as_str() {
                "next" if done => log.outcome(d.seq, Some(true), "the task's checks passed later"),
                "next" => log.outcome(d.seq, None, "the task ended blocked"),
                "research" if done => log.outcome(d.seq, None, "the task's checks passed"),
                _ => {}
            }
        }
    }

    fn decisions_path(&self) -> PathBuf {
        run_dir(&self.root, &self.state.id).join("decisions.jsonl")
    }

    fn say(&self, line: &str) {
        self.runlog
            .event("progress", serde_json::json!({ "text": line }));
        (self.deps.progress)(line);
    }

    /// State and Report to disk (not committed).
    fn save(&mut self) {
        self.state.elapsed = self.elapsed();
        let dir = run_dir(&self.root, &self.state.id);
        let _ = self.state.save(&dir.join("state.json"));
        let (ds, os) = decisions::read(&dir.join("decisions.jsonl"));
        let notes: Vec<NoteLine> = self
            .state
            .notes
            .iter()
            .map(|n| NoteLine {
                path: n.path.clone(),
                question: n.question.clone(),
                verified: n.verified,
                findings: n.findings,
                answered: n.answered,
                need: n.need,
            })
            .collect();
        let text = report::render(&self.state, self.plan.as_ref(), &ds, &os, &notes);
        let _ = write_atomic(&dir.join("REPORT.md"), &text);
    }

    /// Save, then commit everything. The commit's sha, if it landed.
    fn checkpoint(&mut self, message: &str) -> Option<String> {
        self.save();
        match self
            .git
            .checkpoint(message, &self.state.preexisting_untracked)
        {
            Ok(sha) => Some(sha),
            Err(e) => {
                self.say(&format!("checkpoint failed: {e}"));
                None
            }
        }
    }

    fn finish(&mut self, verdict: Verdict) {
        self.state.verdict = Some(verdict.clone());
        // Ended inside an Attempt: its half-done work is set aside, not
        // committed as though it were a checkpoint.
        if let Some(f) = self.state.in_flight.take() {
            let label = format!(
                "smithy-run-{}-{}a{}-interrupted",
                self.state.id, f.task, f.attempt
            );
            if self
                .git
                .stash(&label, &self.state.preexisting_untracked, KEEP_ON_STASH)
                .unwrap_or(false)
            {
                self.state.stashes.push(label);
            }
        }
        let headline = verdict.headline();
        self.say(&headline);
        self.checkpoint(&format!("Run {}: {headline}", self.state.id));
        let report = run_dir(&self.root, &self.state.id).join("REPORT.md");
        let done = self.state.done().len();
        let total = self.plan.as_ref().map(|p| p.tasks.len()).unwrap_or(0);
        self.deps.notifier.notify(
            &format!("Smithy run {}", self.state.id),
            &format!("{headline}\n{done}/{total} tasks · {}", report.display()),
        );
    }
}

/// Every program the toolchain's commands start with, found on the PATH the
/// Checks will run with. The first real Run spent an hour planning,
/// researching and building before `exit 127` from `cargo build` said the
/// shell could not see cargo; this says it in a second.
pub fn preflight(toolchain: &Toolchain, root: &Path) -> Result<(), String> {
    let mut programs: Vec<String> = [
        &toolchain.build,
        &Some(toolchain.test.clone()),
        &toolchain.lint,
    ]
    .into_iter()
    .flatten()
    .flat_map(|cmd| {
        cmd.split("&&")
            .filter_map(|p| p.split_whitespace().next().map(str::to_string))
            .collect::<Vec<_>>()
    })
    .collect();
    programs.sort();
    programs.dedup();
    let missing: Vec<String> = programs
        .into_iter()
        .filter(|p| {
            let probe = format!("command -v {p}");
            smithy_tools::tools::bash::run_captured(&probe, root, Duration::from_secs(30))
                .map(|c| !c.success())
                .unwrap_or(true)
        })
        .collect();
    if missing.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "the Checks need {} but the shell cannot find {} on its PATH; start the Run from a \
             shell where {} runs",
            missing.join(", "),
            if missing.len() == 1 { "it" } else { "them" },
            missing
                .iter()
                .map(|m| format!("`{m}`"))
                .collect::<Vec<_>>()
                .join(" and ")
        ))
    }
}

fn context_percent(session: &Session) -> u32 {
    let hard = session.limits().context_hard.max(1);
    ((session.last_prompt_tokens().max(0) * 100) / hard).min(999) as u32
}

fn first_line(s: &str) -> String {
    let l = s.lines().next().unwrap_or("").trim();
    if l.chars().count() > 72 {
        format!("{}…", l.chars().take(71).collect::<String>())
    } else {
        l.to_string()
    }
}

/// `20260923-1712-3fa9`: sorts by time, and unique enough for one machine.
fn new_run_id() -> String {
    let now = unix_now();
    let date = utc_date(now).replace('-', "");
    let secs = now % 86_400;
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    format!(
        "{date}-{:02}{:02}-{:04x}",
        secs / 3600,
        (secs % 3600) / 60,
        (nanos ^ std::process::id()) & 0xffff
    )
}

fn newest_run(git: &Git) -> Option<String> {
    let out = git
        .run(&[
            "branch",
            "--list",
            "smithy/run-*",
            "--format=%(refname:short)",
        ])
        .ok()?;
    out.lines()
        .filter_map(|b| b.trim().strip_prefix("smithy/run-").map(str::to_string))
        .max()
}

/// Progress to stderr, stamped.
pub fn stderr_progress() -> Arc<dyn Fn(&str) + Send + Sync> {
    Arc::new(|line: &str| {
        let now = unix_now() % 86_400;
        eprintln!(
            "[run {:02}:{:02}:{:02}] {line}",
            now / 3600,
            (now % 3600) / 60,
            now % 60
        );
    })
}

/// Toast the user's desktop. Best effort: a Run never fails over this.
pub struct DesktopNotifier;

impl Notifier for DesktopNotifier {
    fn notify(&self, title: &str, body: &str) {
        eprintln!("\n== {title} ==\n{body}\n");
        #[cfg(windows)]
        {
            let esc = |s: &str| {
                s.replace('&', "&amp;")
                    .replace('<', "&lt;")
                    .replace('>', "&gt;")
                    .replace('\'', "''")
            };
            let lines: Vec<String> = body
                .lines()
                .map(|l| format!("<text>{}</text>", esc(l)))
                .collect();
            let script = format!(
                "[Windows.UI.Notifications.ToastNotificationManager, Windows.UI.Notifications, ContentType = WindowsRuntime] > $null; \
                 [Windows.Data.Xml.Dom.XmlDocument, Windows.Data.Xml.Dom.XmlDocument, ContentType = WindowsRuntime] > $null; \
                 $x = New-Object Windows.Data.Xml.Dom.XmlDocument; \
                 $x.LoadXml('<toast><visual><binding template=\"ToastGeneric\"><text>{}</text>{}</binding></visual></toast>'); \
                 $app = '{{1AC14E77-02E7-4E5D-B744-2EB1AE5198B7}}\\WindowsPowerShell\\v1.0\\powershell.exe'; \
                 [Windows.UI.Notifications.ToastNotificationManager]::CreateToastNotifier($app).Show([Windows.UI.Notifications.ToastNotification]::new($x))",
                esc(title),
                lines.join("")
            );
            let _ = std::process::Command::new("powershell.exe")
                .args(["-NoProfile", "-NonInteractive", "-Command", &script])
                .output();
        }
    }
}

/// Summaries of every Run in a Project, newest first: (id, verdict line).
pub fn list_runs(root: &Path) -> Vec<(String, String)> {
    let Ok(git) = Git::open(root) else {
        return Vec::new();
    };
    let Ok(out) = git.run(&[
        "branch",
        "--list",
        "smithy/run-*",
        "--format=%(refname:short)",
    ]) else {
        return Vec::new();
    };
    let mut runs: Vec<(String, String)> = out
        .lines()
        .filter_map(|b| {
            let id = b.trim().strip_prefix("smithy/run-")?.to_string();
            let state = git.show(b.trim(), &format!("{RUNS_DIR}{id}/state.json"))?;
            let state: RunState = serde_json::from_str(&state).ok()?;
            let line = state
                .verdict
                .map(|v| v.headline())
                .unwrap_or_else(|| "unfinished".into());
            Some((id, format!("{line} — {}", first_line(&state.intent))))
        })
        .collect();
    runs.sort_by(|a, b| b.0.cmp(&a.0));
    runs
}
