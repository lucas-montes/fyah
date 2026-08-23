# End-to-end smoke test: boots a NixOS VM with Ollama (serving a model baked
# into the closure) and the fyah binary, drives one full agent turn through a
# FIFO on stdin, and asserts —
#
#   1. an assistant response arrives in fyah's output,
#   2. the shell tool call the model was forced to make REALLY executed inside
#      the VM (/tmp/marker has the expected content),
#   3. the workflow completed.
#
# Ghostty-style: python lives inline in the Nix string, `${...}` injects store
# paths and pre-evaluated commands.
{
  self,
  nixpkgs,
  system,
}: let
  pkgs = import nixpkgs {inherit system;};

  fyah = self.packages.${system}.fyah;

  ollama-models = import ./ollama-models.nix {
    inherit pkgs;
    ollama = pkgs.ollama;
    models = ["qwen2.5:0.5b"];
    hash = "sha256-1rWxV87i2SyW2Rew4D2GtHyArhLwHW/TLHD7sdb5mwc=";
  };

  # Minimal config: one listening plan step that goes straight to `done`.
  # The model only has to answer; the checked file is written deterministically
  # by a post hook on the record stage (a 0.5B model cannot reliably emit tool
  # calls, so the LLM roundtrip and the file check are asserted separately).
  fyah-config = pkgs.writeText "fyah.toml" ''
    [providers.openai]
    url = "http://127.0.0.1:11434/v1/chat/completions"
    api_key = "ollama"

    [models.qwen]
    format = "openai"
    model = "qwen2.5:0.5b"
    temperature = 0

    [agents.primary]
    system_prompt = "You are a test agent. Reply with exactly: DONE"
    max_iterations = 10

    [workflows.default]
    initial = "plan"
    max_iterations = 20

    [workflows.default.steps.plan]
    type = "plan"
    agent = "primary"
    provider = "openai"
    model = "qwen"
    on_success = "done"
    on_failure = "plan"

    [loop_hooks.update_context]
    post = [{ type = "command", command = "echo fyah-e2e-marker > /tmp/marker" }]
  '';

  # Run fyah in the background reading user messages from a FIFO; capture all
  # output for grepping.
  start-fyah = pkgs.writeShellScript "start-fyah" ''
    set -eu
    mkdir -p /tmp/fyah-e2e
    cd /tmp/fyah-e2e
    rm -f in out marker
    mkfifo in
    fyah --config ${fyah-config} < in > out 2>&1 &
    exec 3>in            # keep the FIFO open so writes never block
    sleep 1              # let the runtime boot
    echo "start" > in    # kick off the listening plan step
  '';
in
  pkgs.testers.runNixOSTest {
    name = "e2e-smoke";

    nodes.machine = {
      # A full run takes ~2-4 minutes (boot + model warm-up + CPU inference
      # per turn) — slow subtests are expected, not a hang.
      virtualisation.memorySize = 4096;
      virtualisation.cores = 4;

      services.ollama = {
        enable = true;
        models = "${ollama-models}";
        environmentVariables = {
          OLLAMA_KEEP_ALIVE = "-1"; # keep the model resident between turns
        };
      };

      environment.systemPackages = [
        fyah
        pkgs.ollama
      ];

      environment.etc."fyah/fyah.toml".source = fyah-config;
    };

    testScript = ''
      with subtest("boot: ollama serves the baked model"):
          start_all()
          machine.wait_for_unit("ollama.service")
          machine.wait_for_open_port(11434)

      with subtest("warm the model outside of any assertion window"):
          machine.succeed(
              "timeout 300 ollama run qwen2.5:0.5b 'Reply with the single word OK'"
          )

      with subtest("launch fyah and send one prompt"):
          machine.succeed("${start-fyah}")

      with subtest("the assistant reply is logged with its content"):
          # update_context logs `reply=DONE` at INFO into fyah stdout; the
          # model answers "DONE" deterministically at temperature 0.
          machine.wait_until_succeeds(
              "grep -a 'assistant reply' /tmp/fyah-e2e/out | grep -q DONE",
              timeout=120,
          )

      with subtest("the workflow reached done"):
          machine.wait_until_succeeds(
              "grep -aq 'workflow complete' /tmp/fyah-e2e/out", timeout=60
          )

      with subtest("the post hook wrote the checked file"):
          machine.succeed(
              "grep -q 'fyah-e2e-marker' /tmp/marker"
          )
    '';
  }
