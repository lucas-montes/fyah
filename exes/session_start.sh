#!/usr/bin/env bash
# session_start hook: once per session, before the workflow runs.
# in:  { session_id, workflow, vcs_rev? }
# out: {"decision":"continue"} | {"decision":"abort","error":{"code":"aborted","scope":"session","detail":"..."}}
# Empty stdout means approval (Continue). Diagnostics go to stderr only.
set -euo pipefail
INPUT=$(cat)
echo "[session_start] once per session, before workflow runs" >&2
if command -v jq >/dev/null 2>&1; then
  echo "$INPUT" | jq . >&2
else
  echo "$INPUT" >&2
fi
