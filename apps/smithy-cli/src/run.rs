//! `smithy-agent run`: an unattended Run from the terminal.
//!
//! The runner is `smithy-run`; this file supplies what it borrows from a
//! host — Sessions built like the REPL's but with unattended hooks, Jev, a
//! toast — and the command line.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use smithy_agent::jev::Jev;
use smithy_agent::{system_prompt, Session, SessionConfig};
use smithy_project::Project;
use smithy_run::runlog::LogLevel;
use smithy_run::runner::{self, Agents, Deps, DesktopNotifier, Judge, NoJudge, Purpose, Runner};
use smithy_run::state::{Ceilings, RunState};
use smithy_run::unattended::{DeniedLog, UnattendedShell, UnattendedWrites};
use smithy_tools::research::{SourceStore, NOTES_DIR};
use smithy_tools::{ToolCtx, Workspace};

use crate::boot::{assemble_registry, prepare, Prepared};

/// How much of a Run to keep for review, and where.
#[derive(Debug, Clone, PartialEq)]
pub struct Logging {
    pub level: LogLevel,
    pub dir: Option<PathBuf>,
}

impl Logging {
    /// `SMITHY_RUN_LOG` (off, events, full; default full) and
    /// `SMITHY_RUN_LOG_DIR`. Flags override both.
    fn from_env() -> Result<Logging, String> {
        let level = match std::env::var("SMITHY_RUN_LOG") {
            Ok(v) if !v.trim().is_empty() => LogLevel::parse(&v)?,
            _ => LogLevel::default(),
        };
        let dir = std::env::var_os("SMITHY_RUN_LOG_DIR").map(PathBuf::from);
        Ok(Logging { level, dir })
    }
}

pub enum RunCommand {
    Start {
        intent: String,
        ceilings: Ceilings,
        logging: Logging,
    },
    Resume {
        id: Option<String>,
        allow: Vec<String>,
        logging: Logging,
    },
    List,
}

pub fn usage() -> &'static str {
    "\
smithy-agent run \"INTENT\" [--hours N] [--attempts N] [--research-minutes N]
                          [--project PATH]
                                               research may spend a third of --hours
                                               unless --research-minutes says otherwise
smithy-agent run --intent-file FILE …          the intent from a file
smithy-agent run --resume [ID] [--allow T3 …]  carry on the newest (or named) Run;
                                               --allow clears a guardrail flag or
                                               gives a blocked Task fresh attempts
smithy-agent runs [--project PATH]             every Run in this Project

--log off|events|full   what to keep for review (default full; SMITHY_RUN_LOG):
                         events = every request, tool call and check, timed;
                         full = that plus every conversation in full
--log-dir DIR            where (default ~/.local/share/smithy/runs/PROJECT/RUN;
                         SMITHY_RUN_LOG_DIR)

A Run works on its own branch (smithy/run-ID), commits each Task when its
checks pass, never pushes, and leaves .smithy/runs/ID/REPORT.md.
"
}

/// `(command, project)` from the words after `run` (or `runs`).
pub fn parse(list: bool, words: &[String]) -> Result<(RunCommand, PathBuf), String> {
    let mut project = PathBuf::from(".");
    let mut logging = Logging::from_env()?;
    let mut intent: Option<String> = None;
    let mut resume = false;
    let mut id = None;
    let mut allow = Vec::new();
    let mut ceilings = Ceilings::default();
    let mut it = words.iter();
    while let Some(w) = it.next() {
        let mut value = |name: &str| {
            it.next()
                .cloned()
                .ok_or_else(|| format!("{name} needs a value\n\n{}", usage()))
        };
        match w.as_str() {
            "--project" | "-C" => project = PathBuf::from(value("--project")?),
            "--hours" => {
                ceilings.hours = value("--hours")?
                    .parse()
                    .map_err(|_| "--hours takes a whole number".to_string())?
            }
            "--research-minutes" => {
                ceilings.research_minutes = Some(
                    value("--research-minutes")?
                        .parse()
                        .map_err(|_| "--research-minutes takes a whole number".to_string())?,
                )
            }
            "--attempts" => {
                ceilings.attempts_per_task = value("--attempts")?
                    .parse()
                    .map_err(|_| "--attempts takes a whole number".to_string())?
            }
            "--intent-file" => {
                let path = value("--intent-file")?;
                intent = Some(
                    std::fs::read_to_string(&path)
                        .map_err(|e| format!("could not read {path}: {e}"))?,
                );
            }
            "--log" => logging.level = LogLevel::parse(&value("--log")?)?,
            "--log-dir" => logging.dir = Some(PathBuf::from(value("--log-dir")?)),
            "--resume" => resume = true,
            "--allow" => allow.push(value("--allow")?),
            flag if flag.starts_with("--") => {
                return Err(format!("unknown flag {flag}\n\n{}", usage()))
            }
            text if resume && id.is_none() => id = Some(text.to_string()),
            text if intent.is_none() && !resume => intent = Some(text.to_string()),
            text => return Err(format!("unexpected `{text}`\n\n{}", usage())),
        }
    }
    let cmd = if list {
        RunCommand::List
    } else if resume {
        RunCommand::Resume { id, allow, logging }
    } else {
        match intent.filter(|i| !i.trim().is_empty()) {
            Some(intent) => RunCommand::Start {
                intent,
                ceilings,
                logging,
            },
            None => return Err(format!("what should the Run build?\n\n{}", usage())),
        }
    };
    Ok((cmd, project))
}

