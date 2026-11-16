{
  description = "Fyah your harness";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-26.05";
    rust-overlay.url = "github:oxalica/rust-overlay";
    rust-overlay.inputs.nixpkgs.follows = "nixpkgs";
    crane.url = "github:ipetkov/crane";
    flake-utils.url = "github:numtide/flake-utils";
    sce.url = "github:crocoder-dev/shared-context-engineering";
  };

  outputs = {
    self,
    nixpkgs,
    crane,
    rust-overlay,
    flake-utils,
    sce,
    ...
  }:
    flake-utils.lib.eachDefaultSystem (
      system: let
        lib = nixpkgs.lib;
        pkgs = import nixpkgs {
          inherit system;
          overlays = [rust-overlay.overlays.default];
        };

        rust = pkgs.rust-bin.stable.latest.default.override {
          extensions = [
            "rust-src"
          ];
        };

        # Crane builds the dependency closure as its own derivation keyed by
        # Cargo.lock/Cargo.toml, so source edits only rebuild the final crate.
        craneLib = (crane.mkLib pkgs).overrideToolchain (_: rust);

        fyahSrc = lib.cleanSourceWith {
          src = craneLib.path ./.;
        };

        commonArgs = {
          pname = "fyah";
          version = (fromTOML (builtins.readFile ./Cargo.toml)).package.version;
          src = fyahSrc;
        };

        # Model served by the dev app and baked into the e2e VM. Flip to
        # [ "qwen2.5:1.5b" ] for the bigger instruct model (pin its hash in
        # nix/ollama-models.nix after the first build).
        ollamaModels = ["qwen2.5:0.5b"];
        rocmEnabled = system == "x86_64-linux";
        ollamaPkg =
          if rocmEnabled
          then pkgs.ollama-rocm
          else pkgs.ollama;
        ollamaEnv = lib.optionalString rocmEnabled ''
          export OLLAMA_IGPU_ENABLE="''${OLLAMA_IGPU_ENABLE:-1}"
        '';

        ollamaModelsDir = import ./nix/ollama-models.nix {
          inherit pkgs;
          ollama = pkgs.ollama;
          models = ollamaModels;
        };

        fyahDeps = craneLib.buildDepsOnly commonArgs;

        fyah = craneLib.buildPackage (commonArgs // {cargoArtifacts = fyahDeps;});

        # Javy ships prebuilt for x86_64-linux only here; see nix/wasm-tools.nix.
        wasmEnabled = system == "x86_64-linux";
        wasmTools = import ./nix/wasm-tools.nix {inherit pkgs;};
      in {
        packages =
          {
            inherit fyah;
            default = fyah;
          }
          // lib.optionalAttrs rocmEnabled {
            ollama-rocm = pkgs.ollama-rocm;
          }
          // lib.optionalAttrs wasmEnabled {
            inherit (wasmTools) javy python-wasi;
            wasm-tools = wasmTools.wasm-tools;
          };

        checks = {
          e2e-smoke = import ./nix/tests.nix {
            inherit self nixpkgs system ollamaModels;
          };

          # Independently cached lint/test runs over the same dependency
          # artifact.
          fyah-clippy = craneLib.cargoClippy (commonArgs
            // {
              cargoArtifacts = fyahDeps;
              cargoClippyExtraArgs = "--all-targets";
            });

          fyah-test = craneLib.cargoTest (commonArgs
            // {
              cargoArtifacts = fyahDeps;
            });
        };

        devShells.default = pkgs.mkShell {
          name = "fyah";
          buildInputs =
            [
              sce.packages.${system}.default
              rust
              ollamaPkg
              pkgs.bazel
              pkgs.buildifier
              pkgs.python3
            ]
            ++ lib.optionals rocmEnabled [
              pkgs.rocmPackages.rocminfo
              pkgs.amdgpu_top
            ]
            # To build and try JS tools by hand.
            ++ lib.optionals wasmEnabled [
              wasmTools.javy
            ];
          shellHook = ollamaEnv;
          RUSTFLAGS = "--cfg tracing_unstable";
        };

        apps =
          lib.optionalAttrs wasmEnabled {
            # Build the wasm examples and link them to wasm/dist, where
            # fyah.toml expects them. Run from the repository root.
            wasm-tools = wasmTools.app;
          }
          // {
          # Launch ollama serving the same baked model the e2e test uses.
          ollama-dev = {
            type = "app";
            program = "${
              pkgs.writeShellScript "ollama-dev" ''
                ${ollamaEnv}
                export OLLAMA_MODELS="${ollamaModelsDir}"
                exec ${ollamaPkg}/bin/ollama serve
              ''
            }";
          };

          generate-openapi = {
            type = "app";
            program = "${pkgs.writeShellScript "generate-openapi" ''
              ${pkgs.openapi-generator-cli}/bin/openapi-generator-cli generate \
                -i https://raw.githubusercontent.com/openai/openai-openapi/refs/heads/master/openapi.yaml \
                -g rust \
                -o generated/openapi \
                --skip-validate-spec \
                --additional-properties=packageName=a2a_openapi
            ''}";
          };
        };
      }
    );
}
