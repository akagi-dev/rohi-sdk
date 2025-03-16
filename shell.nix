{ rust-overlay ? import (builtins.fetchTarball https://github.com/oxalica/rust-overlay/archive/master.tar.gz),
  pkgs ? import <nixpkgs> { overlays = [ rust-overlay ]; },
  toolchain ? pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml,
}:

with pkgs;
with llvmPackages;

mkShell {
  buildInputs = [
    taplo
    mdbook
    espflash
    toolchain
  ];
}
