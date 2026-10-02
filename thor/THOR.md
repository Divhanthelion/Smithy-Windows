# Smithy on a Jetson AGX Thor

How to set up a Jetson AGX Thor the way this fork's unattended Runs were
developed and measured: Qwen3.8-Flash-Next served locally, Smithy's
`smithy-agent` running on the Thor itself, and Jev either hosted or local
(JevK5). One sentence in; a branch of checked commits and a report out, with
nothing sent anywhere but web searches and, if you choose it, the hosted Jev.

What to expect, from the
[field report](../reports/2026-09-24-unattended-runs-on-the-thor.md): 28–47
tokens a second from one stream, about 60 tokens a second across three; the
same small Rust library built in 53 minutes, and a real feature pair on a
real project in about seven hours of running.

The files in this folder:

| File | What it does |
|---|---|
| `start-flash-next.sh` | starts Qwen3.8-Flash-Next (adapted from Marco Pastorio's NemoClaw-Thor recipe, MIT) |
| `start-jevk5.sh` | starts JevK5, a local Jev, beside it |
| `smithy-run.sh` | starts a Run detached, with keys and the right Jev |
| `bench.py` | measures the server: time to first token, decode speed, cache hits |

Every command below is typed **on the Thor** (over `ssh` or at its own
keyboard). Allow an afternoon: most of it is downloading and building.

## What you need

- A Jetson AGX Thor (128 GB) on **JetPack 7.1** (L4T R38.4, Ubuntu 24.04).
  Tested with Docker 29.
- About **200 GB free disk**: 139 GB for the checkpoint (base plus FP8
  hybrid), 21 GB for the container image, 8.4 GB for JevK5, plus caches.
- An internet connection for the downloads. Wired is much faster; on 2.4 GHz
  Wi-Fi we measured 1.5–11 MB/s.
- A regular user account in the `docker` group. Two steps need `sudo`
  (power mode, and clearing memory after a crash).

## 1. Power mode: 90 W

At MAXN, sustained inference tripped the Thor's over-current counter hundreds
of times an hour; at 90 W, three hours of generation tripped it zero times,
for a small loss of speed.

```bash
sudo nvpmodel -m 2
nvpmodel -q          # should say: NV Power Mode: 90W
```

## 2. Serve Qwen3.8-Flash-Next

The model server is Marco Pastorio's recipe,
[NemoClaw-Thor](https://github.com/pastoriomarco/NemoClaw-Thor): a vLLM image
built for the Thor, the NVFP4 checkpoint, and an FP8 conversion of its dense
side weights (+19–27% decode, −2.7 GiB). Follow its
[recommended Thor recipe](https://github.com/pastoriomarco/NemoClaw-Thor/blob/main/serving/docs/QWEN38-FLASH-NEXT-FAST-THOR.md)
through its first three steps, from a clone in your home directory:

```bash
git clone https://github.com/pastoriomarco/NemoClaw-Thor ~/NemoClaw-Thor
cd ~/NemoClaw-Thor
git checkout dd07dcc     # the commit this guide was written against
./serving/build-qwen38-flash-next-fast.sh           # builds the image
./serving/download-qwen38-flash-next.sh             # the checkpoint, ~135 GB
./serving/prepare-qwen38-flash-next-fp8-hybrid.sh   # the FP8 hybrid
```

Then start it with **this repository's** script rather than the recipe's own
(clone Smithy first, step 3, if you haven't):

```bash
free -g                                  # "used" should be a few GB
~/Smithy-Windows/thor/start-flash-next.sh
```

It returns at once and loads for about ten minutes. It is ready when

```bash
curl -s localhost:8000/health && echo READY
```

prints `READY`. What this script changes from the recipe's, and why:

- **A fixed KV cache** (8 GiB, about 290k tokens; `FLASHNEXT_KV_GIB`). The
  Thor counts page cache as used memory, so after a fresh boot the recipe's
  fraction-based budget came out at −0.71 GiB and the server would not start.
- **Port 8000, three request slots, 256k context, detached**, container name
  `flash-next`.
- **`--enable-prompt-tokens-details`**, so each response reports how much of
  its prompt came from the prefix cache. Smithy's Run logs record it.

**If it will not start** and `free -g` shows memory "used" with no server
running: the Thor does not release a crashed GPU process's memory. Clear it
with `sudo sync && echo 3 | sudo tee /proc/sys/vm/drop_caches`, then start
again. If the name `flash-next` is taken by a stopped container, pass another:
`FLASHNEXT_CONTAINER=flash-next-2 ~/Smithy-Windows/thor/start-flash-next.sh`.

To measure it: `python3 ~/Smithy-Windows/thor/bench.py mylabel` (one stream)
or `--streams 3`. Ours, single stream with a 16k shared prefix: 42.9 / 28.7 /
47.1 tok/s on code / prose / JSON, first token in about 1.25 s.

## 3. Build Smithy's agent

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh     # if Rust is not installed
source ~/.cargo/env
rustup component add clippy          # a Run's lint check needs it
git clone https://github.com/Divhanthelion/Smithy-Windows ~/Smithy-Windows
cd ~/Smithy-Windows
cargo build --release --bin smithy-agent
```

This builds only the command-line agent, which is all a Run needs; the editor
window is not used on the Thor.

## 4. Point Smithy at Qwen

Create `~/.local/share/smithy/provider.json` exactly like this. All three
sections must be there: a file missing one fails to parse, and Smithy then
quietly uses its defaults (LM Studio on port 1234) instead.

```bash
mkdir -p ~/.local/share/smithy
cat > ~/.local/share/smithy/provider.json <<'EOF'
{
  "provider": "lmstudio",
  "lmstudio": { "base_url": "http://localhost:8000/v1", "model": "qwen3.8-flash-next" },
  "openrouter": { "base_url": "https://openrouter.ai/api/v1", "model": "anthropic/claude-opus-5.5" },
  "deepseek": { "base_url": "https://api.deepseek.com", "model": "deepseek-v4-flash" }
}
EOF
```

(`lmstudio` is Smithy's backend for any OpenAI-compatible server, vLLM
included. The other two sections are only kept for switching backends.) The
first line of every Run's log names the model it is using: it should say
`model qwen3.8-flash-next · vllm`. `qwen3.6-27b` there means the file was not
read.

## 5. Keys

Create `~/.config/smithy/keys.env`, readable only by you:

```bash
mkdir -p ~/.config/smithy
cat > ~/.config/smithy/keys.env <<'EOF'
export BRAVE_API_KEY=...          # web search for research; leave out to Run without it
export AI_GATEWAY_API_KEY=...     # only for the hosted Jev (step 6)
EOF
chmod 600 ~/.config/smithy/keys.env
```

Both are optional. Without a Brave key, Runs cannot search the web. Without a
gateway key you need JevK5 (step 7).

## 6. Choose a Jev

A Run will not build anything without Jev: it asks Jev first whether the
intent should be built at all, then watches for loops, false "done"s and
weakened tests. Two choices:

| | Hosted Jev | JevK5 on the Thor |
|---|---|---|
| Needs | a Vercel AI Gateway key **with paid credits** (the free tier is refused) | 14 GB of memory beside Qwen, and step 7 |
| Smithy's 63-case suite | 1 wrong (a rate-limit error) | 7 wrong |
| Where it is weaker | — | calls loops later, and picks "hand off" where "research" or "compact" was better; never missed a destructive command, a weakened test or unfinished work |
| Speed | network round trip | ~0.2 s a question, ~0.35 s while Qwen is busy |

`smithy-run.sh` picks for you: JevK5 whenever it is running on port 8090,
otherwise the hosted Jev.

## 7. JevK5 (skip if you use the hosted Jev)

> **Not yet tested on a fresh Thor.** On ours, JevK5 runs in a Python
> environment built for another model. The steps below install the same
> packages at the same versions into a clean one: plain PyPI wheels, whose
> `torch` is a CUDA 13 build that lists the Thor's `sm_110`. Tell us if they
> don't work as written.

```bash
sudo apt install -y python3-venv                       # if missing
python3 -m venv ~/jevk5-venv
~/jevk5-venv/bin/pip install --upgrade pip
~/jevk5-venv/bin/pip install torch==2.14.0 transformers==5.17.0 accelerate==1.15.0 \
  huggingface_hub==1.33.0 safetensors==0.8.0 numpy jinja2
~/jevk5-venv/bin/python -c "import torch; print(torch.cuda.is_available(), torch.cuda.get_arch_list())"
#   expect: True [... 'sm_110' ...]

# the source (pure Python, no install)
git clone --depth 1 --branch v0.3.3 https://github.com/allebee/jevk5 ~/src/jevk5

# the weights, 8.4 GB, pinned to JevK5 v0.3
HF_HOME=~/thor-hf-cache ~/jevk5-venv/bin/hf download alibiserikbay/JevK5 \
  --revision c4f7fdb3aeab5582336406e78d3bef11bf98833d
cd ~/thor-hf-cache/hub/models--alibiserikbay--JevK5/snapshots/c4f7fdb3*/ && sha256sum -c SHA256SUMS
```

Then, **after Qwen is up** (loading both at once makes them compete for
memory):

```bash
~/Smithy-Windows/thor/start-jevk5.sh
```

It says `JevK5 is up` in about a minute. Its linear-attention layers use
transformers' plain PyTorch code (the fast kernels are optional and not
installed): same answers, a little slower on long inputs.

## 8. Your first Run

Tell git who you are (once):

```bash
git config --global user.name "Your Name"
git config --global user.email "you@example.com"
```

A Run needs a project that is a git repository with at least one commit and
nothing uncommitted. For a new Rust project:

```bash
cargo new ~/code/word-counter
cd ~/code/word-counter
git add -A && git commit -m "Empty project" && git branch -M main
```

Start it, in one sentence, with three hours as the ceiling:

```bash
~/Smithy-Windows/thor/smithy-run.sh ~/code/word-counter \
  "a CLI that counts words, lines and characters in files, like wc, with tests" \
  --slots 3 --hours 3
```

The first line says which Jev it will use. The Run carries on after you log
out. Watch it with `tail -f ~/thor-setup/runs/word-counter-*.log` (Ctrl+C
stops watching, not the Run). When the log ends with `done`, `blocked` or
`stopped`, read `.smithy/runs/*/REPORT.md` in the project: the new code is
on the branch the Run left checked out, `smithy/run-…`.

Writing the sentence: say *what*, not *how*; say "with tests"; keep a first
one small; nothing that needs a GUI, a password or a service on the internet.
Rust, CMake and Python (pytest) projects are recognised; for anything else,
declare the checks in `.smithy/checks.toml`.

- **Carry on** after a stop, a crash or a reboot (start Qwen, and JevK5 if
  you use it, first): `smithy-run.sh ~/code/word-counter --resume --slots 3`.
  `--allow T2` gives a blocked task fresh attempts.
- **Stop** a Run: `pkill -f "smithy-agent run"`. Finished tasks stay
  committed.

## 9. Shutting down

Once no Run is going:

```bash
docker stop flash-next                          # or whatever name you started it with
kill $(cat ~/thor-setup/logs/jevk5.pid)         # if JevK5 is running
```

Next time: steps 2 (start and wait), 7 (start JevK5), 8.
