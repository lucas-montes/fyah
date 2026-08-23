{
  description = "Fyah your harness";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-25.11";
    rust-overlay.url = "github:oxalica/rust-overlay";
    rust-overlay.inputs.nixpkgs.follows = "nixpkgs";
    crane.url = "github:ipetkov/crane";
    flake-utils.url = "github:numtide/flake-utils";
    sce.url = "github:crocoder-dev/shared-context-engineering";
  };

  outputs =
    {
      self,
      nixpkgs,
      crane,
      rust-overlay,
      flake-utils,
      sce,
      ...
    }:
    flake-utils.lib.eachDefaultSystem (
      system:
      let
        lib = nixpkgs.lib;
        pkgs = import nixpkgs {
          inherit system;
          overlays = [ rust-overlay.overlays.default ];
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
          version = (builtins.fromTOML (builtins.readFile ./Cargo.toml)).package.version;
          src = fyahSrc;
        };
      in
      {
        packages =
          let
            fyahDeps = craneLib.buildDepsOnly commonArgs;
            fyah = craneLib.buildPackage (commonArgs // { cargoArtifacts = fyahDeps; });
          in
          {
            inherit fyah;
            default = fyah;
          };

        checks.e2e-smoke = import ./nix/tests.nix {
          inherit self nixpkgs system;
        };

        devShells.default = pkgs.mkShell {
          name = "fyah";
          buildInputs = [
            sce.packages.${system}.default
            rust
            pkgs.ollama
            pkgs.bazel
            pkgs.buildifier
          ];
        };

        apps = {
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
