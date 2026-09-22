#!/usr/bin/env bash
# step before hook: around a single step, runs before the step action.
# in:  { workflow, step, session_id }
# out: {"decision":"continue"} | {"decision":"abort_step",...} | {"decision":"abort_workflow",...}
# Empty stdout means approval (Continue). Diagnostics go to stderr only.
set -euo pipefail
INPUT=$(cat)
echo "[step before] before a single step (agent = ... or command = ...)" >&2
if command -v jq >/dev/null 2>&1; then
  echo "$INPUT" | jq . >&2
else
  echo "$INPUT" >&2
fi
