# Build the fyah companion kernel module against the host's exact kernel.
#
# The module is tied to one kernel: same version, config and vermagic. This
# uses the host nixpkgs revision (the one the running system boots) so the
# resulting `.ko` can be loaded on the host and in a matching VM.
{
  hostPkgs,
  lib,
}:
let
  kernel = hostPkgs.linux_7_2;
  kdir = "${kernel.dev}/lib/modules/${kernel.modDirVersion}/build";

  # The kernel was built with these exact versions; a module must use the
  # same rustc and bindgen or the metadata will not match.
  rustc = hostPkgs.rustc;
  bindgen = hostPkgs.rust-bindgen;
  rustLibSrc = hostPkgs.rustPlatform.rustLibSrc;
  binutils = hostPkgs.binutils;
in
hostPkgs.stdenv.mkDerivation {
  pname = "fyah-km";
  version = kernel.version;
  src = ../kmod/fyah_km;

  nativeBuildInputs = [
    hostPkgs.gnumake
    hostPkgs.gcc
    binutils
    hostPkgs.pahole
    hostPkgs.zstd
    hostPkgs.bc
    hostPkgs.flex
    hostPkgs.bison
    rustc
    bindgen
  ];

  # Kbuild passes its own flags; the stdenv hardening flags would be
  # appended to every compiler call and break the kernel build.
  hardeningDisable = [ "all" ];
  dontStrip = true;

  buildPhase = ''
    runHook preBuild
    make -C ${kdir} M=$PWD \
      CC=${hostPkgs.stdenv.cc}/bin/gcc \
      HOSTCC=${hostPkgs.stdenv.cc}/bin/cc \
      HOSTCXX=${hostPkgs.stdenv.cc}/bin/c++ \
      LD=${binutils}/bin/ld \
      AR=${binutils}/bin/ar \
      NM=${binutils}/bin/nm \
      STRIP=${binutils}/bin/strip \
      OBJCOPY=${binutils}/bin/objcopy \
      OBJDUMP=${binutils}/bin/objdump \
      READELF=${binutils}/bin/readelf \
      HOSTAR=${binutils}/bin/ar \
      HOSTLD=${binutils}/bin/ld \
      RUSTC=${rustc}/bin/rustc \
      BINDGEN=${bindgen}/bin/bindgen \
      RUST_LIB_SRC=${rustLibSrc} \
      KRUSTFLAGS="--remap-path-prefix ${rustLibSrc}=/"
    runHook postBuild
  '';

  installPhase = ''
    runHook preInstall
    install -Dm644 fyah_km.ko \
      "$out/lib/modules/${kernel.modDirVersion}/extra/fyah_km.ko"
    runHook postInstall
  '';

  meta = {
    description = "Privileged primitive layer for the fyah harness";
    license = lib.licenses.gpl2Only;
    platforms = [ "x86_64-linux" ];
  };
}
