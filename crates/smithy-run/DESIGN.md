# Unattended runs

One high-level intent in, a branch of checked commits out. Smithy works until
every task's checks pass or it is genuinely blocked, then leaves a report.

```
smithy-agent run "a library that parses ISO 8601 durations …"
smithy-agent run --resume            # after a crash, a reboot, or a wake-up
smithy-agent run --resume --allow T3 # clear a guardrail flag you have read
```

Decided in the interview (2026-09-23): headless CLI; Brave search available;
build straight after planning, behind a legality/ethics guardrail that stops the
run and wakes you; a Windows toast plus a report file; old tests may change but
are checked for weakening; 8 hours, 3 attempts per task, stop after 2 tasks in a
row are blocked; CMake+ctest and pytest as the first non-Rust adapters; notes in
the repo, fetched pages outside it; first trial is a toy Rust crate.

## The order of authority

1. **Checks** — the compiler and the tests, run by the runner, not the model.
   A task is done when its checks pass and at no other time.
2. **Mechanical rules** — test counts, skip markers, citation matching, git
   ownership, ceilings. Cheap, deterministic, and never overruled by a model.
3. **Jev** — judgment where no rule reaches: is it stuck, is it cheating, did
   research answer the question, is this intent something we should build.
   Every Jev call is a logged, calibrated threshold.
4. **The model** — does the work; its claims are inputs, never verdicts.

Jev may add friction (a nudge, a stop, a wake) but never removes a check. An
absent Jev (no key, gateway down) means the run proceeds on rules and checks
alone, except the guardrail, which fails closed: no guardrail answer, no build.

## 1. Intent → plan

A planning Session (fresh, full tools, research allowed) turns the intent into
`.smithy/runs/<run>/plan.toml`:

```toml
intent = "…verbatim…"
toolchain = "rust"            # detected; see §6

[[task]]
id = "T1"
title = "Parse the date part (PnYnMnD)"
why = "…which slice of the intent this serves…"
depends = []
research = ["Which designators does ISO 8601-1:2019 allow in a duration, and may they carry fractions?"]
[[task.check]]                 # every build task has at least one
kind = "test"                  # build | test | lint | command
run = "cargo test --lib duration::date"
min_tests = 3                  # a filter that matches nothing is a failure, not a pass
```

Validation is mechanical before anything runs: ids unique, `depends` acyclic,
every task has a check, every `test` check has `min_tests ≥ 1`, every command
is one the toolchain knows or `.smithy/checks.toml` declares. An invalid plan
goes back to the planner with the errors, twice, then the run wakes you.

The plan is committed as the branch's first commit. Nothing waits for approval;
the branch *is* the approval surface — read it, keep it, or delete it.

## 2. The run loop

- **Start:** the tree must be clean (else wake: "uncommitted work"). Record the
  base commit, create `smithy/run-<id>`, run the **baseline** (build + full
  test suite) and store which tests pass. Pre-existing failures are allowed
  and may not grow.
- **Per task**, in dependency order: guardrail → research-or-build → up to
  **3 attempts**. An attempt is a fresh Session given the intent, the plan,
  this task, its checks, the notes it may read, and the previous attempt's
  handoff. Inside an attempt the model works a turn; the runner then runs the
  checks and cheat checks and feeds failures back as the next turn (≤ 6 turns).
- **Checkpoint:** checks pass and cheat checks pass → one commit
  `T3: <title>` with `.smithy/runs/<id>/{state.json,decisions.jsonl,REPORT.md}`
  updated in the same commit. Messages go through `.git/SMITHY_MSG` and
  `git commit -F`.
- **Git belongs to the runner.** The unattended shell hook denies the model
  `commit, checkout, switch, reset, rebase, merge, stash, branch -d/-D, tag,
  push` outright. `push` is never run by anyone.
- **Nobody is at the keyboard.** Every prompt a hook would have shown becomes a
  denial with a reason and a line in the report ("would have asked: …").
- **Ends:** all tasks done; 8 h wall clock; 2 consecutive blocked tasks; a
  guardrail flag; or an escalation. Each ends with a final commit of the
  report and a toast.
- **Resume:** `state.json` at the branch tip is the truth. Dirty tree on
  resume = a crashed attempt: stash it as `smithy-run-<id>-crash-T3a2`, count
  the attempt, carry on. Nothing is discarded.

## 3. Where Jev decides

Each is one `systemone` call; several questions share one request when they
share a state. All thresholds live beside their measured ranges and are
exercised by `cargo run -p smithy-agent --example jev <suite>`.

| Decision | When | Question type | Acts |
|---|---|---|---|
| **guardrail** | intent before planning; each task before it starts | noul: illegal, or clearly harmful to others? | ≥ T → stop, wake. Fails closed. |
| **research-or-build** | each task, if the planner listed no questions | noul: depends on outside facts the agent is unlikely to know exactly? | ≥ T → research first |
| **next move** | after each failed check round, and at 60% context | choice: continue / research / compact / handoff / escalate / stop | rules first (see below), Jev among what rules allow |
| **loop** | after each step (existing) | noul | nudge, then stop the turn |
| **done** | before an answer (existing) | noul | send back once |
| **cheat** | after checks pass, if an old test file changed | noul on the test diff: weakened to make it pass? | ≥ T → revert attempt, mark blocked |
| **answered** | after research, once citations verify | noul: does this note answer the question? | < T → one more pass on the gap; still < T → proceed, flagged partial |

