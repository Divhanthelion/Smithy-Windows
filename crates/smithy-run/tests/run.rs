//! Whole Runs, end to end: a real git repository, real Checks through the
//! shell, a scripted model and a scripted Jev.
//!
//! The Project's "toolchain" is a shell script that prints cargo-shaped test
//! output: two tests pass once `src.txt` contains `fn parse`, one fails
//! before. That keeps a Run to a second while exercising every seam a real
//! one uses.

use std::collections::{BTreeMap, VecDeque};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use smithy_agent::jev::{Choice, NextMove};
use smithy_agent::provider::test_support::{answer, tool_call, ScriptedProvider};
use smithy_agent::provider::Completion;
use smithy_agent::{Session, SessionConfig};
use smithy_run::runner::{Agents, Deps, Judge, Notifier, Purpose, Runner};
use smithy_run::state::{Ceilings, RunState, TaskStatus, Verdict};
use smithy_run::unattended::{DeniedLog, UnattendedShell, UnattendedWrites};
use smithy_tools::{Registry, ToolCtx, Workspace};

const CHECK_SH: &str = r#"if grep -q "fn parse" src.txt 2>/dev/null; then
  echo "test a ... ok"
  echo "test b ... ok"
  echo "test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out"
  exit 0
else
  echo "test a ... FAILED"
  echo "test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out"
  exit 101
fi
"#;

const PLAN: &str = "Here is the plan.\n\n```toml\nintent = \"x\"\ntoolchain = \"fake\"\n\n[[task]]\nid = \"T1\"\ntitle = \"Write the parser\"\nwhy = \"the whole intent\"\n\n[[task.check]]\nkind = \"test\"\nrun = \"sh check.sh\"\nmin_tests = 2\n```\n";

fn git(root: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn project() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    git(root, &["init", "-q", "-b", "main"]);
    git(root, &["config", "user.email", "run@test"]);
    git(root, &["config", "user.name", "run"]);
    git(root, &["config", "core.autocrlf", "false"]);
    std::fs::write(root.join("check.sh"), CHECK_SH).unwrap();
    std::fs::create_dir_all(root.join(".smithy")).unwrap();
    std::fs::write(
        root.join(".smithy/checks.toml"),
        "toolchain = \"fake\"\ntest = \"sh check.sh\"\ncounter = \"cargo\"\ntest_paths = [\"tests/**\"]\n",
    )
    .unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-q", "-m", "init"]);
    tmp
}

struct ScriptedAgents {
    provider: Arc<ScriptedProvider>,
    root: PathBuf,
    purposes: Mutex<Vec<Purpose>>,
}

#[async_trait]
impl Agents for ScriptedAgents {
    async fn session(&self, purpose: &Purpose, denied: DeniedLog) -> Result<Session, String> {
        self.purposes.lock().unwrap().push(purpose.clone());
        let mut registry = Registry::core();
        registry.add_hook(Box::new(UnattendedShell {
            jev: None,
            denied: denied.clone(),
            task: None,
        }));
        registry.add_hook(Box::new(UnattendedWrites {
            only_under: match purpose {
                Purpose::Build { .. } => None,
                _ => Some(".smithy/research/".into()),
            },
            denied,
            task: None,
        }));
        let ctx = ToolCtx::new(Workspace::open(&self.root)?);
        Ok(Session::new(
            self.provider.clone(),
            Arc::new(registry),
            Arc::new(ctx),
            SessionConfig::new("test"),
        ))
    }
}

/// Answers from queues; a question with an empty queue gets its default.
#[derive(Default)]
struct ScriptedJudge {
    guardrail: Mutex<VecDeque<Result<f64, String>>>,
    moves: Mutex<VecDeque<&'static str>>,
    asked: Mutex<Vec<&'static str>>,
}

