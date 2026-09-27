# Teaching a local model to work while we sleep

*A field report on Smithy's unattended Runs: four trial runs and one real
project on a Jetson AGX Thor, what the first one broke, what it took for the
rest to finish, where the time goes, and whether the result was right.
23–25 September 2026.*

---

The goal was simple to say: give Smithy one sentence — *build X* — walk away,
and come back to a branch of commits whose tests pass and a report that says
what happened. No babysitting, no "shall I proceed?". And do it on a machine
on the desk, with a model that runs locally, costs nothing per token, and
sends nothing anywhere.

The hard part is not getting a model to write code. It is knowing, the next
morning, whether the code is real. So everything starts from one rule: **the
compiler and the tests decide when work is done — not the model, and not the
judge model watching it.** A task is finished when the runner has run its
checks itself, the rest of the suite has not regressed, and no test that
existed before the run was weakened, skipped or deleted to get there. Only
then does it commit. The model can read git history but cannot commit, reset
or push; every commit on the branch is one the checks earned.

Around that rule sit the things an unattended agent needs: a planner that
turns the intent into tasks, each with the exact test command that proves it;
Jev — TypeSafe's fast decision model — watching for loops, for answers that
claim to be finished and aren't, for tests being quietly loosened, and for
intents that shouldn't be built at all; and research that can be checked,
where every page the model reads is saved and every quote in its notes is
matched against the page it came from.

## The first run

It was a toy on purpose: a Rust library that parses ISO 8601 durations like
`P3DT4H30M` into a `Duration`, with tests and a small command-line tool. It
took three and a half hours, finished one task of six, and was stopped by
hand because the Thor was running hot.

The post-mortem found ten causes, and most were ours. The run was launched
from a shell that couldn't see `cargo`. The planner split a small library into
six tasks and gave every one a research question, and every question got the
full, adversarial research method for half an hour. One turn ran for 57
minutes before anything was checked, while the model wrote the whole library
inside the first task and then polished it. And the server recomputed its
entire ~52,000-token prompt on every request, because prefix caching was off:
14.8 million prompt tokens, none from cache. That was the heat.

The parts that mattered held. The one finished task was committed only after
the runner saw its build and tests pass. Every call Jev made was right. And
the research was genuinely good — one note, thirty findings each quoted
verbatim from primary sources, caught a real bug in code the previous task had
committed.

## Fixing causes, not symptoms

A missing toolchain now fails in a second. Research depth is sized to the
question — a quick lookup, a decision, a deep investigation — within a budget
that grows with the run. Jev *reports* how much a question needed outside
sources instead of silently deciding whether to look. A task's checks now run
while the model works, and its turn ends the moment they pass. Every request,
tool call and check is logged with timings.

Then the machine. A community-tuned serving recipe with prefix caching made a
repeated 16,000-token prompt start answering in one second instead of nine.
The Thor had been downloading over 2.4 GHz Wi-Fi at 1.5 MB/s; one cable to the
router made it 80–100 MB/s. And after a reboot the server refused to start at
all — the Thor counts page cache as used memory, which throws off the server's
own arithmetic — until we gave it a fixed amount of memory for its cache
instead of a share of what looked free.

We also measured things we'd been guessing at. A second, smaller model loaded
beside the main one costs nothing while idle, and 12–62% of the main model's
speed while busy — they share the same memory bandwidth. An open-source
stand-in for Jev got half our test cases wrong; Jev got every one it answered
right.

## The second run

Same intent, clean repository, new server. **It finished.** Three tasks, three
commits, 49 tests passing, a working command-line tool, in 2 hours 23
minutes — 91% of the prompt served from cache, and each task stayed inside its
own lane. Three times the model's turn ended the instant its tests went
green, which is exactly the discipline the first run lacked.

It was not an hour, and the reason is the most useful thing we learned. About
thirty minutes of the second task went to three replies in which the model
worked out an entire file in its head — 16,000 tokens of thinking — and hit
Smithy's per-reply output limit a moment before writing it down. Its thinking
isn't shown back to it, so each retry started from nothing, and Smithy's
recovery message, written for an older model stuck saying "Done. Done.", told
it to wrap up in two sentences.

That limit was guarding against runaway output. The model never produced any.
Every real problem across both runs — the loops, the false "done", the missing
toolchain — was caught by something that looks at *what* the model is doing:
Jev, or the tests. The blunt limits on size and time were the ones doing
damage. So the reply cap is gone for the local model, a reply in progress
when the turn's clock runs out now gets to finish, and a cut-off reply is told
plainly what was lost and to take one concrete step at a time. The rule we
took from it: **a limit should be a backstop that never fires during good
work. If it fires on good work, it is set wrong.**

## The third run

One more repair first: the second run's report showed Smithy's shell guard
refusing nine perfectly ordinary commands, like `grep` on the project's own
source, because it misread Git Bash's way of writing Windows paths, the
`/dev/null` everyone sends errors to, and a stray backslash from a search
pattern.

Then the same intent, from the same clean start. **1 hour 49 minutes**, three
of three tasks, 154 requests instead of 214, not one reply cut off. The task
that lost half an hour last time wrote its code in 23 small steps instead of
eight big ones — which is what we'd asked it to do. Somewhere in there it
noticed an arithmetic slip in its own plan and quietly used the right number.

## Where the time actually went

The third run's log answered the question of where to look next, and the
answer was blunt: 107 of its 109 minutes were the model generating text. The
tests took six seconds in total. And three quarters of everything the model
generated was thinking.

So there were three levers — generate faster, generate less, or generate
several things at once — and we measured each before trusting it.

*Faster:* a variant of the model with some of its weights stored more
compactly decodes 19–27% faster and takes 2.7 GB less memory. That was an
easy call.

*Several at once:* we expected running three conversations side by side to
cost little more than one. It doesn't. Together they go about 1.4 times as
fast as one, because each slows down by about 30%. That still pays off
for research, which is limited by its clock rather than its words: three
questions researched side by side finish when the longest does, so that is
how a run does research now.

*Less:* every research note is now kept in a shared library, so the next run
that asks about ISO 8601 durations reads the answer instead of looking it up
again. And we ran the same three research questions twice at the same moment,
once with the model thinking before every step and once without. On quick
lookups, thinking was a handicap: it deliberated its way to six actions in
eight minutes, while the version that just acted took sixty-one and found
more. On the harder question, thinking found the one fact that settled it and
the other version didn't. So quick lookups now skip the thinking, and
everything else keeps it.

On paper, the changes above brought the same run to about an hour and a
quarter. So we ran it a fourth time to find out.

## The fourth run

**53 minutes.** Same intent, same clean start, three of three tasks passing
their tests — half the time of the third run, and faster than the estimate.
Research took nine minutes instead of thirty-seven: two questions side by
side, without thinking, and they came back with more verified findings than
any lookup before them. The task that took forty-two minutes last time took
eighteen. The model wrote little more than half as much text as before, and a
much smaller share of it was thinking. Both research notes are now in the
library, so a fifth run of this intent would research nothing at all.

It isn't perfect: the finished tool reads `-P1D` as a command-line option and
prints its usage instead of saying the duration is invalid — an edge case none
of its 26 tests thought to check. And we can't yet say how much of the gain was
the faster model and how much the better research; this run changed both.

## A real project

The duration library was a toy we had now built four times. It proved the
machinery, not that the machinery was useful. So we pointed Smithy at one of
our own projects — a Hebrew calendar in Rust, a library plus a desktop app —
and asked for two features someone who keeps that calendar would actually
want: the *molad*, the moment of the new moon announced in synagogue before
each month, and *Daf Yomi*, the page of Talmud studied worldwide each day in a
cycle that began in 1923. Both are easy to get almost right. A day off in 1975
or an hour off in one month looks fine, and no existing test knows the
answers.

