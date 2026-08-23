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
  hashOverride ? null,
}:
let
  lib = pkgs.lib;

  # Output hash per known model set. Flip model tags freely; a set without an
  # entry builds unchecked once (empty hash ⇒ network build) and Nix prints
  # the real hash — paste it here to re-pin.
  pinnedHashes = {
    "qwen2.5:0.5b" = "sha256-1rWxV87i2SyW2Rew4D2GtHyArhLwHW/TLHD7sdb5mwc=";
    # "qwen2.5:1.5b" = "";
  };

  key = lib.concatStringsSep "," models;
  slug = lib.concatStringsSep "-" (
    map (model: builtins.replaceStrings [ ":" "/" ] [ "-" "-" ] model) models
  );

  hash =
    if hashOverride != null then
      hashOverride
    else if pinnedHashes ? ${key} then
      pinnedHashes.${key}
    else
      ""; # unchecked first build: Nix reports the hash to pin above
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