pub async fn run(cmd: RunCommand, project: &Path) -> Result<(), String> {
    let project = Project::discover(project)
        .or_else(|_| Project::open(project))
        .map_err(|e| e.to_string())?;
    let root = project.root.clone();

    if let RunCommand::List = cmd {
        let runs = runner::list_runs(&root);
        if runs.is_empty() {
            println!("no Runs in {}", root.display());
        }
        for (id, line) in runs {
            println!("{id}  {line}");
        }
        return Ok(());
    }

    let logging = match &cmd {
        RunCommand::Start { logging, .. } | RunCommand::Resume { logging, .. } => logging.clone(),
        RunCommand::List => unreachable!(),
    };
    let prepared = prepare(&project).await?;
    eprintln!(
        "[run] model {} · project {} · web_search {}",
        prepared.model_label,
        root.display(),
        if prepared.brave_configured {
            "on"
        } else {
            "off (no Brave key: research can fetch known URLs but not search)"
        }
    );
    let jev = Jev::from_store();
    if jev.is_none() {
        eprintln!(
            "[run] no Jev key (AI_GATEWAY_API_KEY): the guardrail cannot be asked, so nothing \
             will be built"
        );
    }
    let quick_jev = jev.map(Arc::new);
    let judge: Arc<dyn Judge> = match Jev::from_store() {
        Some(j) => Arc::new(j.patient()),
        None => Arc::new(NoJudge),
    };
    let deps = Deps {
        agents: Arc::new(CliAgents {
            prepared,
            project: project.clone(),
            jev: quick_jev.clone(),
        }),
        judge,
        notifier: Arc::new(DesktopNotifier),
        sources: SourceStore::default_location(),
        progress: runner::stderr_progress(),
        guardrail_patience: Duration::from_secs(300),
        log_level: logging.level,
        log_dir: logging.dir.clone(),
        supervisor: quick_jev,
    };

    let state = match cmd {
        RunCommand::Start {
            intent, ceilings, ..
        } => Runner::start(&root, &intent, ceilings, deps).await?,
        RunCommand::Resume { id, allow, .. } => {
            Runner::resume(&root, id.as_deref(), &allow, deps).await?
        }
        RunCommand::List => unreachable!(),
    };
    summarize(&root, &state);
    Ok(())
}

fn summarize(root: &Path, state: &RunState) {
    let report = root.join(".smithy/runs").join(&state.id).join("REPORT.md");
    println!(
        "\n{}\nbranch {}\nreport {}",
        state
            .verdict
            .as_ref()
            .map(|v| v.headline())
            .unwrap_or_default(),
        state.branch,
        report.display()
    );
}

/// Sessions built like the REPL's, with the unattended hooks instead of the
/// interactive ones and no MCP (its tools' review hook would have nobody to
/// ask). The Map and the Index are rebuilt for each Session, because the
/// Project changes between Tasks.
struct CliAgents {
    prepared: Prepared,
    project: Project,
    jev: Option<Arc<Jev>>,
}