#[async_trait]
impl Judge for ScriptedJudge {
    async fn guardrail(&self, _: &str) -> Result<f64, String> {
        self.asked.lock().unwrap().push("guardrail");
        self.guardrail
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or(Ok(0.01))
    }
    async fn needs_research(&self, _: &str) -> Result<f64, String> {
        self.asked.lock().unwrap().push("research");
        Ok(0.1)
    }
    async fn next_move(&self, _: &str, allowed: &[NextMove]) -> Result<Choice, String> {
        self.asked.lock().unwrap().push("next");
        let pick = self.moves.lock().unwrap().pop_front().unwrap_or("continue");
        assert!(
            allowed.iter().any(|m| m.name() == pick),
            "{pick} not allowed: {allowed:?}"
        );
        Ok(Choice {
            pick: pick.into(),
            confidence: 0.9,
            probabilities: BTreeMap::from([(pick.to_string(), 0.9)]),
        })
    }
    async fn weakened_tests(&self, _: &str) -> Result<f64, String> {
        self.asked.lock().unwrap().push("cheat");
        Ok(0.0)
    }
    async fn answered(&self, _: &str) -> Result<f64, String> {
        self.asked.lock().unwrap().push("answered");
        Ok(0.9)
    }
}

#[derive(Default)]
struct Toasts(Mutex<Vec<String>>);

impl Notifier for Toasts {
    fn notify(&self, title: &str, body: &str) {
        self.0.lock().unwrap().push(format!("{title}: {body}"));
    }
}

struct Harness {
    agents: Arc<ScriptedAgents>,
    judge: Arc<ScriptedJudge>,
    toasts: Arc<Toasts>,
    deps: Deps,
}

fn harness(root: &Path, script: Vec<Completion>) -> Harness {
    let agents = Arc::new(ScriptedAgents {
        provider: Arc::new(ScriptedProvider::new(script)),
        root: root.to_path_buf(),
        purposes: Mutex::new(Vec::new()),
    });
    let judge = Arc::new(ScriptedJudge::default());
    let toasts = Arc::new(Toasts::default());
    let deps = Deps {
        agents: agents.clone(),
        judge: judge.clone(),
        notifier: toasts.clone(),
        sources: None,
        progress: Arc::new(|l: &str| eprintln!("[run] {l}")),
        guardrail_patience: Duration::ZERO,
        supervisor: None,
    };
    Harness {
        agents,
        judge,
        toasts,
        deps,
    }
}

fn write(id: &str, path: &str, content: &str) -> Completion {
    let args = serde_json::json!({ "path": path, "content": content }).to_string();
    tool_call(id, "write", &args)
}

fn log_subjects(root: &Path, branch: &str) -> Vec<String> {
    git(root, &["log", "--format=%s", branch])
        .lines()
        .map(str::to_string)
        .collect()
}

fn read_state(root: &Path, s: &RunState) -> RunState {
    RunState::load(&root.join(".smithy/runs").join(&s.id).join("state.json")).unwrap()
}

/// The whole path: guardrail, baseline, plan, a failing round, a fix, the
/// checkpoint, the report, and a toast.
#[tokio::test]
async fn a_run_plans_fails_fixes_and_commits() {
    let tmp = project();
    let root = tmp.path();
    let h = harness(
        root,
        vec![
            answer(PLAN),
            write("c1", "src.txt", "nothing yet"),
            answer("Wrote the parser."),
            write("c2", "src.txt", "fn parse() {}"),
            answer("Fixed: parse now exists."),
        ],
    );

    let state = Runner::start(root, "build a parser", Ceilings::default(), h.deps.clone())
        .await
        .unwrap();

    assert_eq!(state.verdict, Some(Verdict::Done), "{:?}", state.verdict);
    let t1 = &state.tasks["T1"];
    assert_eq!(t1.status, TaskStatus::Done);
    assert_eq!(t1.attempts, 1);
    assert!(t1.commit.is_some());
    assert_eq!(state.usage.requests, 5, "every completion is counted once");

    let log = log_subjects(root, &state.branch);
    assert!(
        log[0].starts_with(&format!("Run {}: done", state.id)),
        "{log:?}"
    );
    assert_eq!(log[1], "T1: Write the parser");
    assert!(log[2].starts_with("Plan: build a parser"), "{log:?}");
    assert_eq!(log[3], "init");
    assert_eq!(
        git(root, &["show", &format!("{}:src.txt", state.branch)]),
        "fn parse() {}"
    );
    assert!(
        git(root, &["status", "--porcelain"]).trim().is_empty(),
        "the tree ends clean"
    );

    let dir = root.join(".smithy/runs").join(&state.id);
    let report = std::fs::read_to_string(dir.join("REPORT.md")).unwrap();
    assert!(
        report.contains("**done — every Task's Checks pass**"),
        "{report}"
    );
    assert!(
        report.contains("| ✅ | **T1** Write the parser | 1 |"),
        "{report}"
    );
    let decisions = std::fs::read_to_string(dir.join("decisions.jsonl")).unwrap();
    for kind in ["\"guardrail\"", "\"research\"", "\"next\""] {
        assert!(decisions.contains(kind), "{kind} missing from the log");
    }
    assert!(
        decisions.contains("the task's checks passed later"),
        "the continue was settled as right"
    );
    assert_eq!(
        *h.judge.asked.lock().unwrap(),
        vec!["guardrail", "guardrail", "research", "next"]
    );
    assert_eq!(h.toasts.0.lock().unwrap().len(), 1);
    assert_eq!(
        *h.agents.purposes.lock().unwrap(),
        vec![Purpose::Plan, Purpose::Build { task: "T1".into() }]
    );
}

