#!/usr/bin/env bash
# loop_guard hook: custom continuation logic for [agents.<name>.limits].
# Runs first each iteration, before the context policy.
# in:  { agent, iteration, n_messages, token_estimate, total_tokens_est,
#         tool_calls_total, elapsed_secs, consecutive_errors, stalled_turns }
# out: {"decision":"continue"} | {"decision":"stop","reason":"..."}
# Empty stdout means approval (Continue). Diagnostics go to stderr only.
set -euo pipefail
INPUT=$(cat)
echo "[loop_guard] custom continuation check" >&2
if command -v jq >/dev/null 2>&1; then
  echo "$INPUT" | jq . >&2
else
  echo "$INPUT" >&2
fi