Rules that outrank the next-move choice: context ≥ 80% → compact now; same
failing check signature 3 rounds running → not `continue`; attempts exhausted
→ blocked. Jev's pick below a confidence floor falls back to `continue` (or
`handoff` if `continue` was ruled out). **Wake me** = guardrail, escalate, run
end, or can't start.

Calibration: every suite has known cases on both sides. Every live decision is
logged with its full state, so real runs become new cases:
`--example jev replay .smithy/runs/<id>/decisions.jsonl` rescores a run and
shows where a changed threshold would have acted differently.

## 4. Research

**Triggers:** planner-listed questions; research-or-build; the `research`
option of next move (an unfamiliar error that repeats). Before researching,
the note index is searched; an existing note that Jev judges answers the
question is reused instead.

**Unattended research** is a sub-Session running the `/research` procedure
from its system prompt (no slash needed), with read/search/fetch tools and
`write` confined to `.smithy/research/`. Budget ~60 steps / 20 min.

**Sources are checked mechanically:**

- `web_fetch` saves every page it renders to
  `~/.local/share/smithy/sources/<sha256>.txt` with a log line (url, final
  url, time, status, type) and tells the model the short id: `source a1b2c3d4`.
- Each finding in a note carries a source id and a quote. **`cite_check`**
  (a tool the model can call, and the runner always calls) confirms each quote
  appears in that snapshot (normalised for whitespace, case and quote marks),
  that the URL was fetched, and that load-bearing claims have two sources on
  different domains. Repo citations (`path:line`) are checked against the file.
- Findings that fail are marked `unverified` in the note; a note with no
  verified finding has failed.

**Notes live at** `.smithy/research/YYYY-MM-DD-<slug>.md` with
`.smithy/research/index.json` (question, task, date, verified/total findings,
Jev's answered score, source ids). Not `docs/research/`: this repo ignores
`docs/`, and a run's notes must be committed on its branch. Task prompts list
the notes relevant to them; later steps read, rather than redo.

The interactive `/research` and `/pointed-research` get the same snapshots,
`cite_check`, and index — one method, whether a person or the runner starts it.

## 5. Ground truth

Checks run in this order and stop at the first failure: build → the task's
checks → the full test suite (no baseline-passing test may now fail) → lint
(if the toolchain has one and the plan asks). Output is clipped around the
first errors and handed back verbatim. Test counts are parsed per toolchain;
a check that ran zero tests failed.

Cheat checks, mechanical first:
- the number of passing tests may not drop below baseline;
- no new skip markers (`#[ignore]`, `@pytest.mark.skip`, `xfail`,
  `DISABLED_`), no deleted test files, no net loss of assertions in a test
  file that existed at run start;
- any other edit to a pre-existing test goes to Jev's cheat question.

## 6. Languages

Rust-specific today: `Project::discover` (walks to `Cargo.toml`), the Map
(`smithy-project::context` via `rust.rs`), the symbol index, the SCIP call
graph. The agent loop, tools, Jev, and this runner are not.

The seam is one type, **`Toolchain`**: how to detect it (a manifest), the
build / test / filtered-test / lint commands, how to count tests in output,
which paths are tests, and which markers skip them. Adapters: Rust (cargo),
C (CMake + ctest), Python (pytest, ruff if configured). `.smithy/checks.toml`
overrides any command. The Map and index stay Rust-only and are simply absent
elsewhere (the `symbol` tool already disappears when the index is empty); a
tree-sitter index for C and Python is later work that plugs in behind the
same `symbol` tool.

## 7. The morning report

`.smithy/runs/<id>/REPORT.md`, rewritten at every checkpoint so a crash still
leaves one:

- verdict line, intent, branch, base, duration, how to review
  (`git log base..branch`) and how to discard;
- **flags first**: guardrail stops and escalations, with the text judged;
- tasks: status, attempts, checks, commit, blocker with the last check output;
- research: each note, verified/total findings, answered score;
- every Jev decision: kind, probability, threshold, action, and — where the run
  later learned it — whether it was right;
- commands that would have asked; usage (requests, tokens, cache rate).

## Build order

Each step is tested and committed on its own:

1. Glossary and README corrections; this document.
2. `smithy-run` crate: `Toolchain`, adapters, test-count parsers.
3. Git ownership: branch, checkpoint, stash, clean-tree checks.
4. Plan: schema, validation, rendering, planner prompt and parser.
5. State, decision log, report.
6. Source snapshots, `cite_check`, note index; skills updated.
7. Jev: `choice` support, the new questions, calibration suites, thresholds.
8. Cheat checks.
9. The runner, end to end against a scripted provider, including resume.
10. `smithy-agent run`, unattended hooks, toast.
11. A real run on the Thor, in a scratch Project (`smithy-trial`).
