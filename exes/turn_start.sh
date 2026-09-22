#!/usr/bin/env bash
# turn_start hook: every loop iteration, before the LLM request.
# in:  { agent, iteration, n_messages, queued_len, token_estimate }
# out: {"decision":"continue"} | {"decision":"prune_to","keep":N} | {"decision":"abort",...}
# Empty stdout means approval (Continue). Diagnostics go to stderr only.
set -euo pipefail
INPUT=$(cat)
echo "[turn_start] every loop iteration, before LLM request" >&2
if command -v jq >/dev/null 2>&1; then
  echo "$INPUT" | jq . >&2
else
  echo "$INPUT" >&2
fi
