# Boot the same 7.2.15 kernel the host runs, load the module, and drive the
# ABI from a standalone binary. This is the gate before loading on the host.
{
  hostPkgs,
  fyahKmod,
}:
let
  pkgs = hostPkgs;
  kernel = pkgs.linux_7_2;

  kmodTest = pkgs.stdenv.mkDerivation {
    pname = "fyah-km-test";
    version = "1";
    src = ../kmod/fyah_km;

    nativeBuildInputs = [
      pkgs.rustc
      pkgs.stdenv.cc
    ];
    hardeningDisable = [ "all" ];
    dontConfigure = true;

    buildPhase = ''
      runHook preBuild
      rustc --edition 2021 -O kmod_test.rs -o kmod-test
      runHook postBuild
    '';

    installPhase = ''
      runHook preInstall
      install -Dm755 kmod-test "$out/bin/kmod-test"
      runHook postInstall
    '';
  };
in
pkgs.testers.nixosTest {
  name = "fyah-km";

  nodes.machine = {
    boot.kernelPackages = pkgs.linuxPackages_7_2;
    environment.systemPackages = [ kmodTest ];
    environment.etc."fyah_km.ko".source =
      "${fyahKmod}/lib/modules/${kernel.modDirVersion}/extra/fyah_km.ko";
    virtualisation.memorySize = 1024;
    virtualisation.cores = 2;
    system.stateVersion = "26.05";
  };

  testScript = ''
    machine.start()
    machine.succeed("insmod /etc/fyah_km.ko")
    machine.wait_for_file("/dev/fyah")
    machine.succeed("kmod-test")
    machine.succeed("dmesg | grep -q 'fyah_km: req .* op write '")
    machine.succeed("rmmod fyah_km")
    machine.fail("test -e /dev/fyah")
    machine.succeed("dmesg | grep -q 'fyah_km: unloaded'")
    machine.fail("dmesg | grep -E 'Oops|BUG:|Call Trace'")
  '';
}
