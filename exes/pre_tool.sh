#!/usr/bin/env bash
# pre_tool hook: before a single tool call.
# in:  { agent, call_id, tool, args }
# out: {"decision":"continue"} | {"decision":"deny","error":{"code":"denied","rule":"...","detail":"..."}}
# Empty stdout means approval (Continue). Diagnostics go to stderr only.
set -euo pipefail
INPUT=$(cat)
echo "[pre_tool] before a single tool call" >&2
if command -v jq >/dev/null 2>&1; then
  echo "$INPUT" | jq . >&2
else
  echo "$INPUT" >&2
fi
