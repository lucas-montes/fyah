let
  pkgs = import <nixpkgs> {
    config = { };
    overlays = [ ];
  };

  lock = builtins.fromJSON (builtins.readFile ../flake.lock);
  rust-overlay = lock.nodes.rust-overlay.locked;
  rustOverlaySrc = builtins.fetchTarball {
    url = "https://github.com/${rust-overlay.owner}/${rust-overlay.repo}/archive/${rust-overlay.rev}.tar.gz";
    sha256 = rust-overlay.narHash;
  };

  # Match the flake's devShell rust exactly: rust-overlay stable.latest + rust-src.
  rust = (pkgs.extend (import rustOverlaySrc))
    .rust-bin.stable.latest.default.override {
      extensions = [ "rust-src" ];
    };

  rustlib = pkgs.rust;
  os = rustlib.toTargetOs pkgs.stdenv.targetPlatform;
  build-triple = rustlib.toRustTargetSpec pkgs.stdenv.buildPlatform;
  target-triple = rustlib.toRustTargetSpec pkgs.stdenv.targetPlatform;
  rustc = if os == "macos" then
    # rustc on MacOS expects the `strip` binary in PATH, create a wrapper adding the bin
    # folder from `binutils` to the PATH
    pkgs.symlinkJoin
      {
        name = "rustc";
        paths = [ rust ];
        nativeBuildInputs = [ pkgs.makeWrapper ];
        postBuild = ''
          wrapProgram $out/bin/rustc --suffix PATH : ${pkgs.darwin.cctools}/bin
        '';
      }
  else
    rust;
  binary-ext = "";
  staticlib-ext = ".a";
  dylib-ext = if os == "macos" then ".dylib" else ".so";
in
pkgs.buildEnv {
  extraOutputsToInstall = [ "out" "bin" "lib" ];
  ignoreCollisions = true;
  name = "bazel-rust-toolchain";
  paths = [ rustc ];
  postBuild = ''
    cat <<EOF > $out/BUILD
    filegroup(
        name = "rustc",
        srcs = ["bin/rustc"],
        visibility = ["//visibility:public"],
    )

    filegroup(
        name = "rustfmt",
        srcs = ["bin/rustfmt"],
        visibility = ["//visibility:public"],
    )

    filegroup(
        name = "cargo",
        srcs = ["bin/cargo"],
        visibility = ["//visibility:public"],
    )

    filegroup(
        name = "clippy_driver",
        srcs = ["bin/clippy-driver"],
        visibility = ["//visibility:public"],
    )

    filegroup(
        name = "rustc_lib",
        srcs = glob(
            [
                "bin/*.so",
                "lib/*.so",
                "lib/rustlib/*/codegen-backends/*.so",
                "lib/rustlib/*/codegen-backends/*.dylib",
                "lib/rustlib/*/bin/rust-lld",
                "lib/rustlib/*/lib/*.so",
                "lib/rustlib/*/lib/*.dylib",
            ],
            allow_empty = True,
        ),
        visibility = ["//visibility:public"],
    )

    load("@rules_rust//rust:toolchain.bzl", "rust_stdlib_filegroup")
    rust_stdlib_filegroup(
        name = "rust_std",
        srcs = glob(
            [
                "lib/rustlib/*/lib/*.rlib",
                "lib/rustlib/*/lib/*.rmeta",
                "lib/rustlib/*/lib/*.so",
                "lib/rustlib/*/lib/*.dylib",
                "lib/rustlib/*/lib/*.a",
                "lib/rustlib/*/lib/self-contained/**",
            ],
            allow_empty = True,
        ),
        visibility = ["//visibility:public"],
    )

    filegroup(
        name = "rust_doc",
        srcs = ["bin/rustdoc"],
        visibility = ["//visibility:public"],
    )

    load('@rules_rust//rust:toolchain.bzl', 'rust_toolchain', 'rustfmt_toolchain')
    rust_toolchain(
        name = "rust_nix_impl",
        rust_doc = ":rust_doc",
        rust_std = ":rust_std",
        rustc = ":rustc",
        rustfmt = ":rustfmt",
        cargo = ":cargo",
        clippy_driver = ":clippy_driver",
        rustc_lib = ":rustc_lib",
        binary_ext = "${binary-ext}",
        staticlib_ext = "${staticlib-ext}",
        dylib_ext = "${dylib-ext}",
        exec_triple = "${build-triple}",
        target_triple = "${target-triple}",
        default_edition = "2024",
        stdlib_linkflags = ["-lpthread", "-ldl"],
        visibility = ["//visibility:public"],
    )

    rustfmt_toolchain(
        name = "rustfmt_nix_impl",
        rustfmt = ":rustfmt",
        visibility = ["//visibility:public"],
    )
    EOF
  '';
}
