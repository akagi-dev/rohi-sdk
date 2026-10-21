# Configuration store

`rohi-config` is a hardware independent, transactional configuration store. Configuration is a set of
**independent typed sections**; there is no central `DeviceConfig`. A feature crate defines its own section and
the store persists it without knowing anything about it.

```text
 your type ── #[derive(ConfigSection)] ── section ID / version
     │
 ConfigStore ── postcard payload ── container / A-B / CRC ── ConfigStorage
                                                              ├─ MemoryStorage      (host tests)
                                                              └─ EspConfigStorage   (ESP32-C3 flash)
```

Crates:

| Crate                 | Responsibility                                                                    |
|-----------------------|-----------------------------------------------------------------------------------|
| `rohi-config`         | `ConfigSection`, `ConfigStorage`, `ConfigStore`, container format, `MemoryStorage` |
| `rohi-config-derive`  | `#[derive(ConfigSection)]`, stable section IDs                                      |
| `rohi-config-esp`     | ESP32-C3 flash backend, bounded flash region, partition lookup                      |

## Defining a section

```rust
use postcard::experimental::max_size::MaxSize;
use rohi_config::ConfigSection;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, MaxSize, ConfigSection)]
#[config(name = "wifi", version = 1)]
pub struct WifiConfig {
    pub ssid_len: u8,
    pub ssid: [u8; 32],
    pub password_len: u8,
    pub password: [u8; 64],
}

store.set(&wifi)?;
let wifi = store.get::<WifiConfig>()?; // Option<WifiConfig>
```

`store.remove::<T>()` deletes one section, `store.clear()` all of them. The caller never handles section IDs,
headers, payload framing, CRCs, slots or generations.

> **Codec.** Section payloads use [postcard](https://docs.rs/postcard) (`serde`), not SCALE or JSON.
> Types must implement `Serialize`, `Deserialize` and postcard's `MaxSize`. Prefer bounded types (fixed arrays,
> bounded strings/vectors) over `String`/`Vec`. Every payload must be at most `MaxSize::POSTCARD_MAX_SIZE` and
> `MAX_SECTION_PAYLOAD` bytes; larger values are rejected before anything is written.
> Decoding rejects trailing bytes.
>
> **Secrets.** Do not derive `Debug` for secret-bearing sections (Wi-Fi passwords, private keys); implement it by
> hand with redaction so secrets never reach logs or panic output.

### `#[config(...)]` attributes

| Attribute                        | Meaning                                                                         |
|----------------------------------|---------------------------------------------------------------------------------|
| `version = N`                    | **Required.** Schema version of this section (`u16`).                           |
| `name = "wifi"`                  | Stable logical name; hashed into the section ID.                                |
| `id = 0xAABB…`                   | Explicit 64-bit ID. Mutually exclusive with `name`.                             |
| `decode_versioned = "path::fn"`  | Migration hook `fn(version: u16, payload: &[u8]) -> Result<Self, SectionError>`. |

Without `name` and `id` the Rust type name is hashed; renaming the type then changes the persistent ID, so
production sections should set `name`. There are no global migrations: each section versions itself, and the
default decoder accepts only `stored version == T::VERSION` (`SectionError::UnsupportedVersion` otherwise).

### Section ID ABI

`SectionId(u64)` is the **FNV-1a 64-bit** hash (offset basis `0xcbf29ce484222325`, prime `0x100000001b3`) of the
UTF-8 bytes of the name. Priority: explicit `id`, then `name`, then the Rust type name. The algorithm is part of
the persistent storage ABI and must never change. Golden values:

| Name              | `SectionId`            |
|-------------------|------------------------|
| `wifi`            | `0xa63b01f6557796a8`   |
| `device-identity` | `0xa629f5606897ad50`   |
| `telemetry`       | `0xa042ab53f67e07c0`   |

## Container format

The region is split into two equal slots, **A** and **B**. All integers are little-endian; fields are encoded
explicitly, never as Rust memory layout.

```text
slot:    SlotHeader (24 B) | section | section | ...        (rest of the slot stays erased)
section: SectionHeader (16 B) | postcard payload | zero padding to 4 bytes
```

| SlotHeader offset | Size | Field                                                           |
|-------------------|------|-----------------------------------------------------------------|
| 0                 | 4    | magic `"ROHC"`                                                  |
| 4                 | 2    | format version (`1`)                                            |
| 6                 | 2    | reserved (0)                                                    |
| 8                 | 4    | generation                                                      |
| 12                | 4    | payload length (multiple of 4, section area only)               |
| 16                | 4    | CRC-32 (IEEE) over the payload bytes, then header bytes `4..16` |
| 20                | 4    | commit marker `"COMT"`, written **last**                        |

| SectionHeader offset | Size | Field                                    |
|----------------------|------|------------------------------------------|
| 0                    | 8    | section ID                               |
| 8                    | 2    | section version                          |
| 10                   | 2    | reserved (0)                             |
| 12                   | 4    | actual payload length (without padding)  |

