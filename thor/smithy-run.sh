#!/usr/bin/env bash
# Start a Smithy Run on the Thor, detached from any terminal or SSH session.
#   smithy-run.sh PROJECT_DIR [smithy-agent run arguments...]
# e.g.
#   smithy-run.sh ~/code/word-counter "a CLI that counts words, with tests" --slots 3 --hours 3
#   smithy-run.sh ~/code/word-counter --resume --slots 3
#
# Keys come from ~/.config/smithy/keys.env (mode 600), as `export NAME=value`
# lines: AI_GATEWAY_API_KEY for the hosted Jev, BRAVE_API_KEY for web search.
# Jev: JevK5 on this machine (port 8090, start-jevk5.sh) when it is running,
# otherwise the hosted Jev. Set JEV_ENDPOINT yourself to override.
# Progress goes to ~/thor-setup/runs/ (SMITHY_RUN_LOGS).
set -euo pipefail
project=$1; shift
smithy=${SMITHY_DIR:-$HOME/Smithy-Windows}/target/release/smithy-agent
logs=${SMITHY_RUN_LOGS:-$HOME/thor-setup/runs}
[ -x "$smithy" ] || { echo "no $smithy: build it with  cargo build --release --bin smithy-agent" >&2; exit 1; }
[ -f ~/.config/smithy/keys.env ] && . ~/.config/smithy/keys.env
if [ -z "${JEV_ENDPOINT:-}" ] && curl -sf -m 3 localhost:8090/health > /dev/null; then
  export JEV_ENDPOINT=http://localhost:8090/v1/systemone JEV_MODEL=jevk5
fi
if [ -n "${JEV_ENDPOINT:-}" ]; then
  echo "Jev: $JEV_ENDPOINT"
elif [ -n "${AI_GATEWAY_API_KEY:-}" ]; then
  echo "Jev: hosted (AI Gateway)"
else
  echo "no Jev: start JevK5 (start-jevk5.sh) or set AI_GATEWAY_API_KEY; without Jev the guardrail cannot run, so nothing would be built" >&2
  exit 1
fi
export PATH="$HOME/.cargo/bin:$PATH"
mkdir -p "$logs"
log=$logs/$(basename "$project")-$(date -u +%Y%m%d-%H%M).log
cd "$project"
setsid nohup "$smithy" run "$@" --project . > "$log" 2>&1 < /dev/null &
echo "started (pid $!); progress: $log"
