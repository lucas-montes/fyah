#!/usr/bin/env bash
# session_end hook: once per session, after the workflow finishes (audit only).
# in:  { session_id, workflow, success, n_messages }
# out: {"decision":"continue"}
# Empty stdout means approval (Continue). Diagnostics go to stderr only.
set -euo pipefail
INPUT=$(cat)
echo "[session_end] once per session, after workflow finishes (audit only)" >&2
if command -v jq >/dev/null 2>&1; then
  echo "$INPUT" | jq . >&2
else
  echo "$INPUT" >&2
fi
