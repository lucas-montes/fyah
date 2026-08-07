load("@rules_nixpkgs_core//:nixpkgs.bzl", "nixpkgs_local_repository")
load("@rules_nixpkgs_cc//:cc.bzl", "nixpkgs_cc_configure")
load("@rules_nixpkgs_rust//:rust.bzl", "nixpkgs_rust_configure")

def _non_module_deps_impl(_ctx):
    nixpkgs_local_repository(
        name = "nixpkgs",
        nix_file = "//:nix/nixpkgs.nix",
        nix_file_deps = ["//:flake.lock"],
    )

    nixpkgs_cc_configure(
        name = "nixpkgs_config_cc",
        repository = "@nixpkgs",
        register = False,
    )

    nixpkgs_rust_configure(
        name = "nixpkgs_config_rust",
        repository = "@nixpkgs",
        nix_file = "//:nix/rust-toolchain.nix",
        nix_file_deps = ["//:flake.lock"],
        default_edition = "2024",
        register = False,
    )

non_module_deps = module_extension(
    implementation = _non_module_deps_impl,
)
