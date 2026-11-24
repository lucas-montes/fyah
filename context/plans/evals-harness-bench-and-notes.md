# Plan: first two evals — Harness-Bench smoke + notes.md self-extension

> Status: planned. Date: 2026-11-23.

## 1. Change summary

Add two real-model evals that measure how models work *through* fyah:

1. **Harness-Bench smoke** — run fyah as a harness inside Qihoo360/harness-bench
   (arXiv 2605.27922) on a hand-picked ~20-task subset, using its own Python
   runner, Docker sandboxes and hidden oracles. Comparable to other harnesses
   on the same tasks and model.
2. **notes-ipify** — the self-extension task from `notes.md`: the model writes a
   JS script in `wasm/js`, compiles it to wasm, declares the executable + tool in
   `fyah.toml`, adds it to its own agent, the config hot-reloads, and it calls
   the tool. Runs in a NixOS VM test (pattern of `nix/tests.nix`).

Both need fyah to run unattended and emit machine-readable events, so that
lands first.

## 2. Success criteria

- `fyah --prompt-file P --once --events E` runs one task to completion with no
  stdin and exits with a code naming the stop reason.
- The events file has, per trial: every request (model, prompt / completion /
  cached tokens as reported by the provider, finish reason, latency), every tool
  call (name, ok or error kind, duration), every `Notice`, phase changes, and a
  final `run_end` with the stop reason.
- Harness-Bench: `evals/harness-bench/run.sh` runs the ~20 smoke tasks with
  `--harness fyah` against local Ollama and produces Harness-Bench's standard
  `result.json` per trial; the same subset also runs with one built-in harness
  (e.g. `opencode`) on the same model as a baseline.
- notes-ipify: a flake check runs one trial in a VM and the oracle reports
  pass/fail per check; `evals/notes-ipify/run.sh N` runs N trials and writes one
  result row each.
- Both evals append rows to `evals/results.jsonl` carrying fyah git revision,
  model, config hash, trial index, score and the metrics in section 2a.

### 2a. Metrics recorded per trial

- Outcome: score / pass, false completion (model ended, oracle failed).
- Cost: requests (turns), tool calls, input / output / cached tokens, cache hit
  rate (cached ÷ input), wall time.
- Harness use: tool errors by kind (`errors.rs` variants), repeated failing
  calls, recovery after error, answers given while calls were still running.
- Stop reason: done, max_turns, ToolCallLimit, ToolErrors, EmptyReply,
  RequestFailed, timeout.

## 3. Constraints and non-goals

- Code rules in `.claude/CLAUDE.md` apply to all Rust changes.
- No Python written or run by us on the host, except Harness-Bench's own
  runner (explicitly allowed) and the NixOS test driver's `testScript` (already
  how `nix/tests.nix` works). Our glue is bash or Rust.
- Models: local Ollama only for this iteration (`qwen2.5:14b`, `ministral`, …).
- Non-goals: full standard274 runs, multimodal suite, OpenRouter/Mistral runs,
  a Terminal-Bench adapter, a simulated user, scripted multi-turn evals,
  dashboards, fixing the cache-busting `"still running"` placeholder (the evals
  measure it; changing it is a later plan).
- Not repairing the stale `nix/tests.nix` e2e config here (see open questions).

## Assumptions

- Harness-Bench's provider wrapper accepts Ollama's OpenAI-compatible API root
  (`http://<host>:11434/v1`); T01 verifies this before anything depends on it.
- Ollama on the host may not report `cached_tokens`; then cache hit rate is
  recorded as unknown, not 0.
- The notes-ipify VM reaches the *host's* Ollama over QEMU user networking
  (`10.0.2.2:11434`) so a 14B model runs on the host GPU; everything else in the
  VM has no network.
- Inside the VM, `nix run .#wasm-tools` is replaced by a `wasm-tools` command on
  PATH wrapping the same Javy build; the eval prompt names that command.
- `api.ipify.org` is stubbed in the VM (`/etc/hosts` + a tiny local HTTP server
  answering a fixed IP) so the oracle is deterministic.

## 4. Task stack

