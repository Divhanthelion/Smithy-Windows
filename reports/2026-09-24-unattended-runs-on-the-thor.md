# Teaching a local model to work while we sleep

*A field report on Smithy's unattended Runs, the first real run on a Jetson
AGX Thor, and what it took to make the Thor fit for the job. 23–24 September
2026.*

---

The goal was simple to say: give Smithy one sentence — *build X* — walk away,
and come back to a branch of commits whose tests pass and a report that says
what happened. No babysitting, no "shall I proceed?".

The hard part is not getting a model to write code. It is knowing, the next
morning, whether the code is real. So everything we built starts from one
rule: **the compiler and the tests decide when work is done — not the model,
and not the judge model watching it.** A task is finished when the runner has
run its checks itself, the rest of the suite has not regressed, and no test
that existed before the run was weakened, skipped or deleted to get there.
Only then does it commit.

Around that rule sit the things an unattended agent needs. A planner turns
the intent into tasks, each with the exact test command that proves it. Git
belongs to the runner: the model may read history but cannot commit, reset or
push, so every commit on the branch is one the checks earned. Jev —
TypeSafe's fast "decision" model — watches for loops, asks whether an answer
is really finished, catches tests being quietly loosened, and stops the run
outright if an intent looks illegal or harmful. Research is checked too: every
page the model reads is saved, and every quote in its research notes is
matched mechanically against the page it came from.

**The first real run was humbling, usefully.** It was a toy: a parser for
ISO 8601 durations, maybe two hundred lines. It took hours and heated the
Thor. The post-mortem found ten causes. The first was ours — the run was
launched from a shell that could not see `cargo`, and an hour went by before
the failure said so. Others were design: the planner split a small library
into six tasks and attached a research question to every one, and each
question ran the full, adversarial research method for half an hour. And one
was the server: every request re-computed its whole ~52,000-token prompt,
because the model server had no prefix caching. 284 requests, 14.8 million
prompt tokens, none of them served from cache.

But the parts that mattered worked. The one task that finished was committed
only after the runner saw its build pass, its twelve tests pass, and the full
suite pass. Every judgment Jev made was right — including the call to stop and
ask a human when the toolchain was missing. And the research was genuinely
good: one note, with thirty findings each quoted verbatim from primary sources
and verified, caught a real bug in the code the previous task had committed.

**So we fixed the causes, not the symptoms.** A missing toolchain now fails
in a second. Research depth is now proportional to the question — a quick
lookup, a decision, or a deep investigation — within a budget that grows with
the run, so big projects get more research, not less. Jev now *reports* how
much it thinks a question needed outside sources, instead of silently
deciding whether to look. Every request, tool call and check is logged with
timings, and every conversation is kept, so the next post-mortem will not have
to be reconstructed from fragments.

**Then the machine.** Moving the Thor to a newer, community-tuned serving
recipe with prefix caching turned on cut a repeated 12,000-token request from
21.9 to 10.9 seconds, with 90% of the prompt served from cache. Along the way
we found the Thor downloading at 1.5 MB/s over a 2.4 GHz Wi-Fi link; one
cable to the router made it 79–100 MB/s. A day of model downloads became an
hour.

The model now serves at 25–42 tokens per second depending on the work, in
92 GiB of the Thor's 122, leaving room for speech, embeddings and a second,
research-specialised model — whose real cost to the first we will measure
rather than guess.

The next run is the same small intent, on the new server, with the new
planner and full logs. The bar: done in well under an hour, cool, with the
cache doing its job — and a report the next morning we can believe.

---
---

# Technical report

