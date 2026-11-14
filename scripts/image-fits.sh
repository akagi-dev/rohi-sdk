#!/usr/bin/env bash
# Checks that the Altruist Urban firmware image of one chip fits the `factory` partition of its partition table.
#
#   scripts/image-fits.sh <esp32c3|esp32c6>
#
# Run it after `cargo build-<chip>`, from the repository root, with `espflash` on PATH. `espflash save-image` reads the
# partition table and fails when the image does not fit the app partition; its summary shows the size and the share.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
chip="${1:?chip}"
# Keep in sync with the build-<chip> aliases in .cargo/config.toml and the tables next to the firmware.
case $chip in
    esp32c3) triple=riscv32imc-unknown-none-elf; profile=firmware; flash=4mb ;;
    esp32c6) triple=riscv32imac-unknown-none-elf; profile=release; flash=8mb ;;
    *) echo "unknown chip: $chip" >&2; exit 2 ;;
esac
elf="$root/target/$triple/$profile/altruist-urban-firmware"
[[ -f $elf ]] || { echo "$elf not found: run cargo build-$chip first" >&2; exit 2; }
out="$(mktemp -d)"
trap 'rm -rf "$out"' EXIT
espflash save-image --chip "$chip" --flash-size "$flash" \
    --partition-table "$root/firmwares/altruist-urban/partitions-$chip.csv" "$elf" "$out/app.bin"
