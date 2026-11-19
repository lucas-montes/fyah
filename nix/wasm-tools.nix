# Tooling for the wasm tools fyah.toml points at.
#
#   javy         Javy, which turns a .js file into a WASI preview 1 module.
#                Not in nixpkgs: the upstream release binary, patched for Nix.
#   python-wasi  CPython built for WASI: python.wasm and its standard library.
#                The upstream release; nixpkgs' wasi cross build is fragile.
#   app          `nix run .#wasm-tools`, from the repository root. Fills
#                wasm/dist from the working tree:
#                  js/<name>.wasm      one per wasm/js/<name>.js, built by Javy
#                  python/python.wasm  the interpreter (a link into the store)
#                  python/lib/         its standard library
#                It reads the sources as they are on disk, so a new .js file
#                needs no git add and no flake rebuild. Python scripts under
#                wasm/python/ are read at call time and need no build.
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

  app = pkgs.writeShellApplication {
    name = "fyah-wasm-tools";
    runtimeInputs = [javy];
    text = ''
      if [[ ! -f fyah.toml || ! -d wasm ]]; then
        echo "run this from the fyah repository root" >&2
        exit 1
      fi

      rm -rf wasm/dist
      mkdir -p wasm/dist/js wasm/dist/python

      shopt -s nullglob
      for source in wasm/js/*.js; do
        name=$(basename "$source" .js)
        javy build "$source" -o "wasm/dist/js/$name.wasm"
        echo "built wasm/dist/js/$name.wasm"
      done

      ln -sfn ${python-wasi}/python.wasm wasm/dist/python/python.wasm
      ln -sfn ${python-wasi}/lib wasm/dist/python/lib
      echo "linked wasm/dist/python"
    '';
  };
in {
  inherit javy python-wasi app;
}