**Period:** 2026-09-23 17:36 UTC – 2026-09-24 ~20:30 UTC
**Repository:** `Smithy-Windows` (local; not pushed), 21 commits from
`736e89a` to `c992ab4`, +10,223 lines across 44 files, 535 tests passing
(smithy-agent 267, smithy-tools 153 + 7, smithy-run 57 + 14 end-to-end,
smithy-cli 9).
**Hardware:** NVIDIA Jetson AGX Thor, 128 GB unified memory (122.8 GiB
visible), JetPack 7.1 / L4T R38, 1 TB NVMe; Windows 11 laptop as the
Smithy host.
**Models:** Qwen3.8-Flash-Next (NVFP4) via vLLM on the Thor; Jev
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
  for heat. Post-mortem identified ten causes; eight fixed, two open.
- **Thor re-served** with a tuned vLLM recipe: prefix caching works (90% hit
  on a repeated prompt, 2.0× faster), 92 GiB total footprint, 25–42 tok/s.
- **Network:** Thor was on 2.4 GHz Wi-Fi (144 Mbit/s link). Wired: 79–100 MB/s.
- **Model inventory** downloaded for the next phase (~210 GB).

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
| 9 | One 57-minute turn before any check; scope creep into later tasks | Open |
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

### 6.3 Prefix caching

Two identical requests, 12,077-token prompt, 256 completion tokens:

| | Time | Cache hits (vLLM metrics) |
|---|---|---|
| First | 21.9 s | 0 |
| Second | 10.9 s | 10,816 of 12,077 (90%) |

The response `usage` still omits `prompt_tokens_details`; adding
`--enable-prompt-tokens-details` is needed for Smithy's report to show cache
rates.

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
benchmark; the hybrid (+13 GB disk, no extra memory) remains an option.
Peak temperature under benchmark: 48 °C.

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
Jev's 96.6% on a 49-task third-party decision benchmark; it is a candidate
offline fallback, to be measured on Smithy's own calibration suite.

## 8. Open items

1. **Cache reporting:** add `--enable-prompt-tokens-details` so Runs record
   cached tokens.
2. **Co-residency cost:** benchmark Flash-Next alone, then with the small
   models (≈17–20 GB), then with Apodex; decide what stays resident.
3. **Turn length (post-mortem #9):** shorter turns so checks run before a
   model polishes for an hour, and so work stays inside the current task.
4. **Smithy settings:** model name `qwen3.8-flash-next`.
5. **Fresh trial Run** of the same intent on the new server, new planner,
   full logs. Success: under an hour, cached tokens reported, most of the
   prompt served from cache.
6. **Thor OS updates** (10 pending, 4 security) — after downloads, with a
   reboot window.
7. **Von vs Jev** on the 43-case suite (endpoint made configurable).
8. **Research routing:** optionally send research Sessions to Apodex.
9. **Housekeeping:** remove 16 orphaned partial blobs; FP8 hybrid decision.

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
  paths (`/c/Users/…`) and `~`. Open.

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
```

## Appendix B. Thor state

| | |
|---|---|
| Addresses | one from the router by DHCP, plus the static address from the direct laptop link (kept) |
| SSH | key `~/.ssh/id_ed25519_thor` (laptop), alias `thor` |
| User | a regular account in the `docker` group |
| Rust | 1.98.1 (rustup, user-local) |
| Server | container `flash-next`, port 8000, detached |
| Start command | `~/thor-setup/start-flash-next.sh`: the recipe's `serving/start-qwen38-flash-next-fast.sh` with base revision `7b719225…`, port 8000, one slot, 256k, detached, `--enable-prompt-tokens-details`, and an 8 GiB KV cache (`FLASHNEXT_KV_GIB`). `FLASHNEXT_NO_RM=1` keeps a failed container's logs. Clear caches first if `free -g` shows memory held. |
| Caches | `~/thor-hf-cache` (HF home, token file), `~/thor-vllm-cache`, `~/thor-torch-cache`, `~/thor-flashinfer-cache` |
| Setup scripts and logs | `~/thor-setup/` |
| Monitor | `~/sysmon-tui/target/release/sysmon-tui` (`ssh -t thor …`) |
