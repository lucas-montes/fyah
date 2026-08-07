let
  lock = builtins.fromJSON (builtins.readFile ../flake.lock);
  node = lock.nodes.nixpkgs.locked;
  nixpkgs = fetchTarball {
    url = "https://github.com/${node.owner}/${node.repo}/archive/${node.rev}.tar.gz";
    sha256 = node.narHash;
  };
in
import nixpkgs
