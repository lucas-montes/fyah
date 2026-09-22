#!/usr/bin/env bash
# post_tool hook: after a single tool result.
# in:  { agent, call_id, tool, output_len }
# out: {"decision":"continue"} | {"decision":"drop_result"}
# NOTE: accepted by config but not executed by the loop yet.
# Empty stdout means approval (Continue). Diagnostics go to stderr only.
set -euo pipefail
INPUT=$(cat)
echo "[post_tool] after a single tool result" >&2
if command -v jq >/dev/null 2>&1; then
  echo "$INPUT" | jq . >&2
else
  echo "$INPUT" >&2
fi