#[async_trait]
impl Agents for CliAgents {
    async fn session(&self, purpose: &Purpose, denied: DeniedLog) -> Result<Session, String> {
        let p = &self.prepared;
        let root = self.project.root.clone();
        let project = self.project.clone();
        let budget = p.budget;
        let (context, index) = tokio::task::spawn_blocking(move || {
            let context = project.context_with_graph(budget, None);
            let index = Arc::new(smithy_project::symbols::SymbolIndex::build(&project.root));
            (context, index)
        })
        .await
        .map_err(|e| format!("project scan failed: {e}"))?;

        let research = matches!(purpose, Purpose::Research { .. });
        let mut registry = assemble_registry(
            research,
            p.provider.clone(),
            &root,
            p.brave_configured,
            index,
        );
        let task = match purpose {
            Purpose::Build { task } => Some(task.clone()),
            Purpose::Research { task, .. } => task.clone(),
            Purpose::Plan => None,
        };
        registry.add_hook(Box::new(UnattendedShell {
            jev: self.jev.clone(),
            denied: denied.clone(),
            task: task.clone(),
        }));
        registry.add_hook(Box::new(UnattendedWrites {
            only_under: match purpose {
                Purpose::Build { .. } => None,
                _ => Some(format!("{NOTES_DIR}/")),
            },
            denied,
            task,
        }));

        let workspace = Workspace::open(&root)?;
        let prompt = system_prompt(workspace.root(), &registry.names(), Some(&context.rendered));
        let project_chars = context.rendered.len();
        let mut config = SessionConfig::new(prompt.clone())
            .with_segments(prompt.len().saturating_sub(project_chars), project_chars);
        config.limits = p.limits.clone();
        // A research turn is as long as its question deserves.
        config.limits.max_seconds = match purpose {
            Purpose::Research { depth, .. } => depth.minutes() * 60,
            _ => p.turn_seconds,
        };
        Ok(Session::new(
            p.provider.clone(),
            Arc::new(registry),
            Arc::new(ToolCtx::new(workspace)),
            config,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(s: &[&str]) -> Vec<String> {
        s.iter().map(|w| w.to_string()).collect()
    }

    #[test]
    fn start_with_ceilings_and_project() {
        let (cmd, project) = parse(
            false,
            &words(&["build a parser", "--hours", "2", "--project", "p"]),
        )
        .unwrap();
        assert_eq!(project, PathBuf::from("p"));
        match cmd {
            RunCommand::Start {
                intent, ceilings, ..
            } => {
                assert_eq!(intent, "build a parser");
                assert_eq!(ceilings.hours, 2);
                assert_eq!(ceilings.attempts_per_task, 3);
            }
            _ => panic!("not a start"),
        }
    }

    #[test]
    fn resume_with_an_id_and_allows() {
        let (cmd, _) = parse(
            false,
            &words(&[
                "--resume",
                "20260923-1712-3fa9",
                "--allow",
                "T3",
                "--allow",
                "intent",
            ]),
        )
        .unwrap();
        match cmd {
            RunCommand::Resume { id, allow, .. } => {
                assert_eq!(id.as_deref(), Some("20260923-1712-3fa9"));
                assert_eq!(allow, vec!["T3", "intent"]);
            }
            _ => panic!("not a resume"),
        }
    }

    #[test]
    fn a_run_needs_an_intent() {
        assert!(parse(false, &[]).is_err());
        assert!(
            parse(false, &words(&["a", "b"])).is_err(),
            "two intents is a quoting mistake"
        );
        assert!(matches!(parse(true, &[]).unwrap().0, RunCommand::List));
    }

    #[test]
    fn log_flags_and_research_minutes() {
        let (cmd, _) = parse(
            false,
            &words(&[
                "x",
                "--log",
                "events",
                "--log-dir",
                "D:/logs",
                "--research-minutes",
                "90",
            ]),
        )
        .unwrap();
        match cmd {
            RunCommand::Start {
                ceilings, logging, ..
            } => {
                assert_eq!(logging.level, LogLevel::Events);
                assert_eq!(logging.dir, Some(PathBuf::from("D:/logs")));
                assert_eq!(ceilings.research_minutes, Some(90));
            }
            _ => panic!("not a start"),
        }
        assert!(parse(false, &words(&["x", "--log", "loud"])).is_err());
    }
}