We went in suspecting the project wasn't in great shape, so before judging the
run we checked where it started, against Hebcal — the calendar most Jewish
calendar software is checked against. The suspicion was half right. The core
was sound: every Hebrew date, holiday and weekly Torah portion across 21 years
matched. Around it things were rougher. The prayer times are calculated to
within a minute and then shown in the wrong time zone — UTC in the app, and an
hour off all summer even where a zone is set. The app's default build fails on
Windows, and its release build ships an older copy of its own interface. The
run was told to leave the app alone, and it did.

**It finished: three of three tasks, every check passing — and the result is
right.** Across 13,991 days of Daf Yomi between 1923 and 2040 and 351 molad
announcements between 2009 and 2040, it disagrees with Hebcal on none. It
built the molad on the calendar's existing new-moon arithmetic, as asked,
rather than writing a second copy, and nothing that worked before changed:
41,030 days from 1923 to 2035 give the same dates, holidays and portions
after the run as before it.

It was not an overnight run. It took sixteen hours on the clock and about six
and three-quarters of running. Planning took 78 minutes, against ten for the
toy, because the planner went and did its own research instead of asking for
it. The rest of the gap was ours and the hardware's. A laptop restart killed
the run, which had been started from the session that was driving it, so we
moved it onto the Thor itself. In its first half-hour there, three bugs in
Smithy stopped it, one after another. Three research conversations hitting the
server at once triggered a race. The shell guard refused a scratch directory,
so the model wrote a probe file into the source tree. Then it wasn't allowed
to delete the file. Each was fixed, and the run resumed. Then the Thor began
tripping its over-current alarm, hundreds of times an hour at full power. We
stopped for the afternoon, and the fix was a lower power mode: at 90 watts,
three hours of generation tripped it zero times. The last task
spent forty minutes trying to install a lint tool its checks needed and the
machine didn't have, and was refused each time. Smithy should have caught
that before the run started.

The work itself was harder, and you can see it. While building, 84–91% of
what the model wrote was thinking, against about half on the fourth run, and
two tasks each spent a whole attempt designing before a single test ran.

One flaw turned up in checking that no test could have caught. A comment in
the Daf Yomi code says its dates drift from the published ones by a few days
in the 1980s, so it deliberately leaves those years untested. Hebcal agrees
with the code, not the comment — and the comment gets its own arithmetic
wrong. The next person to read it would believe it.

## What it is, honestly

A local model on a Thor writes code at 30–45 tokens a second. Work a hosted
frontier model does in minutes takes this one the better part of an hour, and
watching it live is painful. Overnight, on a machine that is otherwise idle,
private and free, that trade looks different — which is the point of making
it run unattended.

The first run took three and a half hours to finish one task of six. The
fourth finished everything in under an hour, on the same machine, unattended.
The real project finished too, with code that is right on every date we could
check against an outside reference, though it needed a person six times to
get there, and never once for the code. The bar hasn't moved: done while we
sleep, and a report the next morning we can believe. The second half held up
because of the checks. What still stands between us and the first half is
everything around the model.

---
---

# Technical report

**Period:** 2026-09-23 17:36 UTC – 2026-09-25 23:25 UTC
**Repository:** `Smithy-Windows` on GitHub, 44 commits from `736e89a`
to the commit carrying this report, 562 tests passing (smithy-agent 271 + 28
integration, smithy-tools 158 + 7, smithy-run 72 + 16 end-to-end, smithy-cli
10).
**Hardware:** NVIDIA Jetson AGX Thor, 128 GB unified memory (122.8 GiB
visible), JetPack 7.1 / L4T R38, 1 TB NVMe; Windows 11 laptop as the
Smithy host.
**Models:** Qwen3.8-Flash-Next (NVFP4; FP8-hybrid side weights from 25
September) via vLLM on the Thor; Jev
(`typesafe-ai/jev`) via the Vercel AI Gateway.

## 1. Summary

- Built **unattended Runs**: `smithy-agent run "<intent>"` plans, researches,
  builds, checks and commits on its own branch until done, blocked, or in
  need of a person, and leaves a report. New crate `smithy-run`; supporting
  changes in `smithy-agent`, `smithy-tools`, `smithy-cli`.
- **Order of authority:** Checks (compiler, tests) → mechanical rules → Jev →
  the model. A task is done only when the runner's own checks pass.
- **Research made checkable:** fetched pages are saved by content hash;
  `cite_check` verifies every quoted finding against the saved page.
- **Jev calibrated** for five new decisions; 43 of 43 labelled cases on the
  correct side of their thresholds.
- **First real Run** on the Thor completed 1 of 6 tasks before being stopped
  for heat. Post-mortem identified ten causes; all ten are now fixed.
- **Second real Run** of the same intent **finished**: 3 of 3 tasks, 2 h
  23 min, 91% of 6.2 M prompt tokens from cache, 49 tests in the finished
  library (§9). It exposed a per-reply output cap that cost ~30 minutes and a
  shell guard that refused ordinary reads; both fixed.
- **Third real Run finished in 1 h 49 min**: 3 of 3 tasks, 154 requests,
  3.9 M prompt tokens (90% cached), no reply cut off (§9).
- **Optimised from its log** (§10): 107 of its 109 minutes were the model
  generating, three quarters of it thinking. The FP8 hybrid decodes 19–27%
  faster in 2.7 GiB less; research now runs side by side on three server
  slots, stops when its Note is done, and reuses Notes from a library shared
  between Projects; a thinking A/B made lookups think-free.
- **Fourth real Run finished in 53 minutes** with all of it: 3 of 3 tasks,
  research 9 minutes instead of 37, thinking down from 76% to 54% of what the
  model wrote (§10).
- **A real project** (§11): molad and Daf Yomi for `hebrew-calendar`, 3 of 3
  Tasks. Checked against Hebcal: 0 differences in 13,991 days of Daf Yomi
  (1923–2040) and 351 molad announcements, and 0 regressions over 41,030 days.
  16 h 18 min of clock for ~6 h 45 min of running: six human interventions,
  none about the code (a laptop restart, three Smithy bugs fixed that hour, the
  Thor's over-current throttling at MAXN — gone at 90 W — and a missing
  `clippy`). The project it started from had sound calendar arithmetic but
  zmanim in the wrong time zone and an app that doesn't build on Windows.
- **Thor re-served** with a tuned vLLM recipe: prefix caching works and is
  now reported per request (93% of a 16k prompt; time to first token 9.05 s →
  1.0 s), 8 GiB fixed KV cache (292k tokens), 28–46 tok/s decode on the
  FP8 hybrid.
- **Co-residency measured:** an idle second model costs nothing; a busy one
  costs Flash-Next 12–62%. Apodex does not fit beside it.
- **Von vs Jev** on Smithy's 63-case suite: Von 32 wrong, Jev 0 (one 429).
- **Network:** Thor was on 2.4 GHz Wi-Fi (144 Mbit/s link). Wired: 79–100 MB/s.
- **Model inventory** downloaded for the next phase (~210 GB); 230 GB of
  superseded copies removed.

## 2. Goal and constraints

Commander's-intent autonomy: one high-level intent, run unattended until
done or genuinely blocked. Rust first, designed to port to C and Python.
Research is treated as fundamental: cited, checked, and stored for reuse.
Constraints: single-request local model on the Thor (131k context at the
time); Jev reachable over a gateway that sometimes returns 429/503; Windows
host (PowerShell 5.1, Git Bash for tools).

Decisions from the design interview (2026-09-23): headless CLI; Brave search
available; build immediately after planning behind a legality/ethics
guardrail that stops and wakes the user; Windows toast plus a report file;
pre-existing tests may change but are checked for weakening; 8 h, 3 attempts
per task, stop after 2 consecutive blocked tasks; CMake+ctest and pytest as
the first non-Rust adapters; research notes in the repo, fetched pages
outside it.

