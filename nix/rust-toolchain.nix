let
  nixpkgsSrc = import ./nixpkgs.nix;
  pkgs = import nixpkgsSrc { config = {}; overlays = []; };
in
pkgs.rust-bin.stable.latest.default.override {
  extensions = [ "rust-src" ];
}
