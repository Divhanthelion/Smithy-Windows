#!/usr/bin/env bash
# Start JevK5 (a local Jev: https://huggingface.co/alibiserikbay/JevK5) on port
# 8090, detached. Start Qwen first and wait until it is up: loading both at once
# makes them compete for memory. Setup (a Python venv, the weights, the source):
# thor/THOR.md, step 7.
#   start-jevk5.sh
#
# JEVK5_PYTHON  the venv's python      (default ~/jevk5-venv/bin/python)
# JEVK5_SRC     the jevk5 source tree  (default ~/src/jevk5, tag v0.3.3)
# JEVK5_REV     the weights' commit    (default c4f7fdb3…, JevK5 v0.3)
# HF_HOME       the Hugging Face cache (default ~/thor-hf-cache)
set -euo pipefail
python=${JEVK5_PYTHON:-$HOME/jevk5-venv/bin/python}
src=${JEVK5_SRC:-$HOME/src/jevk5}
rev=${JEVK5_REV:-c4f7fdb3aeab5582336406e78d3bef11bf98833d}
export HF_HOME=${HF_HOME:-$HOME/thor-hf-cache}
weights=$HF_HOME/hub/models--alibiserikbay--JevK5/snapshots/$rev
logs=${JEVK5_LOGS:-$HOME/thor-setup/logs}
if curl -sf -m 3 localhost:8090/health > /dev/null; then echo "JevK5 is already running"; exit 0; fi
[ -x "$python" ] || { echo "no $python (see THOR.md step 7)" >&2; exit 1; }
[ -d "$src/jevk5" ] || { echo "no JevK5 source in $src (see THOR.md step 7)" >&2; exit 1; }
[ -f "$weights/model.safetensors" ] || { echo "no JevK5 weights in $weights (see THOR.md step 7)" >&2; exit 1; }
mkdir -p "$logs"
cd ~
HF_HUB_OFFLINE=1 PYTHONPATH=$src setsid nohup "$python" -m jevk5.server \
  --model "$weights" --host 0.0.0.0 --port 8090 > "$logs/jevk5-serve.log" 2>&1 < /dev/null &
echo $! > "$logs/jevk5.pid"
for i in $(seq 1 30); do
  curl -sf -m 3 localhost:8090/health > /dev/null && { echo "JevK5 is up (port 8090; stop it with: kill \$(cat $logs/jevk5.pid))"; exit 0; }
  sleep 5
done
echo "JevK5 did not come up; see $logs/jevk5-serve.log" >&2; exit 1
