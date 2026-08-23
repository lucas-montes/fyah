# A store closure containing Ollama models pulled from registry.ollama.ai.
#
# Runs `ollama pull` inside a fixed-output derivation: the first build needs
# network and fails with the real output hash, which is then pinned in `hash`
# — after that the models directory is served from the store like any other
# artifact and everything consuming it is hermetic. Point `OLLAMA_MODELS` at
# the result path (read-only serving works).
#
# Note: a floating tag (e.g. `qwen2.5:0.5b`) resolves to immutable blobs but
# its manifest can be repointed upstream; if a pull ever mismatches the pin,
# refresh `hash`.
{
  pkgs,
  ollama,
  models ? [ "qwen2.5:0.5b" ],
  hash ? "",
}:
let
  lib = pkgs.lib;
  slug = lib.concatStringsSep "-" (
    map (model: builtins.replaceStrings [ ":" "/" ] [ "-" "-" ] model) models
  );
in
pkgs.runCommand "ollama-models-${slug}"
  {
    nativeBuildInputs = [ ollama ];
    outputHashAlgo = "sha256";
    outputHashMode = "recursive";
    outputHash = hash;

    # FODs get network access by design; honor standard proxy variables.
    impureEnvVars = lib.fetchers.proxyImpureEnvVars;
  }
  ''
    export HOME="$TMPDIR"
    export OLLAMA_MODELS="$out"
    mkdir -p "$out"

    # Recent ollama CLIs delegate `pull` to a server process, so start one
    # locally for the duration of the build.
    export OLLAMA_HOST="127.0.0.1:11500"
    ollama serve &
    server_pid=$!
    for _ in $(seq 120); do
      if (exec 3<>/dev/tcp/127.0.0.1/11500) 2>/dev/null; then
        break
      fi
      sleep 0.5
    done

    ${lib.concatStringsSep "\n" (
      map (model: "ollama pull ${lib.escapeShellArg model}") models
    )}

    kill "$server_pid"

    echo "-- pulled models --"
    find "$out" -type f | sort
  ''
