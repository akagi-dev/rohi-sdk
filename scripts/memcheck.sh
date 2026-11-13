#!/usr/bin/env bash
# Task memory and stack budget analysis of Altruist Urban for one chip, see docs/validation/resource-budget.md.
#
#   scripts/memcheck.sh images  <esp32c3|esp32c6> <out-dir>        build the production and the analysis ELF
#   scripts/memcheck.sh analyze <esp32c3|esp32c6> <out-dir> [memcheck analyze options, e.g. --base base.json]
#   scripts/memcheck.sh report  <out-dir> [memcheck report options]  render task-memory-report.md (complete) and task-memory-summary.md (anomalies only)
#
# The analysis ELF is the production build plus `-Z emit-stack-sizes` (RUSTC_BOOTSTRAP=1 enables it on the pinned
# stable toolchain, so both images use the same compiler); its `.stack_sizes` section feeds the frame report. The
# production ELF is what the RAM, task and stack numbers come from. Run from the repository root, with `cargo` on PATH.
# `MEMCHECK` (path of a built memcheck binary) and `MEMCHECK_GIT_SHA` (commit recorded in the report) are optional.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
budgets="$root/firmwares/altruist-urban/memcheck-budgets.toml"
cmd="${1:-}"
[[ -n $cmd ]] || { sed -n '2,10p' "${BASH_SOURCE[0]}" >&2; exit 2; }
shift

memcheck() {
    if [[ -n ${MEMCHECK:-} ]]; then "$MEMCHECK" "$@"; else cargo run -q --release -p rohi-memcheck -- "$@"; fi
}

if [[ $cmd == report ]]; then
    out="${1:?out-dir}"; shift
    memcheck report "$out"/task-memory-esp32c*.json --out "$out/task-memory-report.md" \
        --summary-out "$out/task-memory-summary.md" "$@"
    exit
fi

chip="${1:?chip}"; out="${2:?out-dir}"; shift 2
# Keep in sync with the build-<chip> aliases in .cargo/config.toml.
case $chip in
    esp32c3) triple=riscv32imc-unknown-none-elf; profile=firmware; flag="--profile firmware" ;;
    esp32c6) triple=riscv32imac-unknown-none-elf; profile=release; flag="--release" ;;
    *) echo "unknown chip: $chip" >&2; exit 2 ;;
esac
grep -q "^build-$chip = .*$flag " "$root/.cargo/config.toml" \
    || { echo "the build-$chip alias no longer uses '$flag'; update scripts/memcheck.sh" >&2; exit 2; }
mkdir -p "$out/$chip"
elf="target/$triple/$profile/altruist-urban-firmware"
flags="-C force-frame-pointers"
analysis_flags="$flags -Z emit-stack-sizes"

case $cmd in
    images)
        cargo "build-$chip"
        cp "$elf" "$out/$chip/firmware.elf"
        RUSTC_BOOTSTRAP=1 RUSTFLAGS="$analysis_flags" CARGO_TARGET_DIR="$out/$chip/analysis-target" cargo "build-$chip"
        cp "$out/$chip/analysis-target/$triple/$profile/altruist-urban-firmware" "$out/$chip/analysis.elf"
        rm -rf "$out/$chip/analysis-target"
        ;;
    analyze)
        memcheck analyze --chip "$chip" \
            --elf "$out/$chip/firmware.elf" --analysis-elf "$out/$chip/analysis.elf" \
            --budgets "$budgets" \
            --out-json "$out/task-memory-$chip.json" \
            --toolchain "$(rustc --version)" \
            --git-sha "${MEMCHECK_GIT_SHA:-$(git -C "$root" rev-parse HEAD)}" \
            --target "$triple" --profile "$profile" --features "$chip" \
            --flags="$flags" --analysis-flags="$analysis_flags" "$@"
        ;;
    *) echo "unknown command: $cmd" >&2; exit 2 ;;
esac
