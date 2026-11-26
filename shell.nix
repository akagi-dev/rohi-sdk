{
  pkgs,
  linker ? "wild",
  packages ? [ ],
  env ? { },
}:

assert pkgs.lib.assertOneOf "linker" linker [
  "mold"
  "wild"
  "lld"
];

let
  cargoLinker =
    with pkgs;
    let
      rustTarget = stdenv.hostPlatform.rust.cargoEnvVarTarget;
      linkerFlags = {
        lld = "-Clink-arg=-fuse-ld=${llvmPackages.lld}/bin/ld.lld -Clink-arg=-Wl,--no-rosegment";
        mold = "-Clink-arg=-fuse-ld=${mold}/bin/ld.mold -Clink-arg=-Wl,--no-rosegment -Clink-arg=-flto";
        wild = "-Clink-arg=-fuse-ld=${wild}/bin/ld.wild -Clink-arg=-flto";
      };
    in
    if stdenv.hostPlatform.isDarwin then
      # clangStdenv still injects -fuse-ld=ld64.lld; that linker mishandles Unicode paths.
      {
        "CARGO_TARGET_${rustTarget}_LINKER" = "/usr/bin/clang";
        "CARGO_TARGET_${rustTarget}_RUSTFLAGS" = "-Clink-arg=-fuse-ld=/usr/bin/ld";
        RUSTFLAGS = "-Clink-arg=-fuse-ld=/usr/bin/ld";
        CC = "/usr/bin/clang";
        CXX = "/usr/bin/clang++";
      }
    else
      {
        "CARGO_TARGET_${rustTarget}_LINKER" = "clang";
        "CARGO_TARGET_${rustTarget}_RUSTFLAGS" = linkerFlags.${linker};
      };
in
with pkgs;
mkShell.override { stdenv = clangStdenv; } {
  packages =
    packages
    ++ [ rust-toolchain ]
    ++ lib.optionals stdenv.hostPlatform.isDarwin [ bashInteractive ];
  env = cargoLinker // env;
}