#[tokio::test]
async fn a_flagged_intent_builds_nothing_and_wakes_the_user() {
    let tmp = project();
    let root = tmp.path();
    let h = harness(root, vec![answer(PLAN)]);
    h.judge.guardrail.lock().unwrap().push_back(Ok(0.9));

    let state = Runner::start(
        root,
        "something the guardrail stops",
        Ceilings::default(),
        h.deps.clone(),
    )
    .await
    .unwrap();

    assert!(
        matches!(state.verdict, Some(Verdict::Flagged(_))),
        "{:?}",
        state.verdict
    );
    assert!(
        h.agents.purposes.lock().unwrap().is_empty(),
        "no Session was ever started"
    );
    assert_eq!(state.flags[0].subject, "intent");
    assert_eq!(h.toasts.0.lock().unwrap().len(), 1);
    let report =
        std::fs::read_to_string(root.join(".smithy/runs").join(&state.id).join("REPORT.md"))
            .unwrap();
    assert!(report.find("## Needs you").unwrap() < report.find("## Tasks").unwrap());
}

/// No answer from Jev is not permission.
#[tokio::test]
async fn an_unanswered_guardrail_fails_closed() {
    let tmp = project();
    let root = tmp.path();
    let h = harness(root, vec![answer(PLAN)]);
    h.judge
        .guardrail
        .lock()
        .unwrap()
        .push_back(Err("429".into()));

    let state = Runner::start(root, "a parser", Ceilings::default(), h.deps.clone())
        .await
        .unwrap();

    assert!(
        matches!(&state.verdict, Some(Verdict::Failed(w)) if w.contains("does not build unchecked")),
        "{:?}",
        state.verdict
    );
    assert!(h.agents.purposes.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_dirty_tree_is_refused_before_anything_happens() {
    let tmp = project();
    let root = tmp.path();
    std::fs::write(root.join("check.sh"), "echo changed").unwrap();
    let h = harness(root, vec![]);
    let err = Runner::start(root, "x", Ceilings::default(), h.deps.clone())
        .await
        .unwrap_err();
    assert!(err.contains("uncommitted changes"), "{err}");
    assert_eq!(git(root, &["branch", "--show-current"]).trim(), "main");
}

/// The endpoint dies mid-attempt; the half-done work is stashed, not
/// committed. Then the process is "killed" mid-attempt on resume (state
/// left in flight with a dirty tree). A second resume stashes that, too,
/// and finishes the Task.
#[tokio::test]
async fn interrupted_and_crashed_attempts_are_set_aside_and_resumed() {
    let tmp = project();
    let root = tmp.path();
    let h = harness(root, vec![answer(PLAN), write("c1", "src.txt", "half")]);
    let first = Runner::start(root, "build a parser", Ceilings::default(), h.deps.clone())
        .await
        .unwrap();
    assert!(
        matches!(&first.verdict, Some(Verdict::Failed(w)) if w.contains("stopped answering")),
        "{:?}",
        first.verdict
    );
    assert!(
        first
            .stashes
            .iter()
            .any(|s| s.ends_with("T1a1-interrupted")),
        "{:?}",
        first.stashes
    );
    assert!(
        !git(root, &["ls-tree", "--name-only", &first.branch]).contains("src.txt"),
        "the half-done file was stashed, not committed"
    );

    // Simulate a kill: an Attempt in flight, and its files on disk.
    let path = root.join(".smithy/runs").join(&first.id).join("state.json");
    let mut s = read_state(root, &first);
    s.in_flight = Some(smithy_run::state::InFlight {
        task: "T1".into(),
        attempt: 2,
        since: 0,
    });
    s.tasks.get_mut("T1").unwrap().attempts = 2;
    s.save(&path).unwrap();
    std::fs::write(root.join("src.txt"), "crashed mid-edit").unwrap();

    let h2 = harness(
        root,
        vec![write("c2", "src.txt", "fn parse() {}"), answer("Done.")],
    );
    let done = Runner::resume(root, None, &[], h2.deps.clone())
        .await
        .unwrap();

    assert_eq!(done.verdict, Some(Verdict::Done), "{:?}", done.verdict);
    assert!(
        done.stashes.iter().any(|s| s.ends_with("crash-T1a2")),
        "{:?}",
        done.stashes
    );
    assert_eq!(done.tasks["T1"].attempts, 3, "the crashed attempt counted");
    let stashes = git(root, &["stash", "list"]);
    assert!(
        stashes.contains("interrupted") && stashes.contains("crash-T1a2"),
        "{stashes}"
    );
}

/// A Task that cannot pass is given up on, its work stashed, and the Run
/// says why.
#[tokio::test]
async fn a_task_that_will_not_pass_is_blocked_with_its_reason() {
    let tmp = project();
    let root = tmp.path();
    let h = harness(
        root,
        vec![answer(PLAN), write("c1", "src.txt", "nope"), answer("done")],
    );
    h.judge.moves.lock().unwrap().push_back("block");

    let state = Runner::start(root, "build a parser", Ceilings::default(), h.deps.clone())
        .await
        .unwrap();

    assert!(
        matches!(&state.verdict, Some(Verdict::Blocked(w)) if w.contains("T1")),
        "{:?}",
        state.verdict
    );
    let t1 = &state.tasks["T1"];
    assert_eq!(t1.status, TaskStatus::Blocked);
    assert!(
        t1.blocker.as_deref().unwrap().contains("given up"),
        "{:?}",
        t1.blocker
    );
    assert!(state.stashes.iter().any(|s| s.ends_with("T1-blocked")));
    let report =
        std::fs::read_to_string(root.join(".smithy/runs").join(&state.id).join("REPORT.md"))
            .unwrap();
    assert!(
        report.contains("## Blocked") && report.contains("test a ... FAILED"),
        "{report}"
    );
}

/// The model may not commit, and says so in the Report.
#[tokio::test]
async fn the_model_cannot_use_git_and_the_report_lists_what_it_tried() {
    let tmp = project();
    let root = tmp.path();
    let h = harness(
        root,
        vec![
            answer(PLAN),
            tool_call("g", "bash", r#"{"command":"git commit -am sneaky"}"#),
            write("c1", "src.txt", "fn parse() {}"),
            answer("Done."),
        ],
    );
    let state = Runner::start(root, "build a parser", Ceilings::default(), h.deps.clone())
        .await
        .unwrap();
    assert_eq!(state.verdict, Some(Verdict::Done));
    assert_eq!(state.denied.len(), 1, "{:?}", state.denied);
    assert!(state.denied[0].command.contains("git commit"));
    assert!(!log_subjects(root, &state.branch)
        .iter()
        .any(|s| s == "sneaky"));
}

/// From the first real Run: a toolchain the shell cannot find fails in a
/// second, before planning, instead of an hour later as `exit 127`.
#[tokio::test]
async fn a_missing_toolchain_fails_before_any_session() {
    let tmp = project();
    let root = tmp.path();
    std::fs::write(
        root.join(".smithy/checks.toml"),
        "toolchain = \"fake\"\ntest = \"no-such-tool-xyz check\"\ncounter = \"cargo\"\n",
    )
    .unwrap();
    git(root, &["commit", "-qam", "missing tool"]);
    let h = harness(root, vec![answer(PLAN)]);

    let state = Runner::start(root, "x", Ceilings::default(), h.deps.clone())
        .await
        .unwrap();

    assert!(
        matches!(&state.verdict, Some(Verdict::Failed(w)) if w.contains("`no-such-tool-xyz`")),
        "{:?}",
        state.verdict
    );
    assert!(h.agents.purposes.lock().unwrap().is_empty());
}