## 3. System design

Design record: `crates/smithy-run/DESIGN.md`. Vocabulary: `CONTEXT.md`
(Run, Intent, Plan, Task, Attempt, Check, Checkpoint, Guardrail, Baseline,
Source, Note, Report).

### 3.1 Run lifecycle (`smithy-run/src/runner.rs`)

1. Refuse a dirty tree; branch `smithy/run-<id>` from HEAD.
2. **Guardrail** on the intent (fails closed; retries an unreachable Jev for
   five minutes).
3. **Preflight**: every program the toolchain's commands start with must be
   on the Checks' PATH.
4. **Baseline**: run the full suite and record which tests pass.
5. **Plan** (`plan.toml`): validated mechanically — ids, dependencies, ≤12
   tasks, every task has a `test` check, `min_tests ≥ 1`, checks start with
   the toolchain's own programs and chain only with `&&` (no `||`, `;`,
   pipes, `$(…)`, backgrounding). Committed as the first commit.
6. Per task: guardrail → research (as planned) → up to 3 Attempts × 6 check
   rounds. After a failed round the next move is chosen by rules first
   (compact at ≥80% context; last round hands off or blocks; three identical
   failures rule out "continue"), then by Jev among the rest.
7. **Checkpoint** when the task's checks, the full suite against the
   Baseline, and the cheat rules all pass.
8. **Finish**: final commit, `REPORT.md`, Windows toast. Interrupted work is
   stashed, never discarded.

Resume (`--resume [ID]`) reads `state.json` from the branch; an Attempt still
marked in flight is stashed as `…-crash-T<n>a<m>` and counted. `--allow T<n>`
clears a guardrail flag or gives a blocked task fresh attempts.

### 3.2 Language seam (`toolchain.rs`)

`Toolchain` holds build/test/lint commands, a test-output counter
(cargo, pytest, ctest), test-path globs, skip markers and assertion markers.
Rust, CMake+ctest (`-C Debug` for multi-config generators) and pytest are
detected; `.smithy/checks.toml` overrides any field or declares a custom
toolchain (unknown keys are errors). The Map, symbol index and call graph
remain Rust-only and are absent elsewhere.

### 3.3 Integrity

- **Git ownership** (`git.rs`): the model may run read-only git (status,
  diff, log, show, `stash list/show`); anything that moves a ref, rewrites
  the tree or talks to a remote is refused wherever it appears in a command.
- **Unattended hooks** (`unattended.rs`): prompts become logged refusals;
  writes to `.smithy/runs/` are refused; research Sessions may write only
  under `.smithy/research/`.
- **Cheat checks** (`cheat.rs`): on test files that predate the Run —
  deleted file, new skip marker, or net loss of assertions is a violation;
  other edits to assertion lines go to Jev. First detection fails the round
  with an explanation; a second blocks the task.
- **Test counting**: a test check that ran fewer than `min_tests` fails even
  on exit 0 (a filter matching nothing passes otherwise).

### 3.4 Research (`smithy-tools/src/research.rs`, `tools/cite_check.rs`)

- `web_fetch` saves every rendered page to
  `~/.local/share/smithy/sources/<sha256[:12]>.txt` and labels its output
  `source <id>`; `find`/`offset` reach into long pages; PDFs are read via
  `pdf-extract`.
- A finding is `- [kind] (key) claim — url {src:id} "exact quote"` or
  `{repo:path:line}`. `cite_check` verifies source existence, URL match, and
  quote presence (normalised; `...` elides; ≥4 words or ≥20 characters).
  `(key)` findings must span two domains.
- Notes live in `.smithy/research/`; `find_notes` retrieves them by question.
- In a Run, each question has a **depth**: lookup (8 min, pointed),
  decision (20 min, pointed), deep (45 min, full `/research`). Research may
  spend a third of the Run's hours (`--research-minutes` overrides). A note
  must be drafted early; the Session is nudged if it has none after 6/12/20
  tool calls.
- Shipped Skills in `~/.smithy/skills/` are upgraded when provably
  unmodified (hash manifest or known earlier release).

### 3.5 Jev (`smithy-agent/src/jev.rs`)

Decisions that force an action keep thresholds: guardrail, cheat, loop,
done, next move (a `choice` question), and reuse of an existing note.
Research is **reported, not gated**: "does this need outside sources?" and
"does this note answer it?" are recorded on each note and shown in the
report. Every decision is logged in `decisions.jsonl` with the exact state
shown to Jev; `--example jev replay` rescores a real run.

### 3.6 Logs and report

