#!/usr/bin/env bash
# Flash and RAM section sizes of one Altruist Urban firmware variant, see docs/validation/compact-firmware-report.md.
#
#   scripts/measure.sh <esp32c3|esp32c6> [extra altruist-urban features, comma separated, e.g. lz4]
#
# Builds with the same target, profile and flags as `cargo build-<chip>` and prints one TSV line per section group:
#   <chip> <features> <metric> <bytes>
# `flash` is what an image occupies in flash (.text, .rodata, .rwtext, the initial values of .data), `ram_static` is
# .data + .bss. Run from the repository root, with `cargo` and GNU `size` on PATH. For task pools, frames and the main
# stack use scripts/memcheck.sh.
set -euo pipefail

chip="${1:?chip}"
extra="${2:-}"
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
case $chip in
    esp32c3) triple=riscv32imc-unknown-none-elf; profile=firmware; flag="--profile firmware" ;;
    esp32c6) triple=riscv32imac-unknown-none-elf; profile=release; flag="--release" ;;
    *) echo "unknown chip: $chip" >&2; exit 2 ;;
esac
features="$chip${extra:+,$extra}"
cd "$root"
# shellcheck disable=SC2086
cargo build $flag -p altruist-urban --no-default-features --features "$features" --target "$triple" >&2
elf="target/$triple/$profile/altruist-urban-firmware"
size -A "$elf" | awk -v chip="$chip" -v features="$features" '
    $1 ~ /^\.(text|rodata|rwtext|trap|flash\.appdesc|rodata\.wifi|rwtext\.wifi)$/ { flash += $2 }
    $1 == ".rwdata_dummy" || $1 == ".rotext_dummy" { next }
    $1 == ".data" || $1 == ".data.wifi" { data += $2 }
    $1 == ".bss" { bss += $2 }
    $1 == ".text" { text = $2 }
    $1 == ".rodata" { rodata = $2 }
    END {
        printf "%s\t%s\ttext\t%d\n", chip, features, text
        printf "%s\t%s\trodata\t%d\n", chip, features, rodata
        printf "%s\t%s\tdata\t%d\n", chip, features, data
        printf "%s\t%s\tbss\t%d\n", chip, features, bss
        printf "%s\t%s\tflash\t%d\n", chip, features, flash + data
        printf "%s\t%s\tram_static\t%d\n", chip, features, data + bss
    }'

# The default image carries no LZ4 codec (see `lz4` in rohi-logging).
if [[ ",$extra," != *,lz4,* ]] && nm -C "$elf" | grep -qi 'lz4'; then
    echo "an LZ4 symbol is linked into the image built without the lz4 feature" >&2
    exit 1
fi
