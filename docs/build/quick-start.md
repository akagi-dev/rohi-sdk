# Quick start

This section takes a developer from a minimal firmware to a device that generic ROHI clients can operate.

1. **Set up** the toolchain: [Installation](../getting-started/installation.md), then
   [Workspace layout](../getting-started/workspace.md).
2. **Run a sensor example** on a board: [First device](../getting-started/first-device.md). It uses the hardware
   independent drivers and the board module, with no networking, and shows how to work on it with an AI assistant.
3. **Build and test** the SDK: [Build and test](../getting-started/build-and-test.md).
4. **Flash the reference firmware** and connect with [`rohi`](../use/desktop/index.md):

   ```bash
   cargo run --release -p altruist-urban --features esp32c6 --target riscv32imac-unknown-none-elf
   rohi status
   ```

   (ESP32-C3: `--features esp32c3 --target riscv32imc-unknown-none-elf`.)
5. **Learn the three concepts** a firmware exposes: [Config, Status and Action](core-concepts.md).
6. **Add your own capability**, also with an AI assistant: [Create & customize](create-and-customize.md). It becomes visible in generic clients with
   no client change.

Check that the board you target is listed in the [support matrix](../support/matrix.md) and add your findings to it when you
validate hardware: [Validation](../verify/validation.md).

The reference firmware is [`firmwares/altruist-urban`](https://github.com/akagi-dev/rohi-sdk/tree/master/firmwares/altruist-urban);
complete one-sensor programs are in [`examples/`](https://github.com/akagi-dev/rohi-sdk/tree/master/examples/src/bin).
