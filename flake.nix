{
  description = "Fyah your harness";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-25.11";
    rust-overlay.url = "github:oxalica/rust-overlay";
    rust-overlay.inputs.nixpkgs.follows = "nixpkgs";
    flake-utils.url = "github:numtide/flake-utils";
    sce.url = "github:crocoder-dev/shared-context-engineering";
  };

  outputs =
    {
      self,
      nixpkgs,
      rust-overlay,
      flake-utils,
      sce,
      ...
    }:
    flake-utils.lib.eachDefaultSystem (
      system:
      let
        pkgs = import nixpkgs {
          inherit system;
          overlays = [ rust-overlay.overlays.default ];
        };

        rust = pkgs.rust-bin.stable.latest.default.override {
          extensions = [
            "rust-src"
            "rust-analyzer"
          ];
        };
      in
      {
        devShells.default = pkgs.mkShell {
          name = "fyah";
          buildInputs = [
            pkgs.pkg-config
            pkgs.openssl
            sce.packages.${system}.default
            rust
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
