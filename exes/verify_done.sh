#!/usr/bin/env bash
# verify_done hook: when the model makes no tool calls (claims to be done).
# in:  { agent, iteration, last_text?, n_messages }
# out: {"decision":"done"} | {"decision":"not_done","error":{"code":"check_failed","check":"...","detail":"..."}}
# Empty stdout means approval (Done). Diagnostics go to stderr only.
set -euo pipefail
INPUT=$(cat)
echo "[verify_done] model claims done (no tool calls), external checker decides" >&2
if command -v jq >/dev/null 2>&1; then
  echo "$INPUT" | jq . >&2
else
  echo "$INPUT" >&2
fi