`--log off|events|full` (default full), outside the repo at
`~/.local/share/smithy/runs/<project>/<run>/`: `events.jsonl` (timed lines
per model request — tokens, cached tokens, ms — per tool call, per check,
per Session, per progress line) and `sessions/` (every conversation with
reasoning, in the REPL's stored-session format). `REPORT.md` on the branch
lists verdict, flags, tasks with checks and commits, blocked reasons with
failing output, baseline, research with Jev's numbers, refused commands,
stashes and every decision.

## 4. Jev calibration

`cargo run -p smithy-agent --example jev <suite>`, 2026-09-23. All 43 labelled
cases on the correct side.

| Suite | Should act | Should not | Threshold |
|---|---|---|---|
| research-needed | 0.79–0.93 | 0.07–0.10 | 0.5 (now reported only) |
| cheat | 0.89–0.97 | 0.06–0.16 | 0.5 |
| answered | 0.63–0.71 | 0.03–0.06 | 0.35 |
| next move | 5/5 acceptable picks | | confidence floor 0.3 |
| guardrail (allow side) | — | 0.02–0.05 | 0.3 |
| loop (existing) | 0.93–0.98 | 0.06–0.09 | 0.85 |
| done (existing) | finished 0.81–0.94 | unfinished 0.04–0.27 | 0.5 |

**The guardrail's must-stop side has not been calibrated.** The suite reads
those cases from a local file (`SMITHY_GUARDRAIL_CASES`), and none has been
written yet; every guardrail number here is for intents it must let through.
The guardrail itself runs on every Run and every Task. "Compact near
a full window" was the one low-confidence pick (0.22), which is why that
move is a rule. A sustained 429 outlasted 30 s of retries once; the
guardrail therefore retries for five minutes before failing closed.

Rerun on 2026-09-24 with the suite grown to 63 cases (shell 20, loop 6,
done 6, guardrail 10, research 6, next 5, cheat 6, answered 4): 62 on the
correct side; the 63rd was a 429 from the gateway, not a wrong answer.

## 5. First real Run

Full post-mortem: `crates/smithy-run/postmortems/2026-09-23-iso8601-trial.md`.

**Intent:** a library parsing ISO 8601 durations into `std::time::Duration`
(years/months rejected), with unit tests and a CLI. Run
`20260923-1736-7d8c` in `smithy-trial`.

| | |
|---|---|
| Wall clock | ~3 h 30 min over three processes (2 h 55 min recorded) |
| Model requests | 284 |
| Prompt tokens | 14.8 M (≈52 k per request) |
| Completion tokens | 387 k |
| Cached tokens | 0 reported |
| Tasks done | 1 of 6 |
| Pages fetched | 67 |

Time: research 43%, building 54%, planning 3%.

**What worked.** T1 was committed only after the build, 12 filtered tests
(6 required) and the full suite (17) passed under the runner. Every Jev
judgment was correct: guardrail allowed; "done" sent back an unfinished
answer (0.23); loop stopped a stuck model twice; next move escalated a
missing toolchain; cheat accepted a legitimate template-test removal (0.30).
Both research notes quoted primary sources verbatim — 21/23 and 30/30
findings verified — and the second caught a bug in T1's committed code.

**Causes and status.**

| # | Cause | Status |
|---|---|---|
| 1 | Run launched without `cargo` on PATH (operator error) | Fixed: preflight |
| 2 | Plan over-split; research on every task | Fixed: plan sized to work; depth per question |
| 3 | Full adversarial research method for every question | Fixed: depth sets method and time |
| 4 | Research read without writing a note | Fixed: draft early, nudged |
| 5 | Resume re-researched a draft note | Fixed |
| 6 | `cite_check` rejected dense quotes | Fixed |
| 7 | `git stash list` refused | Fixed |
| 8 | No prefix caching on vLLM | Fixed on the new server (§6) |
| 9 | One 57-minute turn before any check; scope creep into later tasks | Fixed: the task's checks run as the model works and end the turn when they pass; 20-minute build turns (`893bff2`) |
| 10 | No conversation logs | Fixed: Run logs |

## 6. Serving on the Thor

### 6.1 Recipe

pastoriomarco's NemoClaw-Thor recipe (NVIDIA forum thread 383269), image
`nemoclaw-thor/qwen38-flash-next-vllm:sm110-fast-v3` built locally from four
pinned Dockerfiles (base `vllm/vllm-openai:qwen38-flash-next`, digest-pinned;
kernel sources pinned to commits). Scripts reviewed before running: no sudo,
no remote-script execution, staged-and-validated snapshot preparation.

Differences from the previous launch command:

| | Before | Now |
|---|---|---|
| Prefix caching | off | `--enable-prefix-caching` (mamba cache mode "align") |
| Chunked prefill | off | on, 8192 batched tokens |
| Checkpoint | `local-inference-lab/…-NVFP4` | `RadixArk/…-NVFP4` @ `7b71922…` (base; FP8 hybrid not applied) |
| Context / slots | 131k / 1 | 256k / 1 |
| Tool parser | `qwen3_xml` | `qwen3_coder` |
| Served name | `Qwen3.8-Flash-Next` | `qwen3.8-flash-next` |
| Memory utilisation | 0.93 | 0.75 |

The two checkpoints share 1 of 208 weight files; the old one could not be
reused.

### 6.2 Memory (measured at startup)

| | GiB |
|---|---|
| Weights + non-torch | 82.92 |
| Peak activation | 1.6 |
| CUDA graphs | 0.04 |
| KV cache | 7.6 (277,701 tokens; 1.06 × 256k) |
| **Total** | **≈92 of 122.8** |

Model load: 79.42 GiB in 438 s; engine init 172 s. A 47.7 GiB per-layer
embedding table stays file-backed (mmap, random access); weights plus table
(127 GiB) exceed memory by design.

After a reboot the same 0.75 gave a KV budget of −0.71 GiB and the server
refused to start: CUDA on the Thor counts page cache as used, and loading the
weights fills it. The KV cache is now a fixed 8 GiB (`--kv-cache-memory-bytes`;
292,481 tokens, 1.12 × 256k) — see §9.

### 6.3 Prefix caching

Two identical requests, 12,077-token prompt, 256 completion tokens:

| | Time | Cache hits (vLLM metrics) |
|---|---|---|
| First | 21.9 s | 0 |
| Second | 10.9 s | 10,816 of 12,077 (90%) |

At the time the response `usage` omitted `prompt_tokens_details`. With
`--enable-prompt-tokens-details` it is reported per request, and Smithy's
report shows the rate: 91% over the second Run's 214 requests (§9).

### 6.4 Generation speed

Base checkpoint, one slot, temperature 0, natural stopping, MTP speculative
decoding (3 tokens):

| Prompt | Tokens | Time | tok/s |
|---|---|---|---|
| Rust duration parser with tests | 1200 (cap, reasoning) | 44.5 s | 27.0 |
| Python dedupe CLI | 1200 (cap, reasoning) | 47.2 s | 25.4 |
| 300-word explanation | 1200 (cap, reasoning) | 35.0 s | 34.3 |
| JSON, 8 records | 569 | 13.4 s | 42.4 |

MTP acceptance: 2,723 of 4,338 draft tokens (63%). A forced-length run
(`ignore_eos`) measured 22.6–25.3 tok/s. The recipe reports 37.6 tok/s on
code for the base checkpoint and 42.9 for the FP8 hybrid on its own
benchmark. Measured here on 25 September, the hybrid decodes 19–27% faster
in 2.7 GiB less and is now the default (§10). Peak temperature under
benchmark: 48 °C.

### 6.5 Network

The Thor's default route was Wi-Fi (2.4 GHz, 144 Mbit/s link): 1.5 MB/s
single-stream, 5–8 MB/s sustained from Hugging Face with a token. The wired
port had a static address from a direct laptop link and no IPv4 gateway;
IPv6 reached Hugging Face but the Xet storage backend stalled. Switching the
port to DHCP while keeping the static address gave 79–100 MB/s.

## 7. Model inventory (on the Thor, `~/thor-hf-cache`)

| Model | Repo | On disk | Role |
|---|---|---|---|
| Qwen3.8-Flash-Next | `RadixArk/Qwen3.8-Flash-Next-NVFP4` | 126 GiB | Primary (serving) |
| Apodex 1.1 mini | `apodex/Apodex-1.1-mini-NVFP4` | 23 GB | Candidate research model (35B MoE, Apache-2.0) |
| Qwen Image 2.1 | `Qwen/Qwen-Image-2.1` | 31 GB | Image generation, on demand |
| Nemotron 3 Nano 4B | `nvidia/NVIDIA-Nemotron-3-Nano-4B-BF16` | 7.5 GB | Candidate fast model for `explore` |
| Qwen3-ASR 1.7B | `Qwen/Qwen3-ASR-1.7B` | 4.4 GB | Speech to text |
| Qwen3-TTS 1.7B CustomVoice + tokenizer | `Qwen/Qwen3-TTS-12Hz-1.7B-CustomVoice`, `…-Tokenizer-12Hz` | 5.0 GB | Text to speech |
| Nemotron 3 Embed 1B | `nvidia/Nemotron-3-Embed-1B-NVFP4` | 1.0 GB | Embeddings for note retrieval |
| Qwen3 Reranker 0.6B | `Qwen/Qwen3-Reranker-0.6B` | 1.2 GB | Reranking |
| Von | `wfzyx/von` | 3.0 GB | Local Jev-style decision model, to evaluate |

Sizes are measured on disk; figures quoted earlier in the session from the
Hugging Face API double-counted large files.

**Evaluated and not pursued.** "Lumen 7B": the only model by that name is a
LoRA coding fine-tune of Qwen2.5-Coder-7B, not a calibrated decision model; a
third-party description attributing Jev-like capabilities, ECE figures and
latencies to it was not supported by any source. Von reports 72.0% against
Jev's 96.6% on a 49-task third-party decision benchmark. Measured on Smithy's
own suite it put 32 of 63 cases on the wrong side (§9), so it is not an
offline fallback for these decisions.

## 8. Open items

The nine items this section listed on 24 September (cache reporting,
co-residency, turn length, settings, a fresh trial Run, OS updates, Von,
research routing, housekeeping) were all worked the same evening; §9 has the
results. What remains:

1. **Which change did how much.** The fourth Run measured §10's changes
   together (53 minutes, against an estimate of 1 h 15 min); it cannot say how
   much of T2's gain was the hybrid's speed and how much the better research.
2. **Research is bound by its clock.** None of the six A/B research sessions
   finished its Note inside its time (8 or 20 minutes), so stopping when done
   saved nothing there, and a lookup whose source is behind a paywall (ISO
   8601's own text) stalls either way.
3. **The thinking rule rests on three questions**, one sample each. It is a
   direction, worth re-checking on the next Runs' logs.
4. **Memory release on the Thor.** A GPU process that stops leaves its memory
   counted as used until the page cache is dropped (needs sudo). A Run cannot
   restart the server unattended until this is handled.
5. **The shell guard is lexical.** It reads commands, not what they do;
   Python's `//` inside a heredoc fed to `python` is still refused as a path.
   Acceptable while the model has the `write` tool and other ways round.

From the real project (§11). The first four were fixed on 27 September
(`d47fdb0`); the fifth is a direction, not a bug:

6. ~~**Preflight should run the checks' tools.**~~ It now probes cargo
   subcommands that do not ship with cargo (`cargo clippy`, `cargo fmt`,
   `cargo nextest`) and says how to install them.
7. ~~**The planner researches.**~~ The planning Session has no web tools and
   is told that what must be looked up goes into a task's `research`.
8. ~~**Run records name local paths.**~~ State, plan, report, decisions and
   checkpoint messages write the Project as `.` and the home directory as
   `~`, in every spelling a path takes (native, forward slashes, Git Bash,
   JSON-escaped).
9. ~~**A Run should survive the session that starts it.**~~ `smithy-agent run
   --detach` starts it as a process of its own. A restart still ends it;
   `--resume` carries on.
10. **Checks cannot see a wrong reason not to test.** The Daf Yomi comment
    that excused cycles 8–9 from testing was wrong. A reviewer pass against an
    outside reference, where one exists, is worth considering.

## 9. Follow-up (24 September, evening)

The open items, worked after an OS update and reboot of the Thor.

**Starting the server.** The first restart after the reboot refused to start:
`Available KV cache memory: -0.71 GiB` at 0.75. On the Thor, CUDA counts page
cache as used memory, and loading 126 GB of weights fills the page cache, so a
budget taken as a fraction of memory comes out short. The launch script now
fixes the KV cache at 8 GiB (`--kv-cache-memory-bytes`; 292,481 tokens, 1.12 ×
256k). A failed or stopped GPU process also leaves its memory counted as used
(93 GB once, 10 GB after the Nano test) until
`echo 3 > /proc/sys/vm/drop_caches`.

**Cache reporting (1).** With `--enable-prompt-tokens-details`, responses
carry `cached_tokens`. On a 16k-token prompt: 14,976 of 16,030 served from
cache on the repeat, and time to first token 9.05 s cold → 1.0 s warm.

**Co-residency (2).** `~/thor-setup/bench.py`: three prompts behind a
16k-token shared prefix, each sent twice, temperature 0, 400 tokens. Decode
tok/s:

| Flash-Next with | code | prose | JSON | Memory used / available |
|---|---|---|---|---|
| nothing | 35.4 | 22.2 | 38.3 | 101 / 20 GB |
| Nemotron-3-Nano-4B loaded, idle | 35.4 | 22.2 | 38.3 | 114 / 8 GB |
| Nemotron-3-Nano-4B, 2 requests running | 31.2 | 8.3–18.8 | 14.4 | 114 / 8 GB |

An idle neighbour costs nothing; a busy one costs Flash-Next 12–62%, and
itself decodes at only ~16 tok/s for two streams. The two share memory
bandwidth, which is what decoding spends. Apodex (23 GB of weights) does not
fit beside Flash-Next at all (20 GB available). So: small models may stay
resident for occasional use (speech, embeddings); nothing should run
continuously beside a Run; Apodex is a swap, not a neighbour.

**Shorter turns (3).** Commit `893bff2`: the Task's own checks run every six
tool calls once a file has changed, and the turn ends the first time they
pass; build turns return to the runner after 20 minutes.

**Settings (4).** `provider.json` names `qwen3.8-flash-next`.

**OS updates (6).** 31 packages (none kernel, L4T or Docker), then a reboot.

**Von vs Jev (7).** Commit `c800125`: `JEV_ENDPOINT` / `JEV_MODEL` /
`JEV_API_KEY` point Smithy at any server with the same API; the gateway key is
never sent elsewhere. On Smithy's 63-case suite (`--example jev`), on the same
evening:

| | Wrong side | Notes |
|---|---|---|
| Jev | 1 of 63 | the one was a 429, not a wrong answer |
| Von 1.2 (`von serve`, GPU) | 32 of 63 | nearly every answer 0.2–0.4; flagged none of the destructive commands, loops or weakened tests |

Von is not a fallback for these decisions.

**Research on Apodex (8).** Not pursued: it cannot be resident beside
Flash-Next, and a model swap costs ~10 minutes each way.

**Housekeeping (9).** 19 partial downloads (381 MB) removed, and the two
superseded copies of Flash-Next (230 GB) deleted. The FP8 hybrid is not
worth building while there is no memory pressure.

The laptop, too, had only its old static address on the router's port and
no IPv4 route: Jev (IPv4-only) was unreachable until a second address with
the router as gateway was added.

**Second real Run (5).** The same intent, from a clean `master`, on the new
server with shorter turns: run `20260924-2200-4e00`, **done — every Task's
Checks pass**, unattended.

| | First Run (23 Sep) | Second Run (24 Sep) |
|---|---|---|
| Result | 1 of 6 Tasks, stopped by hand | 3 of 3 Tasks, done |
| Time | ~3.5 h | 2 h 23 m |
| Prompt tokens | 14.8 M, none cached | 6.2 M, 91% from cache |
| Research | 90 min, full method on every question | 36 min: two lookups (8 min), one decision (20 min) |

Planning 10 min; T1 24 min (13 grammar tests, one round past the 20-minute
turn); T2 63 min (11 fraction tests; it rewrote three of T1's assertions from
"rejected" to exact values, as its task required); T3 10 min (a CLI and 13
integration tests). The finished library: 49 tests pass; `iso-duration
P3DT4H30M` prints `275400`; `P1Y` is refused with the reason. The early check
ended three turns the moment their Task's checks passed.

What it found:

- **The reply cap.** About 30 of T2's minutes went to three replies that
  thought for 15–16k tokens, designing a whole file, and hit the 16,384-token
  cap before writing it. The thinking is never replayed to the model, so each
  retry began again, and the session's correction ("give ONLY the final
  answer") pointed the wrong way. Fixed in `fa9d717` and `99044be`: no cap for
  the local server, a reply in flight at the turn limit may finish (15-minute
  grace), a cut-off reply is told what was lost and to take one concrete step,
  and the task prompt says up front to work in small steps and write plans
  down.
- **Which tests are protected.** T2 spent a long stretch deciding whether it
  could change a test T1 had written. The task prompt now names the Run's base
  commit.
- **The shell guard refuses ordinary reads.** Nine read-only commands (`grep`
  and `sed -n` on `src/`, `ls -R src`, `cargo test … | tail`) were refused as
  "reaching outside the Project", most likely on `2>/dev/null`, Git Bash
  paths (`/c/Users/…`) and `~`. Fixed in `c8c2ec2` (below).

**Third real Run.** The same intent from the same clean `master`, on a build
with the fixes above and the shell guard repaired: run `20260925-0112-de54`,
**done — every Task's Checks pass**, in **1 h 49 min**.

| | First | Second | Third |
|---|---|---|---|
| Result | 1 of 6, stopped | 3 of 3 | 3 of 3 |
| Time | ~3.5 h | 2 h 23 m | 1 h 49 m |
| Model requests | 284 | 214 | 154 |
| Prompt tokens | 14.8 M, 0% cached | 6.2 M, 91% | 3.9 M, 90% |
| Replies cut off | — | 3 | 0 |
| Refused commands | — | 9 (8 wrong) | 4 (2 wrong) |

Planning 9.5 min, research 37 min (two lookups and a decision, as before),
T1 15 min, T2 42 min, T3 5 min. T2, the task that lost half an hour in the
second Run, made 23 replies in its first turn against 8, the longest with
7.5k tokens of thinking, none cut off. The CLI gives `0.5` for `PT0.5S`,
`275400` for `P3DT4H30M`, `1.5` for `PT1,5S`, and refuses `P1Y`, `P1W2D` and
`-P1D` with reasons. In a test comment the model noted an arithmetic slip in
its own plan (274,200 for 275,400) and used the right value.

**The shell guard.** `c8c2ec2` fixed the second Run's three causes: Git Bash
drive paths (`/c/Users/…`) are judged as the Windows paths they are, the
standard streams and `/dev/null` are not files outside the Project, and a
bare `\` left by splitting a regex is an escape, not a drive root. Of the
third Run's four refusals, two were right (a temp directory and `/tmp`
outside the Project) and two were heredoc bodies — a test comment reading
`(dur-date / dur-time)` and Python's `//` — read as paths. A heredoc body is
now skipped when it only becomes a file: its program is `cat` or `tee`,
nothing pipes it onward, it is not inside `$(…)`, and its terminator is
found. A heredoc fed to `bash`, `python` or `| sh` is still read as code, so
the Python case stays refused.

## 10. Optimisation (25 September)

**Where the third Run's time went**, from its event log:

| Phase | Replies | Model time | Generated | Thinking share |
|---|---|---|---|---|
| Planning | 6 | 9.4 min | 16k tokens | 60% |
| Research (3 questions) | 62 | 36.6 min | 71k | 80% |
| Building (3 tasks) | 86 | 61.1 min | 134k | 77% |
| **Total** | **154** | **107 of 109 min** | **221k** | **76%** |

Checks took six seconds in all; tool calls and cached prefill are small. The
Run is model generation at ~35 tok/s, three quarters of it thinking, and
every research session ran to its time limit. So there were three levers:
generate faster, generate less, or generate several things at once.

**Faster: the FP8 hybrid.** The recipe's prepare script rewrites four shards
of dense side-weights as blockwise FP8 from the local checkpoint (about a
minute, no download; worst per-tensor error 3.5%). Same benchmark as §9,
three slots:

| | code | prose | JSON | model memory |
|---|---|---|---|---|
| Base | 35.3 | 21.9 | 38.2 tok/s | 79.4 GiB |
| FP8 hybrid | 43.7 | 27.9 | 45.5 tok/s | 76.8 GiB |

+19–27% decode in 2.7 GiB less. It is now the launch script's default.

**Several at once: slots.** Three server slots cost a single stream nothing
(35.3 / 21.9 / 38.2 with one slot or three). Several streams at once do not
come free, though: on the base checkpoint two streams decoded 35.1 tok/s
together and three 43.7 (about 1.1× and 1.4× one stream); on the hybrid,
39.5 and 45.9. The model is a mixture of experts, so different conversations
wake different experts, and speculative decoding already fills part of the
batch; each stream slows by about 30%. It still pays for research, which is
bound by its clock rather than its tokens: questions run side by side finish
when the longest does. With `--slots N` above one (`SMITHY_RUN_SLOTS`), a Run
now researches every question its plan asks before the first Task, N at a
time; each still passes its Task's Guardrail, has its need reported and
reuses a Note that answers it (`0a9b962`). The launch script serves three
slots.

**Less: research that stops when done.** A research turn now ends when its
Note meets the skills' own definition of done, checked mechanically — Status
`verified`, `cite_check` passing, an Unknowns section and a written
Implication. Jev still only reports whether the Note answers the question.
In the A/B below no session got there inside its time, so this saved nothing
yet; it bounds the case where a Note is finished early.

**Less: a note library.** Every Note a Run writes is also kept in
`~/.local/share/smithy/library` (laid out like a Project's notes, so the same
search reads it). A later Run, in any Project, reuses one that answers its
question — the same bar as before, including Jev's threshold — and copies it
in instead of researching. The three trial Runs re-researched the same ISO
facts each time because each Run's Notes stayed on its branch.

**Less: thinking, by depth.** Qwen can be asked not to think
(`enable_thinking: false`). `smithy-agent research "Q" --thinking on|off`
runs one question exactly as a Run would and reports what it cost and how
good the Note is. The third Run's three questions, each run both ways at the
same moment on the hybrid server:

| Question | Thinking | Replies | Verified | Jev: answers it |
|---|---|---|---|---|
| Week and `T` rules (lookup, 8 min) | on | 6 | 1 of 9 | 0.68 |
| | off | 61 | 6 of 14 | 0.76 |
| Where a fraction may go (lookup, 8 min) | on | 10 | 0 of 0 | 0.08 |
| | off | 74 | 0 of 0 | 0.18 |
| Signs on durations (decision, 20 min) | on | 31 | 8 of 12 | 0.74 |
| | off | 233 | 6 of 10 | 0.78 |

Thinking made each lookup reply a long deliberation — six actions in eight
minutes — and not thinking did ten times as much work in the same time and
verified more. On the decision, thinking found the fact that settled it (ISO
8601-2:2019 allows a leading `-` as an extension) and not thinking said, in
its Unknowns, that it had not reached that clause; Jev scored them alike. The
middle question stalled both ways on ISO's paywalled text. So a lookup now
runs without thinking and a decision or deep research with it
(`811ea1e`); building keeps thinking.

**The estimate.** From the third Run's breakdown: research side by side
would take its questions in about 20 minutes rather than 37; the hybrid takes
roughly a fifth off every model minute. Planning and building's 71 minutes at
the hybrid's speed are about 57, plus 20 of research: about 1 h 15 min.

**The fourth Run** measured it: the same intent from the same clean `master`,
with everything above and `--slots 3` — run `20260925-0612-3aa0`, **done —
every Task's Checks pass, in 53 minutes**.

| | Third Run | Fourth Run |
|---|---|---|
| Wall clock | 1 h 49 m | **53 m** |
| Planning | 9.4 min | 10.7 min |
| Research | 37 min, one question at a time | **9 min**, two lookups side by side, no thinking |
| T1 / T2 / T3 building | 15 / 42 / 5 min | **9.6 / 17.8 / 4.6 min** |
| Model requests | 154 | 240 |
| Tokens generated | 221k, 76% thinking | **125k, 54% thinking** |
| Prompt tokens cached | 90% | 92% |
| Refused commands | 4 (2 wrong) | 2 (both right: a temp directory and `~/.cargo`) |

It beat the estimate, and almost all of the difference is T2: 18 minutes
instead of 42. Two things plausibly account for it, and this run cannot
separate them: the hybrid's speed, and better research — the two lookups ran
without thinking and verified 13 of 24 and 19 of 25 findings, where the
third Run's T2 went in with less to build on. More requests but fewer tokens:
research without thinking makes many small calls instead of few long ones.

The library now holds both Notes, so a fifth Run of this intent would
research nothing. The finished CLI gives `0.5` for `PT0.5S`, `275400` for
`P3DT4H30M` and `1.5` for `PT1,5S`, and refuses `P1Y` and `P1W2D` with
reasons; it prints its usage for `-P1D`, reading the minus sign as an option —
a real edge case none of the 26 tests covers.

## 11. A real project: hebrew-calendar (25 September)

The first Run against an existing codebase instead of a fresh toy: run
`20260925-0707-82d4` on `hebrew-calendar` (a public GitHub repository: Rust
library `hebrew_core`, Tauri v1 + Axum app `hebrew_app`). **Done — every
Task's Checks pass**, 3 of 3 Tasks, 07:07–23:25 UTC.

### 11.1 The intent

> Add two features to hebrew_core, the calendar library crate (leave
> hebrew_app and src-tauri alone): 1. The molad of any Hebrew month […] as it
> is announced on Shabbat Mevarchim, built on the crate's existing
> molad-of-Tishrei arithmetic rather than a second implementation. 2. The Daf
> Yomi of any Gregorian date […] in the cycle that began on 11 September 1923,
> including every change to the cycle's length since then (such as
> Shekalim's). Both need unit tests that check against published values […]
> and both should be reachable through the crate's public API.

One preparation commit (`ae4b7c1`) added `.smithy/checks.toml` restricting
build, test and lint to `-p hebrew_core`, since the GUI crate cannot build
headless on the Thor.

### 11.2 The project as it stood (`5ca089e`)

About 6,100 lines: `hebrew_core` (calendar 761, holidays 1,024, parsha 553,
zmanim 545, lib 276) with 102 tests, and `hebrew_app` (~1,000 lines of Rust
and two frontends). Assessed against Hebcal's API (diaspora settings) with a
scratch harness that prints the library's answer for every day:

| Area | Checked | Result |
|---|---|---|
| Gregorian → Hebrew date | 7,670 days, 2015–2035 | 0 differences |
| Holidays the library knows (45 kinds, diaspora) | every occurrence 2015–2035 | 0 differences |
| Weekly portion | 1,096 Shabbatot | 0 differences in naming a portion; the 73 holiday Shabbatot are labelled "Haftarah Only" where the holiday's own reading applies |
| Zmanim (sunrise, sunset, dawn, nightfall, chatzot, latest Shema ×2) | Jerusalem and New York, 24 dates in 2025 | within 1 minute as instants; **60 minutes off on the clock** under daylight time |

So the calendar arithmetic was sound. The problems were elsewhere:

- **Zmanim are printed in the wrong zone.** `GeoLocation` carries one fixed
  offset (the presets say "UTC+2 (standard), +3 in summer" and apply +2 all
  year). `GeoLocation::new` defaults it to 0; the API's `/zmanim` and
  `/convert` never set it, and the GUI sets `with_timezone(0) // UTC for
  now`. A New York user asking the API for sunrise gets a UTC time labelled
  as nothing.
- **The app.** The default build (`gui` + `server`) fails on Windows:
  `icons/icon.ico not found` (the icon set lives in the leftover `src-tauri/`).
  `tauri.conf.json` serves `frontend/` in development (1,247 lines) and
  `frontend/dist/` in release (454 lines, an older copy). Tauri v1 with
  `api-all`. The API crate's 13 tests pass (`--no-default-features
  --features server`).
- **Limits and errors.** `calculate_day`, which the app goes through, refuses
  dates after 2050 although the conversion is good well beyond. The
  `DateOutOfRange` error's text is hard-coded as "(0 AD to 2050 AD)", so any
  other use of it reads wrongly.
- **Scope.** Diaspora only (no Israel mode for holidays or portions); no Erev
  days, special Shabbatot, Pesach Sheni, Purim Katan and other minor days.
- **Tests and hygiene.** `test_year_zero_boundary` prints and asserts nothing;
  the zmanim tests accept sunrise anywhere between 3 and 8 am. CI tests
  `hebrew_core` only. Five clippy warnings, which the `lint` check does not
  fail on (no `-D warnings`). `format_display_date` gives "3 15, 2024 AD".

### 11.3 What the Run produced

`e7984a2` T1 molad (`molad.rs` 335 lines + 253 of tests), `d3d2ab9` T2 Daf
Yomi (`daf_yomi.rs` 384 + 376), `2211b99` T3 public API (`lib.rs` +193). T1
hoisted the molad-of-Tishrei parts arithmetic out of
`hebrew_calendar_elapsed_days` into `DateConverter::molad_parts` (+42/−11 in
`calendar.rs`) and built on it, as the intent asked. Tests: 102 → 131. Clippy
warnings: 5 → 5 (T1 removed one old warning; the Run added none). Six research
Notes (43 of 58 findings verified by `cite_check`).

Checked against Hebcal:

| | Checked | Differences |
|---|---|---|
| Daf Yomi | 13,991 days: 1923–24 (cycle 1's start), 1930, 1955, 1968–76 (the 2702→2711 change at cycle 8, June 1975), 1982, 1990, 1997, 2005, 2009–2040 | **0** |
| Molad | 351 Shabbat Mevarchim announcements, 2009–2040: weekday, hour, minute, chalakim | **0** |
| Molad Tishrei 5786 | Monday 22 Sep 2025, 12:10 pm + 7 chalakim; Rosh Hashanah on the Tuesday | as published |
| Regression | Hebrew date, holidays and portion for all 41,030 days 1923–2035, before vs after | **0** |

What the review found in the new code:

- **A false "known imprecision".** `daf_yomi.rs` and its tests say the constant
  2711-daf step "is 6 days early for cycles 8–9", citing published siyumim of
  1982-11-21 and 1990-04-24 against the model's 1982-11-24 and 1990-04-27, and
  so assert no cycle 8 or 9 end. Hebcal gives Niddah 73 on 1982-11-24 and
  1990-04-27, as the module does. The comment's own numbers are three days
  *before* the model's, not six after. I could not settle which siyum dates
  its source meant; a maintainer reading it would believe the code is wrong.
- **The pre-1923 error** reuses `DateOutOfRange`, so it renders as "Date out
  of supported range (0 AD to 2050 AD): Daf Yomi cycle 1 began on
  11 September 1923; …".
- Otherwise the modules are careful: conventions stated at the top (raw mean
  conjunction, no dehiyyot, Jerusalem mean time, noon-based remainder),
  constants named, the four small Kodashim tractates' shared folio numbering
  handled by walk index, `Molad::parts_since_epoch` as an inverse with tests.
  Neither feature appears in the app (`DailyData`), as the intent asked.
- **The branch carries the Run's records** (`.smithy/runs/…`: `REPORT.md`,
  `decisions.jsonl`, `state.json`, ~3,000 lines) and they contain absolute
  paths with the laptop's and the Thor's user names. The repository is public;
  the branch should not be merged as it stands.

### 11.4 Timeline

| UTC | Event |
|---|---|
| 07:07 | Started on the laptop, from the Claude Code session. Planning. |
| 08:12 | Planner's turn hits its 60-minute limit; plan written at 08:25 (3 Tasks, 6 questions). |
| 08:25–08:59 | Research, three at a time: T1's three questions (20 min), then T2's. |
| 08:59 | **Stopped: laptop restarted**; the Run was a child of the session. Moved to the Thor, launched detached (`setsid nohup`); resume learned to take a log directory from another machine and new ceilings (`57fced1`). |
| 12:12 | Resumed on the Thor; T2's research. |
| 12:18 | **Stopped: HTTP 400** "min_p … not yet supported with speculative decoding" on all three concurrent sessions. The per-provider "don't send min_p" flag was only honoured by the first request to be refused; the others retried with it (`95dadcd`, with a test against a local server that fails on the old code). |
| 12:23 | Resumed. |
| 12:29–12:32 | T1 attempt 1: the shell guard refused a scratch directory, the model put a probe file in `src/`, Jev (68–80%) held back `rm` of it, and the loop stopped. The failure question then quoted "Compiling hebrew_core…" rather than the error. **Stopped by hand**; `c9b8cdd`: a per-Run scratch directory the guard allows, deleting files the session itself wrote needs no review, the failure question quotes the first error line. |
| 12:39–14:37 | T1 attempts 2 and 3: five 20-minute turns without a passing check, then green after 18 tool calls. **T1 done.** |
| 14:37 | **Stopped by hand** at T2's start: the Thor's over-current alarm (`soctherm` oc3) had counted 824 in the first hour at MAXN and 1,261 by now. Moved room and outlet; replaced a flaky Ethernet cable; the direct link's DHCP (no server on that cable) was dropping the link and was set to a static address. |
| 20:00 | Resumed, still at MAXN: oc3 1 → 85 → 99 within minutes of generation. |
| 20:12 | **Stopped by hand** to change the power mode. The reboot into 90 W hung at "failed to transfer message -62 / failed to read time"; the next cold boot was clean. |
| 21:00 | Resumed at 90 W. T2 attempts 3 and 4. |
| 22:24 | **T2 done.** |
| 22:24–23:06 | T3 attempt 1: `cargo clippy` is not installed on the Thor. 42 minutes, 72 requests, 38 refused commands (`rustup component add clippy` and ways round it; all refused rightly). `clippy` installed by hand. |
| 23:25 | T3 attempt 2 green after 32 tool calls. **Done.** oc3: 0 new events in ~3 h of generation at 90 W. |

### 11.5 Where the time went

Six starts, about 6 h 45 min of running (Smithy's report charges 6 h 15 min
against the 24-hour budget) in 16 h 18 min of clock. From the two event logs:

| Phase | Requests | Model time | Generated | Thinking |
|---|---|---|---|---|
| Planning | 43 | 77.5 min | 196k | 92% |
| Research (6 questions, some twice across the restarts) | 222 | 73 min, three at a time | 129k | 45% (lookups 0%) |
| T1 (3 attempts) | 85 | 120 min | 302k | 91% |
| T2 (4 attempts) | 95 | 92 min | 230k | 84% |
| T3 (2 attempts) | 104 | 59 min | 124k | 87% |
| **Total** | **549** | **~7 h** (research overlapped) | **~980k** | **~83%** |

Smithy's own report counts 458 requests and 16.1 M prompt tokens (92% from
cache); the logs include sessions cut off by the stops. Checks took seconds.

Against the fourth trial Run (53 minutes, 125k tokens, 54% thinking), the
difference is mostly the work: building thought 84–91% of the time, T1 and
T2 each spent a whole attempt designing across 20-minute turns without a
single passing check, and contexts passed Smithy's 32k warning. Planning is
the outlier: 78 minutes against 10. The planner made 18 web calls of its own
(15 fetches, 3 searches) and 25 shell commands, when research is the Run's job
after the plan.

### 11.6 Jev and the rules

515 decisions: 464 loop checks (451 continue, 9 nudges, 4 stops), 17
guardrail (all build), 11 next-step (10 later shown right, none wrong), 11
research reports, 6 each of done and answered. 88 refused commands; the
largest groups were writes outside `.smithy/research/` before `c9b8cdd` and
T3's 38 attempts to install clippy. No refusal cost correct work; the scratch
directory's absence did, until fixed.

### 11.7 What it showed

- **The order of authority held on real work.** Every commit on the branch
  was earned by the runner's checks, and the result is right on every date
  an independent reference could check, including history the tests
  deliberately skip (cycles 8–9). Nothing that passed before was weakened.
- **The unattended part did not.** Six human interventions, none about the
  code: one launch mistake (process tree), three Smithy bugs (all fixed the
  same hour), one hardware limit (MAXN), one missing tool.
- **Where the checks' blind spot is:** a confident, wrong comment that
  justified *not* testing something. Only an outside reference caught it.

### 11.8 Afterwards

The review's findings about the project itself were then fixed by hand
(with Claude Code), and hebrew-calendar 0.2 was published: Israel and
diaspora observance, every holiday Hebcal lists, Torah readings for both,
zmanim in the location's own time zone, and a new desktop app and web page.
A checker in the repository (`tools/check_against_hebcal.py`) now compares
every day from 1950 to 2080 with Hebcal in both observances and finds no
differences. The Run's molad and Daf Yomi code went in unchanged apart from
the wrong comment, which became two tests.

## Appendix A. Commits

```
736e89a Unattended runs: the design, and words for it
5ad2b8a smithy-run: the toolchain seam and Checks
e614806 smithy-run: git belongs to the runner
542282b smithy-run: the Plan, and what a check may say
fe2b27e smithy-run: Run state, the decision log, and the morning report
4fbb3eb Research you can check: saved sources, cite_check, find_notes
0a5a2ea Jev: the decisions an unattended Run asks for, calibrated
b448859 smithy-run: cheat checks on tests that predate the Run
b401a34 smithy-run: the runner — intent to plan to checked commits, resumable
4b6bcd7 smithy-agent run: unattended Runs from the terminal
7a97436 Run: say whether web_search is on; DESIGN records what changed
e1fbccb cite_check: a dense quote identifies as well as a sentence
2aaf432 Run: preflight the toolchain; don't claim a cache rate nobody reported
904ee4b cmake adapter: ctest -C Debug
2214b4b Run: don't research the same question twice on resume
7f603c6 Run: the model may look at stashes
05e4f7f Research: draft the note early, and be told to if you don't
56fcc63 Runs: research is the exception, plans are as small as the intent
eca3e11 Runs: research scales with the question, not a cap; post-mortem
acd62fe Run logs: every request, tool call and check timed; every conversation kept
f0b78f8 Post-mortem: keep the log row inside its table
c992ab4 Research: Jev reports, it does not gate
85edae5 Report: unattended Runs and the Thor, 23-24 September
893bff2 Runs: a build turn ends when its Task's checks pass
c800125 Jev: the endpoint and model can point at a compatible server
fa9d717 Limits that fired on good work: the reply cap, and a reply cut off in flight
99044be Prompts: say why thinking is lost, and which tests are the Run's own
a93e551 Report: the nine open items, and the second real Run
1a82996 Report: rewrite the cover around two runs; bring the technical report current
c8c2ec2 Shell guard: Git Bash drive paths, /dev/null, and a bare backslash
aa08fd8 Shell guard: a heredoc that only writes a file is data; report run 3
0a9b962 Research: side by side, done when done, shared between Projects
811ea1e Research: a lookup does not think; a decision does
7fbf53d Report: where the time goes, and what was changed because of it
27c69ff Report: the fourth Run — 53 minutes, measured
85cf5df Scrub LAN addresses and a local username from the repo
f1d2be2 Report and DESIGN: the guardrail's stop side is uncalibrated
57fced1 Resume: new ceilings, and a log directory from another machine
95dadcd LM Studio provider: every request refused for min_p retries, not only the first
c9b8cdd Runs: a scratch directory the shell allows, and deleting what you made
9a04d99 Report: a real project — hebrew-calendar, checked against Hebcal
d47fdb0 Runs: what the hebrew-calendar Run needed a person for
85c4338 Formatting, and the float literals the newer compiler will reject
```

## Appendix B. Thor state

| | |
|---|---|
| Addresses | Wi-Fi from the router (internet); the direct laptop link static only, DHCP off (it timed out on a cable with no DHCP server and dropped the link, 25 Sep) |
| Power mode | 90W (mode 2) since 25 Sep; MAXN tripped `soctherm` oc3 hundreds of times an hour under inference |
| Runs | launched detached with `~/thor-setup/smithy-run.sh` (keys from `~/.config/smithy/keys.env`, mode 600); logs in `~/thor-setup/runs/` |
| SSH | key `~/.ssh/id_ed25519_thor` (laptop), alias `thor` |
| User | a regular account in the `docker` group |
| Rust | 1.98.1 (rustup, user-local), with `clippy` since 25 Sep |
| Server | container `flash-next`, port 8000, detached |
| Start command | `~/thor-setup/start-flash-next.sh`: the recipe's `serving/start-qwen38-flash-next-fast.sh` with the FP8-hybrid snapshot `7b719225…-fp8hybrid` (`FLASHNEXT_MODEL_REV` selects the base), port 8000, three slots (run Smithy with `--slots 3`), 256k, detached, `--enable-prompt-tokens-details`, and an 8 GiB KV cache (`FLASHNEXT_KV_GIB`). `FLASHNEXT_NO_RM=1` keeps a failed container's logs. Clear caches first if `free -g` shows memory held. |
| Caches | `~/thor-hf-cache` (HF home, token file), `~/thor-vllm-cache`, `~/thor-torch-cache`, `~/thor-flashinfer-cache` |
| Setup scripts and logs | `~/thor-setup/` |
| Monitor | `~/sysmon-tui/target/release/sysmon-tui` (`ssh -t thor …`) |
