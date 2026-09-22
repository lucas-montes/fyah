#!/usr/bin/env bash
# pre_prompt hook: before the LLM request (context injection point).
# in:  { agent, n_messages }
# out: {"decision":"continue"} | {"decision":"abort",...}
# NOTE: accepted by config but not executed by the loop yet.
# Empty stdout means approval (Continue). Diagnostics go to stderr only.
set -euo pipefail
INPUT=$(cat)
echo "[pre_prompt] before LLM request (context injection point)" >&2
if command -v jq >/dev/null 2>&1; then
  echo "$INPUT" | jq . >&2
else
  echo "$INPUT" >&2
fi