A slot is valid only if magic, format version, commit marker, length bounds, CRC and section structure all check
out, there are at most `MAX_SECTIONS` sections and **no duplicate section ID**. CRC-32 detects accidental
corruption; it is not authentication.

Limits: `MAX_SECTIONS = 16`, `MAX_SECTION_PAYLOAD = 1024` bytes, slot size at most `MAX_SLOT_SIZE = 64 KiB`
(half of the region, rounded down to the erase size). All length arithmetic is checked.

## Transactions

Every update streams the complete new container into the **inactive** slot; the active slot is never modified:

```text
plan (size / section count, nothing written)
 -> erase inactive slot
 -> stream sections: replaced one re-serialized, all others copied byte-for-byte
 -> write slot header (without commit marker)
 -> read back and verify the whole slot
 -> write commit marker LAST
```

`ConfigStore::open` picks the valid slot with the newest generation (wrap-safe serial comparison, so
`u32::MAX -> 0` is handled). A corrupt or uncommitted newest slot falls back to the previous generation.

> After interruption at any point during an update, the previous valid configuration or the completely
> committed new configuration remains loadable.

The host tests prove it by failing the storage after every possible number of programmed bytes (torn writes) and
erased bytes, then reopening the store.

### Unknown sections

Sections whose ID the running firmware does not know are never decoded; they are copied unchanged (header,
payload and padding) on every update, so older firmware preserves a newer firmware's sections. They can be listed
(`for_each_section`), read raw (`read_raw`) and explicitly removed (`remove_raw`).

## Storage backends

```rust
pub trait ConfigStorage {
    type Error;
    const ERASE_SIZE: usize;
    const WRITE_SIZE: usize;
    fn capacity(&self) -> usize;
    fn read(&mut self, offset: usize, output: &mut [u8]) -> Result<(), Self::Error>;
    fn write(&mut self, offset: usize, data: &[u8]) -> Result<(), Self::Error>;
    fn erase(&mut self, offset: usize, len: usize) -> Result<(), Self::Error>;
}
```

* `MemoryStorage<N, ERASE>` emulates flash (erase to `0xFF`, writes only to erased words) and offers
  deterministic fault injection: `fail_write_after(bytes)` tears the write at that byte,
  `fail_erase_after(bytes)` leaves a partially erased sector.
* `EspConfigStorage<F: NorFlash>` restricts an `embedded-storage` NOR flash to a `FlashRegion { offset, length }`.
  Every offset is region-relative and checked; writes and erases must be word/sector aligned
  (`EspStorageError::InvalidAlignment`), unaligned reads are served through a small bounce buffer. Errors are the
  typed `OutOfBounds`, `InvalidAlignment`, `InvalidRegion`, `ReadFailed`, `WriteFailed`, `EraseFailed`.
  With the `esp` feature, `rohi_config_esp::esp::config_storage` builds it on the `esp-storage` driver and the
  `rohi-cfg` partition.

## Flash layout (Altruist Urban, ESP32-C3, 4 MB)

The single source of truth is `firmwares/altruist-urban/partitions.csv`, mirrored by the constants in
`rohi_config_esp::layout` (a host test keeps both in sync):

```text
0x09000  nvs        24 KiB
0x0f000  phy_init    4 KiB
0x10000  rohi-cfg   16 KiB   <- configuration region
           ├─ slot A  8 KiB (2 sectors)
           └─ slot B  8 KiB (2 sectors)
0x20000  factory    app
```

`cargo run` passes the table to `espflash` (see `.cargo/config.toml`); run it from the repository root. If the
partition is missing the firmware logs an error and keeps running the sensors without persistent configuration.

## Resource baseline

| Item                               | Size                                                            |
|------------------------------------|-----------------------------------------------------------------|
| Configuration flash region         | 16 KiB (`rohi-cfg`)                                             |
| Slot size                          | 8 KiB                                                           |
| Slot header / section header       | 24 B / 16 B                                                     |
| Maximum container payload          | 8 168 B (slot minus header)                                     |
| Maximum section payload            | 1 024 B, at most `MaxSize::POSTCARD_MAX_SIZE`                   |
| Maximum sections                   | 16                                                              |
| `ConfigStore` RAM                  | ~1 KiB read workspace plus a few words of state                 |
| Commit stack workspace             | < 0.5 KiB (64 B write buffer, 64 B copy chunk, 128 B ID set)    |

Commit streams postcard output straight into flash and copies unrelated sections in 64 byte chunks, so there is
never a second copy of the configuration in RAM.

## Hardware self-test

`examples/src/bin/config.rs` verifies persistence on a real ESP32-C3. Flash it and reset or power-cycle the board
repeatedly:

```bash
cargo run --release -p rohi-examples --bin example-config
```

Every boot checks what the previous boot stored (typed write + reboot + read, several sections, replacement
preserving unrelated sections, repeated write cycles) and logs `SELFTEST PASS` or `SELFTEST FAIL`. On the third
boot it corrupts the newest slot on purpose and checks the fallback to the previous generation.

## Not in this layer

Device identity (WP4.1), BLE/Wi-Fi control, OTA, flash encryption and JSON configuration are out of scope.
