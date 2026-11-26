{
  description = "ROHI SDK Flakes";

  inputs = {
    systems.url = "github:nix-systems/default";
    nixpkgs.url = "github:NixOS/nixpkgs/f20cd90e28fe0403873f473b29e42662cd86e8c4";
    # Nixpkgs 26.11 dropped Intel Mac; 26.05 still supports it through 2026.
    nixpkgs-x86_64-darwin.url = "github:NixOS/nixpkgs/nixpkgs-26.05-darwin";
    fenix.url = "github:nix-community/fenix";
    fenix.inputs.nixpkgs.follows = "nixpkgs";
    robins.url = "github:airalab/robins";
    robins.inputs.nixpkgs.follows = "nixpkgs";
  };

  outputs =
    {
      self,
      nixpkgs,
      nixpkgs-x86_64-darwin,
      systems,
      fenix,
      robins,
      ...
    }:
    let
      toolchainFile = {
        file = ./rust-toolchain.toml;
        sha256 = "sha256-+xUr6Al1wZSqX8aEsq7DpuFOK4Ve/i+qTl+K5uWQ9GM=";
      };
      mkPkgs =
        system:
        let
          intelMac = system == "x86_64-darwin";
          pkgs =
            if intelMac then
              nixpkgs-x86_64-darwin.legacyPackages.${system}
            else
              nixpkgs.legacyPackages.${system};
          rustOverlay = final: prev: {
            rust-toolchain =
              if intelMac then
                # fenix.packages no longer lists x86_64-darwin; the overlay still works on 26.05.
                final.fenix.fromToolchainFile toolchainFile
              else
                fenix.packages.${system}.fromToolchainFile toolchainFile;
          };
          # `edge` / robins overlay track the 26.11 pin; do not mix them into 26.05.
          overlays =
            (if intelMac then [ fenix.overlays.default ] else [ robins.overlays.default ])
            ++ [ rustOverlay ];
        in
        pkgs.appendOverlays overlays;
      # nix-systems/default dropped Intel Mac; keep it so `nix develop` works on x86_64-darwin.
      supportedSystems = nixpkgs.lib.unique ((import systems) ++ [ "x86_64-darwin" ]);
      eachSystem = f: nixpkgs.lib.genAttrs supportedSystems (system: f system (mkPkgs system));
    in
    {
      checks = eachSystem (
        system: pkgs: {
          buildAll = pkgs.symlinkJoin {
            name = "build-all-packages";
            paths = builtins.attrValues self.packages.${system};
          };
        }
      );

      packages.x86_64-darwin.bashInteractive =
        nixpkgs-x86_64-darwin.legacyPackages.x86_64-darwin.bashInteractive;

      # macOS ships Bash 3.2, which cannot parse the Nix stdenv hook (`;&`).
      # `nix run .` puts Bash 5 on PATH and then enters the same dev shell as `nix develop`.
      apps.x86_64-darwin.default =
        let
          pkgs = nixpkgs-x86_64-darwin.legacyPackages.x86_64-darwin;
          script = pkgs.writeShellScript "rohi-devshell" ''
            set -euo pipefail
            export PATH="${pkgs.bashInteractive}/bin:$PATH"
            exec nix develop --command "''${SHELL:-zsh}"
          '';
        in
        {
          type = "app";
          program = "${script}";
        };

      lib = eachSystem (system: pkgs: {
        mkDevShell = args: import ./shell.nix ({ inherit pkgs; } // args);
      });

      devShells = eachSystem (
        system: pkgs:
        {
          default = self.lib.${system}.mkDevShell (
            {
              packages =
                with pkgs;
                [
                  taplo
                  actionlint
                  cargo-machete
                  cargo-bloat
                  espflash
                  mdbook
                  lychee
                  protobuf
                  pkg-config
                ]
                ++ lib.optionals stdenv.hostPlatform.isLinux [ dbus ];
            }
            // pkgs.lib.optionalAttrs (system != "x86_64-darwin") {
              env.RUSTC_WRAPPER = pkgs.lib.getExe pkgs.sccache;
            }
          );
        }
        // pkgs.lib.optionalAttrs (system != "x86_64-darwin") {
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
