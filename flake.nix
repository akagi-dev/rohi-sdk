{
  description = "ROHI SDK Flakes";

  inputs = {
    systems.url = "github:nix-systems/default";
    nixpkgs.url = "github:NixOS/nixpkgs/f20cd90e28fe0403873f473b29e42662cd86e8c4";
    fenix.url = "github:nix-community/fenix";
    fenix.inputs.nixpkgs.follows = "nixpkgs";
    robins.url = "github:airalab/robins";
    robins.inputs.nixpkgs.follows = "nixpkgs";
  };

  outputs =
    {
      self,
      nixpkgs,
      systems,
      fenix,
      robins,
      ...
    }:
    let 
      mkPkgs = system: nixpkgs.legacyPackages.${system}.appendOverlays [
        robins.overlays.default
        (final: prev: {
          rust-toolchain = fenix.packages.${system}.fromToolchainFile { 
            file = ./rust-toolchain.toml;
            sha256 = "sha256-+xUr6Al1wZSqX8aEsq7DpuFOK4Ve/i+qTl+K5uWQ9GM=";
          };
        })
      ];
      eachSystem = f: nixpkgs.lib.genAttrs (import systems) (system: f system (mkPkgs system));
    in {
      checks = eachSystem (
        system: pkgs: {
          buildAll = pkgs.symlinkJoin {
            name = "build-all-packages";
            paths = builtins.attrValues self.packages.${system};
          };
        }
      );

      lib = eachSystem (system: pkgs: {
        mkDevShell = args: import ./shell.nix ({ inherit pkgs; } // args);
      });

      devShells = eachSystem (
        system: pkgs: rec {
          default = self.lib.${system}.mkDevShell {
            packages = with pkgs; [
              taplo actionlint cargo-nextest cargo-audit cargo-machete mdbook
            ];
            env.RUSTC_WRAPPER = pkgs.lib.getExe pkgs.sccache;
          }; 
          e2e = self.lib.${system}.mkDevShell {
            packages = with pkgs; [
              edge
            ];
            env.RUSTC_WRAPPER = pkgs.lib.getExe pkgs.sccache;
          }; 
        }
      );
    };
}