- [ ] T01: `Spike the Harness-Bench harness contract` (status:todo)
  - Task ID: T01
  - Goal: Know exactly how a harness plugs into Harness-Bench and that Ollama works with its provider wrapper.
  - Boundaries (in/out of scope): In — read `tools/`, `docker/`, `docs/EVALUATION_AND_SCORING.md`, `docs/TASK_AUTHORING_AND_ACCEPTANCE.md`; how a built-in harness (opencode) gets the instruction, workspace path, model URL, and how completion/timeouts are detected; license; one opencode task run against Ollama. Out — any fyah code.
  - Done when: `context/evals/harness-bench.md` records the adapter contract (files, env vars, entrypoint, exit expectations), the Ollama verdict, the license, and the pinned harness-bench commit.
  - Verification notes (commands or checks): one `python tools/run_benchmark.py --harness opencode --base-url http://…:11434/v1 …` run on a single task produces a `result.json` with non-empty `provider_trace`.

- [ ] T02: `Headless run: --prompt-file and --once` (status:todo)
  - Task ID: T02
  - Goal: fyah takes its first user message from a file and exits when the run ends instead of idling on stdin.
  - Boundaries (in/out of scope): In — CLI flags in `main.rs`, the first `Inbound::Append` from the file, exiting on idle / limit notice / max_turns / request failure, distinct exit codes per stop reason. Out — event output (T03), interactive behaviour unchanged.
  - Done when: without the flags fyah behaves as today; with them it exits on its own with the documented code for each stop reason.
  - Verification notes (commands or checks): `cargo clippy --all-targets`, `cargo test`; a run against Ollama with a trivial prompt exits 0; a prompt forcing `max_tool_calls` exits with the limit code.

- [ ] T03: `Event stream: --events JSONL` (status:todo)
  - Task ID: T03
  - Goal: Every trial leaves a machine-readable record of what the harness and model did.
  - Boundaries (in/out of scope): In — a new `tracing` target next to `agent_trace` written to the `--events` path; events `request`, `tool_call`, `tool_result`, `notice`, `phase`, `run_end`; parsing `usage` (incl. `prompt_tokens_details.cached_tokens`) in the OpenAI provider, and its equivalent in `providers/anthropic.rs`. Out — summarising or scoring (T08).
  - Done when: a run's events file holds one line per event with the fields listed in section 2, and unknown usage fields are absent rather than zero.
  - Verification notes (commands or checks): unit tests for usage parsing and event serialisation; `jq -c . events.jsonl` on a real run; counts of `request` lines equal the provider calls seen in the app log.

- [ ] T04: `Harness-Bench fyah adapter image` (status:todo)
  - Task ID: T04
  - Goal: Harness-Bench can run fyah as `--harness fyah`.
  - Boundaries (in/out of scope): In — `evals/harness-bench/fyah/` holding the Dockerfile, the entrypoint (bash) that writes a `fyah.toml` from Harness-Bench's env (base URL, model, workspace dir as `dirs`, builtins `read`/`write`/`bash`) and runs `fyah --prompt-file … --once --events …`, and whatever registration T01 found is needed in a pinned harness-bench checkout (vendored patch or fork, decided in T01). Out — task selection, baseline runs.
  - Done when: one Harness-Bench task runs end to end with `--harness fyah` and yields `result.json`; the fyah events file is kept next to it.
  - Verification notes (commands or checks): single-task `run_benchmark.py --harness fyah` run; `result.json` has a score and `provider_trace.provider_transactions` equal to the `request` count in fyah's events.

- [ ] T05: `Harness-Bench smoke subset and runner` (status:todo)
  - Task ID: T05
  - Goal: One command runs the ~20-task smoke subset for fyah and the baseline harness.
  - Boundaries (in/out of scope): In — `evals/harness-bench/smoke.txt` (task IDs, mostly Software and Systems Engineering, each with a one-line reason), `evals/harness-bench/run.sh` taking harness, model, trials; run roots under `.fyah/evals/`. Out — summarising (T08).
  - Done when: `run.sh fyah qwen2.5:14b 1` and `run.sh opencode qwen2.5:14b 1` complete over all smoke tasks.
  - Verification notes (commands or checks): both runs leave one `result.json` per task; timeouts and failures are counted, not dropped.

