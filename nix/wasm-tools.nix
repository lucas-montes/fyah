# The wasm tool examples under wasm/, and what builds and runs them.
#
#   javy         Javy, which turns a .js file into a WASI preview 1 module.
#                Not in nixpkgs: the upstream release binary, patched for Nix.
#   python-wasi  CPython built for WASI: python.wasm and its standard library.
#                The upstream release; nixpkgs' wasi cross build is fragile.
#   wasm-tools   What fyah.toml points at, under wasm/dist:
#                  js/text_stats.wasm     wasm/js/text_stats.js, built by Javy
#                  python/python.wasm     the interpreter
#                  python/lib/            its standard library
#                The Python scripts themselves are read from wasm/python/ in
#                the repository, so editing one needs no rebuild.
{pkgs}: let
  inherit (pkgs) lib stdenv fetchurl;

  javy = stdenv.mkDerivation rec {
    pname = "javy";
    version = "9.1.0";
    src = fetchurl {
      url = "https://github.com/bytecodealliance/javy/releases/download/v${version}/javy-x86_64-linux-v${version}.gz";
      sha256 = "a68b122d48eb3dfc1b801d4e14c39271fde3638243d3272d206e376ac9189e39";
    };
    nativeBuildInputs = [pkgs.autoPatchelfHook];
    buildInputs = [stdenv.cc.cc.lib];
    dontUnpack = true;
    installPhase = ''
      runHook preInstall
      install -d $out/bin
      gunzip -c $src > $out/bin/javy
      chmod +x $out/bin/javy
      runHook postInstall
    '';
    meta = {
      description = "JavaScript to WebAssembly toolchain";
      homepage = "https://github.com/bytecodealliance/javy";
      license = lib.licenses.asl20;
      platforms = ["x86_64-linux"];
      mainProgram = "javy";
    };
  };

  python-wasi = stdenv.mkDerivation rec {
    pname = "python-wasi";
    version = "3.15.2";
    src = fetchurl {
      url = "https://github.com/brettcannon/cpython-wasi-build/releases/download/v${version}/python-${version}-wasi_sdk-33.zip";
      sha256 = "b8a63559c56760bbdc310dedb74fd69d10bcbafae26b705bea0ad353ad256888";
    };
    nativeBuildInputs = [pkgs.unzip];
    dontUnpack = true;
    # Nothing here runs on the host: no shebangs to patch, nothing to strip.
    dontFixup = true;
    installPhase = ''
      runHook preInstall
      unzip -q $src -d $out
      runHook postInstall
    '';
    meta = {
      description = "CPython built for WASI";
      homepage = "https://github.com/brettcannon/cpython-wasi-build";
      license = lib.licenses.psfl;
      platforms = lib.platforms.all;
    };
  };

  wasm-tools = stdenv.mkDerivation {
    pname = "fyah-wasm-tools";
    version = "0.1.0";
    src = lib.fileset.toSource {
      root = ../wasm;
      fileset = ../wasm/js;
    };
    nativeBuildInputs = [javy];
    buildPhase = ''
      runHook preBuild
      export HOME=$TMPDIR
      for js in js/*.js; do
        javy build "$js" -o "''${js%.js}.wasm"
      done
      runHook postBuild
    '';
    installPhase = ''
      runHook preInstall
      install -d $out/js $out/python
      cp js/*.wasm $out/js/
      ln -s ${python-wasi}/python.wasm $out/python/python.wasm
      ln -s ${python-wasi}/lib $out/python/lib
      runHook postInstall
    '';
    dontFixup = true;
    meta.platforms = javy.meta.platforms;
  };

  # `nix run .#wasm-tools`, from the repository root: builds the above and
  # points wasm/dist at it. The link is a GC root, so a garbage collection
  # does not pull the modules out from under a running fyah.
  link = pkgs.writeShellScript "fyah-wasm-tools" ''
    set -euo pipefail
    if [[ ! -f fyah.toml ]]; then
      echo "run this from the fyah repository root (no fyah.toml here)" >&2
      exit 1
    fi
    mkdir -p wasm
    nix-store --realise ${wasm-tools} --add-root wasm/dist >/dev/null
    echo "wasm/dist -> ${wasm-tools}"
  '';
in {
  inherit javy python-wasi wasm-tools;
  app = {
    type = "app";
    program = "${link}";
  };
}
