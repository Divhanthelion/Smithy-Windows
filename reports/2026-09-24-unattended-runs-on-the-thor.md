# Teaching a local model to work while we sleep

*A field report on Smithy's unattended Runs: three real runs on a Jetson AGX
Thor, what the first one broke, what it took for the next two to finish, and
where the time goes. 23–25 September 2026.*

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

## What it is, honestly

A local model on a Thor writes code at 30–45 tokens a second. Work a hosted
frontier model does in minutes takes this one the better part of an hour, and
watching it live is painful. Overnight, on a machine that is otherwise idle,
private and free, that trade looks different — which is the point of making
it run unattended.

On paper, the changes above bring the same run to about an hour and a
quarter, and closer to an hour for a topic it has researched before. That is
an estimate from measurements of each piece, not a run, and the next run will
say whether it holds. The bar hasn't moved: done while we sleep, and a report
the next morning we can believe.

---
---

# Technical report

**Period:** 2026-09-23 17:36 UTC – 2026-09-25 05:10 UTC
**Repository:** `Smithy-Windows` (private GitHub), 34 commits from `736e89a`
to the commit carrying this report, 549 tests passing (smithy-agent 270 + 28
integration, smithy-tools 157 + 7, smithy-run 62 + 16 end-to-end, smithy-cli
9).
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
  between Projects; a thinking A/B made lookups think-free. Estimated, not
  yet measured: about 1 h 15 min for the same Run.
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

The guardrail's must-stop cases are read from a local file
(`SMITHY_GUARDRAIL_CASES`) and are not kept in the repository. "Compact near
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

1. **The combined effect is unmeasured.** §10's changes — the FP8 hybrid,
   research side by side, research that stops when done, the note library,
   and lookups without thinking — were each measured on their own, not
   together in a Run. The estimate in §10 is an estimate.
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

**What to expect.** Not yet measured in a Run. From the third Run's
breakdown: research side by side would take its three questions in about 20
minutes rather than 37; the hybrid takes roughly a fifth off every model
minute; the library takes research to zero for questions a Run has already
answered. Together: planning and building's 71 minutes at the hybrid's
speed are about 57, plus 20 of research, so the third Run's work in about
1 h 15 min, and about 1 h on a topic already in the library. Lookups without
thinking change what research produces in its time, not how long it takes.

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
```

## Appendix B. Thor state

| | |
|---|---|
| Addresses | one from the router by DHCP, plus the static address from the direct laptop link (kept) |
| SSH | key `~/.ssh/id_ed25519_thor` (laptop), alias `thor` |
| User | a regular account in the `docker` group |
| Rust | 1.98.1 (rustup, user-local) |
| Server | container `flash-next`, port 8000, detached |
| Start command | `~/thor-setup/start-flash-next.sh`: the recipe's `serving/start-qwen38-flash-next-fast.sh` with the FP8-hybrid snapshot `7b719225…-fp8hybrid` (`FLASHNEXT_MODEL_REV` selects the base), port 8000, three slots (run Smithy with `--slots 3`), 256k, detached, `--enable-prompt-tokens-details`, and an 8 GiB KV cache (`FLASHNEXT_KV_GIB`). `FLASHNEXT_NO_RM=1` keeps a failed container's logs. Clear caches first if `free -g` shows memory held. |
| Caches | `~/thor-hf-cache` (HF home, token file), `~/thor-vllm-cache`, `~/thor-torch-cache`, `~/thor-flashinfer-cache` |
| Setup scripts and logs | `~/thor-setup/` |
| Monitor | `~/sysmon-tui/target/release/sysmon-tui` (`ssh -t thor …`) |