- [ ] T06: `notes-ipify fixture` (status:todo)
  - Task ID: T06
  - Goal: A reproducible starting repo state and prompt for the self-extension task.
  - Boundaries (in/out of scope): In — `evals/notes-ipify/prompt.md` (the notes.md task, naming the in-VM `wasm-tools` command), `evals/notes-ipify/fyah.toml` (current schema, Ollama at `10.0.2.2`, agent with `read`/`write`/`bash`, no `ip_info`), the fixture tree built from the repo minus `wasm/js/ip_info.js` and any `ip_info` config, the ipify stub. Out — the VM itself.
  - Done when: the fixture config passes fyah's validation and contains no `ip_info`; the prompt is self-contained.
  - Verification notes (commands or checks): `fyah --config evals/notes-ipify/fyah.toml` starts without a config error; `grep -r ip_info` over the fixture finds nothing.

- [ ] T07: `notes-ipify NixOS VM check and oracle` (status:todo)
  - Task ID: T07
  - Goal: One VM run executes the task and grades it on final state.
  - Boundaries (in/out of scope): In — `nix/evals/notes-ipify.nix` wired as a flake check; VM with fyah, Javy + `wasm-tools` wrapper, the ipify stub, the fixture copied to a writable dir; an oracle script (bash) checking: (1) a new `wasm/js/*.js` exists, (2) its `.wasm` built, (3) `fyah.toml` declares a matching `[executables.*]` (type wasm) and `[tools.*]`, (4) the agent's `tools` list names it, (5) the config still validates, (6) events show a successful call of that tool after a reload, (7) the final AI text contains the stub IP. Each check reported separately. Out — multi-trial looping (T08).
  - Done when: `nix build .#checks.x86_64-linux.eval-notes-ipify` runs a trial and prints per-check results; the derivation keeps the events file and oracle output as outputs.
  - Verification notes (commands or checks): the check passes when the oracle is pointed at a hand-made solved fixture and fails, naming the check, on the untouched fixture.

- [ ] T08: `Trials and results rows` (status:todo)
  - Task ID: T08
  - Goal: Both evals produce comparable rows across trials, models and fyah revisions.
  - Boundaries (in/out of scope): In — `evals/summarize.sh` (bash + jq) turning one trial's events + score into a row with the section 2a metrics, appended to `evals/results.jsonl`; `evals/notes-ipify/run.sh N` looping VM trials (`--rebuild` or a trial-index argument so each run is fresh); fyah revision, model and config hash in every row. Out — charts, statistics beyond per-cell mean and spread.
  - Done when: 3 trials of notes-ipify and 1 Harness-Bench smoke run each produce rows with every metric present or explicitly unknown.
  - Verification notes (commands or checks): `jq -s 'group_by(.eval,.model)|map({eval:.[0].eval,model:.[0].model,n:length,pass:(map(.pass)|add/length)})' evals/results.jsonl`.

- [ ] T09: `Validation and cleanup` (status:todo)
  - Task ID: T09
  - Goal: Everything builds, runs and is reflected in context.
  - Boundaries (in/out of scope): In — full checks, removing scratch files, `context/evals/` current-state docs (how to run each eval, metrics meanings, known limits), plan status updates. Out — new features.
  - Done when: all checks below pass, context matches code, no drift.
  - Verification notes (commands or checks): `cargo clippy --all-targets`, `cargo test`, `nix flake check` (incl. `eval-notes-ipify`), one smoke `run.sh fyah` run, `evals/results.jsonl` has rows from both evals.

## 5. Open questions

- Should the stale `nix/tests.nix` e2e (old `[workflows]` / `[models.*]` schema) be repaired before T07, since T07 copies its pattern?
- Trials per cell: 3 as the starting default — enough for qwen2.5:14b on the host GPU, or do you want 5+?
- Where should the harness-bench checkout live: a git submodule under `evals/harness-bench/upstream`, or a path outside the repo given by an env var?
- The OpenRouter key committed in `fyah.toml` should be revoked and moved to an env var; separate from this plan but blocks any later OpenRouter runs.
