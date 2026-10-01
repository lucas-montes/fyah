#!/usr/bin/env bash
# on_error hook: on LLM/tool errors.
# in:  { agent, phase, error }
# out: {"decision":"continue"} | {"decision":"abort",...}
# Empty stdout means approval (Continue). Diagnostics go to stderr only.
set -euo pipefail
INPUT=$(cat)
echo "[on_error] on LLM/tool errors" >&2
if command -v jq >/dev/null 2>&1; then
  echo "$INPUT" | jq . >&2
else
  echo "$INPUT" >&2
fi
