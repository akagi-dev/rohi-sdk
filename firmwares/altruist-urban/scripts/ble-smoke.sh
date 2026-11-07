#!/usr/bin/env bash
# Smoke test of the Linux BLE peripheral of the std Altruist Urban.
#
# Checks that BlueZ and an adapter are usable, then runs the device with a BLE
# peripheral and simulated sensors. It cannot check a phone: follow the steps in
# docs/guides/linux-ble.md with the ROHI mobile application while it runs.
#
# Usage: ble-smoke.sh [ADAPTER]      (for example hci0)
set -euo pipefail

adapter="${1:-}"
fail() { echo "ble-smoke: $*" >&2; exit 1; }

command -v bluetoothctl >/dev/null || fail "bluetoothctl not found: install BlueZ"
if command -v systemctl >/dev/null; then
  systemctl is-active --quiet bluetooth || fail "bluetoothd is not running (systemctl start bluetooth)"
fi
if command -v rfkill >/dev/null && rfkill list bluetooth | grep -q 'Soft blocked: yes'; then
  fail "Bluetooth is blocked (rfkill unblock bluetooth)"
fi
bluetoothctl show ${adapter:+"$adapter"} | grep -q 'Powered: yes' \
  || fail "no powered adapter (bluetoothctl power on)"

args=(--ephemeral --log-level debug)
[ -n "$adapter" ] && args+=(--ble-adapter "$adapter")

echo "ble-smoke: starting the device; look for ALTRUIST-URBAN-XXXX in the mobile application"
echo "ble-smoke: stop it with Ctrl-C"
cd "$(dirname "$0")/../../.."
exec cargo run -p altruist-urban --bin altruist-urban \
  --features std,std-tcp,std-ble,sensor-simulator -- "${args[@]}"
