#!/usr/bin/env bash
# context hook: custom compaction (strategy = "custom").
# Runs after the deterministic policy, before pre_prompt.
# in:  { agent, iteration, n_messages, token_estimate, tool_output_bytes,
#         max_tokens?, max_messages?, max_output_bytes? }
# out: {"decision":"continue"}
#    | {"decision":"prune_to","keep":N}
#    | {"decision":"truncate_outputs","max_bytes":N}
#    | {"decision":"drop_tool_results","ids":[...]}
#    | {"decision":"abort","error":{"code":"aborted","scope":"turn","detail":"..."}}
# Empty stdout means approval (Continue). Diagnostics go to stderr only.
set -euo pipefail
INPUT=$(cat)
echo "[context] custom compaction after deterministic policy" >&2
if command -v jq >/dev/null 2>&1; then
  echo "$INPUT" | jq . >&2
else
  echo "$INPUT" >&2
fi
