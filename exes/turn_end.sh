#!/usr/bin/env bash
# turn_end hook: after the model responds, before tool execution.
# in:  { agent, iteration, n_tool_calls, has_text }
# out: {"decision":"continue"} | {"decision":"drop_turn"}
# Empty stdout means approval (Continue). Diagnostics go to stderr only.
set -euo pipefail
INPUT=$(cat)
echo "[turn_end] after model responds, before tool execution" >&2
if command -v jq >/dev/null 2>&1; then
  echo "$INPUT" | jq . >&2
else
  echo "$INPUT" >&2
fi
