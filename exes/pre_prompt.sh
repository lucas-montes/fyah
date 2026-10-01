#!/usr/bin/env bash
# pre_prompt hook: before the LLM request.
# in:  { agent, iteration, n_messages }
# out: {"decision":"continue"}
#    | {"decision":"inject","mode":"ephemeral"|"persistent","content":"..."}
#    | {"decision":"abort",...}
# ephemeral = this call only, never persisted; persistent = appended as a
# User message (auditable, billed). Empty stdout means Continue.
# Diagnostics go to stderr only.
set -euo pipefail
INPUT=$(cat)
echo "[pre_prompt] before LLM request" >&2
if command -v jq >/dev/null 2>&1; then
  echo "$INPUT" | jq . >&2
else
  echo "$INPUT" >&2
fi
# Example injection (uncomment to enable):
# echo '{"decision":"inject","mode":"ephemeral","content":"Prefer short answers."}'
