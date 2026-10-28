# Configuration store

`rohi-config` is a hardware independent, transactional configuration store. Configuration is a set of
**independent typed sections**; there is no central `DeviceConfig`. A feature crate defines its own section and
the store persists it without knowing anything about it.

The store only persists bytes. How users see and change a section (`rohi config`, atomic patches of overrides,
secret fields, revisions) is a separate, generic layer: a feature crate also implements `ManagedConfig` for its typed
section, see [Service pipeline](service.md#config-sections).

```text
feature crate
    ↓
typed section ── overrides (`config_overrides!`) or `#[derive(ConfigSection)]` ── section ID / version
    ↓
minicbor payload
    ↓
CBOR SectionRecord
    ↓
CBOR ConfigPayload
    ↓
CBOR SlotEnvelope (+ CRC-32) ── commit word
    ↓
A/B flash transaction ── ConfigStorage
                          ├─ MemoryStorage      (host tests)
                          └─ EspConfigStorage   (ESP32 flash)
```

Crates:

| Crate                 | Responsibility                                                                    |
|-----------------------|-----------------------------------------------------------------------------------|
| `rohi-config`         | `ConfigSection`, `Override`, `config_overrides!`, `ConfigStorage`, `ConfigStore`, CBOR slot format, `MemoryStorage` |
| `rohi-config-derive`  | `#[derive(ConfigSection)]`, section IDs from struct names                          |
| `rohi-config-esp`     | ESP32-C3 / ESP32-C6 flash backend, bounded flash region, partition lookup          |

Each feature crate owns its own schema (for example `rohi-net` owns `NetworkConfig` and `NetworkOverrides`); `rohi-config` never learns
the set of application sections.

## Defining a section

A hand written section derives the codec and `ConfigSection` (this is how `Identity` is defined):

```rust,ignore
use minicbor::{CborLen, Decode, Encode};
use rohi_config::ConfigSection;

#[derive(Encode, Decode, CborLen, ConfigSection)]
#[cbor(array)]
#[config(version = 1)]
pub struct Identity {
    #[n(0)]
    pub secret: Secret,
}

store.set(&identity)?;
let identity = store.get::<Identity>()?; // Option<Identity>
```

A section that users configure is split in two. The typed struct (`WifiConfig`) is the **effective** configuration and
is not persisted; its `Default` is only a portable baseline, the firmware supplies the authoritative defaults. What the
store holds is the user's explicit **overrides**, declared with the declarative macro `config_overrides!`:

```rust,ignore
rohi_config::config_overrides! {
    pub struct WifiOverrides for WifiConfig, version 1 {
        1 => ssid: WifiSsid,
        2 => password: Option<WifiPassword>,
        3 => scan_timeout_ms: u32,
    }
}

store.set(&overrides)?;
let overrides = store.get::<WifiOverrides>()?; // Option<WifiOverrides>
let mut wifi = firmware_defaults();            // the firmware decides the base
overrides.apply_to(&mut wifi);                 // effective = firmware defaults + overrides
```

The macro generates a struct with an `Override<T>` per field (`Override::Default`: the user did not override it,
`Override::Value(v)`: explicit; nullable fields are `Override<Option<T>>`, so `Value(None)` differs from `Default`),
derives the CBOR codec as a **map** keyed by the stable field ids, and implements `ConfigSection` (the section ID is the
hash of the name of the *section type*, `WifiConfig`) and `ConfigOverrides<WifiConfig>` (`apply_to`,
`is_overridden`, `capture`, `clear`, `is_empty()`, `FIELD_IDS`). The generated code only overlays the explicit overrides
on a base value; it never decides the base (`over_default()` is a convenience on `C::default()` for tests). A section
without overrides encodes as the empty map.

`store.remove::<T>()` deletes one section, `store.clear()` all of them. The caller never handles section IDs,
record framing, CRCs, slots or generations.

Section payloads are CBOR. Field numbers, the explicit `#[cbor(array)]` / `#[cbor(map)]` representation, strict decode,
secret handling, section IDs, versioning, the container format and unknown-section preservation are **persistent ABI**
and documented once in [Persistent ABI](../reference/persistent-abi.md). The step by step recipe is
[Add a ConfigSection](../guides/add-config-section.md).

The `#[config(version = N)]` attribute is required for a derived section (`config_overrides!` takes the version in its
header); `id` and `decode_versioned` are optional, see
[Section versioning](../reference/persistent-abi.md#section-versioning).


## Transactions

Every update streams the complete new document into the **inactive** slot; the active slot is never modified:

```text
plan (exact CBOR sizes, nothing written; StorageFull / SectionTooLarge / TooManySections)
 -> clear the inactive slot's commit word (program it to 0), then erase the slot
 -> stream envelope, payload and records: the replaced section is encoded, all others copied byte-for-byte
 -> write CRC
 -> read back and verify the whole slot
 -> write commit word LAST
```

> After interruption at any point during an update, the previous valid configuration or the completely
> committed new configuration remains loadable.

The host tests prove it by failing the storage after every possible number of programmed bytes (torn writes,
including a torn commit word) and erased bytes, then reopening the store; the erase faults include documents that
reach into the sector holding the commit word.

Generation is a `u32` inside `ConfigPayload`. `ConfigStore::open` picks the committed, valid slot with the newest
generation using wrap-safe serial arithmetic, so `u32::MAX -> 0` is handled.

## Open states: empty versus corrupt

`ConfigStore::open` distinguishes a fresh device from damaged configuration:

| Flash                                                            | Result                                  |
|------------------------------------------------------------------|-----------------------------------------|
| Neither slot has a commit word (erased, or first write interrupted before the commit) | `ConfigOpenState::Empty`  |
| Every committed slot validates                                   | `ConfigOpenState::Valid` (newest generation is used) |
| One committed slot is invalid, the other validates               | `ConfigOpenState::Recovered` (the valid slot is used) |
| Committed slot(s) exist but none validates                       | `Err(ConfigError::CorruptStorage(reason))` |

`Recovered` does not say which slot was newer: the generation of the damaged slot cannot be read, so the active
configuration may be older than an update that was lost, or the damage may have hit the older, inactive copy. The
next successful update overwrites the damaged slot. `open_state()` keeps reporting how the flash looked at open;
`state()` is the current health and turns `Valid` after that update (and after the first update of an empty store).

Corruption is never reported as an empty store, so a corrupted `Identity` cannot look like "not provisioned". The `Identity` section is described in [Device identity](identity.md).
Only genuinely erased storage reads as absent. The store also exposes `generation()`, `active_slot()` and
`section_count()` for diagnostics.

### Recovering corrupt storage

`ConfigStore::open` drops the storage on failure. `ConfigStore::try_open` returns an `OpenFailure { storage, error }`
instead, so the caller can decide what to do with the flash. `ConfigStore::format(storage)` is the explicit way out of
`CorruptStorage`: it clears both commit words, erases both slots and returns an empty store, discarding every section
(known, unknown or corrupted). An interrupted format never leaves a committed, half-erased slot: each slot is either
untouched or uncommitted, and running `format` again completes it.

## Errors

| Error                                       | Meaning                                                            |
|---------------------------------------------|--------------------------------------------------------------------|
| `SectionError::Decode(DecodeFailure)`       | Payload is not a valid CBOR encoding of the type                   |
| `SectionError::TrailingBytes`               | Valid value followed by extra bytes                                |
| `SectionError::UnsupportedSectionVersion`   | Stored version unknown to this firmware                            |
| `ConfigError::SectionTooLarge`              | Encoded section above `MAX_SECTION_PAYLOAD`                        |
| `ConfigError::StorageFull`                  | Resulting document does not fit a slot (nothing was written)       |
| `ConfigError::TooManySections`              | More than `MAX_SECTIONS` sections                                  |
| `ConfigError::CorruptStorage(Corruption)`   | Committed configuration unreadable (`Envelope`, `Crc`, `Payload`, `Record`, `DuplicateSection`, `TooManySections`, `UnsupportedFormatVersion`) |
| `ConfigError::Storage(E)`                   | The storage backend failed                                         |

## Storage backends

```rust,ignore
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

`write` follows NOR programming: it may only clear bits. The store writes erased bytes, except that it programs an
already written commit word to zero before erasing its slot, so a backend must allow re-programming a word with a
value that only clears bits.

* `MemoryStorage<N, ERASE>` emulates flash (erase to `0xFF`, writes may only clear bits, `MemoryError::NotErased`
  otherwise) and offers deterministic fault injection: `fail_write_after(bytes)` tears the write at that byte,
  `fail_erase_after(bytes)` leaves a partially erased sector.
* `EspConfigStorage<F: MultiwriteNorFlash>` restricts an `embedded-storage` NOR flash to a
  `FlashRegion { offset, length }`. `MultiwriteNorFlash` is the `embedded-storage` marker for flash that can program
  a word more than once (`esp-storage`'s `FlashStorage` implements it; encrypted flash would not qualify).
  Every offset is region-relative and checked; writes and erases must be word/sector aligned
  (`EspStorageError::InvalidAlignment`), unaligned reads are served through a small bounce buffer. Errors are the
  typed `OutOfBounds`, `InvalidAlignment`, `InvalidRegion`, `ReadFailed`, `WriteFailed`, `EraseFailed`.
  With a chip feature (`esp32c3` or `esp32c6`), `rohi_config_esp::esp::config_storage` builds it on the `esp-storage` driver and the
  `rohi-cfg` partition. It knows nothing about CBOR, sections or any feature's configuration.

## Runtime ownership (Altruist Urban)

The firmware opens the store once at boot and hands it to a **configuration task**, the single logical owner, as
described in [Runtime ownership](../concepts/runtime-ownership.md). Other code (the control layer) uses a cloneable
`ConfigHandle`:

```text
ConfigStore ← config task ← bounded request channel ← ConfigHandle::{get, set, remove, status}
```

Nothing outside `altruist_urban::config` touches the configuration flash and there is no global mutable store. If
the partition is missing or committed configuration is corrupt the firmware keeps running, and every request fails
with `ServiceError::Unavailable` (`status()` tells why) instead of behaving as if the configuration were empty.
`status()` reports the store's current `state()`, so `Recovered` becomes `Valid` once an update repaired the slot.

The owner task also publishes a committed `WifiConfig` or `NetworkConfig` to the Wi-Fi runtime as the desired configuration, in the same
step as the commit and with no `.await` in between (and once at boot from the stored value). Cancelling a
`ConfigHandle::set` or `remove` therefore either happens before the request was queued (nothing changed) or the
operation is logically completed: see [Async and cancellation](../concepts/async-and-cancellation.md).

`ConfigHandle` is section-oriented only: there is deliberately no "erase everything" operation, so ordinary
maintenance such as removing `WifiConfig` never touches unrelated or future protected sections (for example an
identity). The way out of corruption without reflashing is the separate `config::maintenance()` handle:
`StorageMaintenance::format_storage()` formats an unavailable store (corrupt or unreadable, partition present) with
`ConfigStore::format` and the service becomes ready with an empty store. It refuses a ready store
(`ServiceError::StorageHealthy`) and cannot help a missing partition. The factory reset is its own explicit,
privileged operation: `StorageMaintenance::factory_reset()` formats the region whatever its state, so it also removes the
identity, unknown sections and the generation counter, and it repairs corrupt storage. The control protocol exposes it as
`FactoryReset` (`rohi factory-reset`).

Flash erase and programming are synchronous, and `esp-storage` runs them in a critical section with the flash cache
disabled. An update therefore stalls the whole CPU, not just the configuration task (each 4 KiB sector erase takes
tens of milliseconds), and moving the task to another executor would not change that. Configuration updates are
rare and must not be issued from timing-critical paths.

## Flash layout (Altruist Urban, ESP32-C3 4 MB / ESP32-C6 8 MB)

The single source of truth is the per-chip `firmwares/altruist-urban/partitions-esp32c3.csv` (4 MB flash) and
`partitions-esp32c6.csv` (8 MB flash), mirrored by the constants in `rohi_config_esp::layout` (a host test keeps both in
sync). Both tables differ only in the size of the `factory` partition (`0x3E0000` on ESP32-C3, `0x7E0000` on ESP32-C6):

```text
0x09000  nvs        24 KiB
0x0f000  phy_init    4 KiB
0x10000  rohi-cfg   16 KiB   <- configuration region
           ├─ slot A  8 KiB (2 sectors)
           └─ slot B  8 KiB (2 sectors)
0x20000  factory    app (3.9 MB on ESP32-C3, 7.9 MB on ESP32-C6)
```

`cargo run` passes the table of the selected target to `espflash` (see `.cargo/config.toml`); run it from the repository root. If the
partition is missing the firmware logs an error and keeps running the sensors without persistent configuration.

## Resource baseline

Flash region, slot, payload and stack figures of the store are listed with all other limits in
[Resource limits](../reference/resource-limits.md#configuration-store). Commit streams CBOR straight into flash and
copies unrelated records in 64 byte chunks, so there is never a second copy of the configuration in RAM, and no heap or
dynamic CBOR tree is used.

## Hardware self-test

`examples/src/bin/config.rs` verifies persistence on a real ESP32-C3 or ESP32-C6. Flash it and reset or power-cycle the board
repeatedly:

```bash
cargo run --release -p rohi-examples --features esp32c3 --target riscv32imc-unknown-none-elf --bin example-config
```

Every boot checks what the previous boot stored (typed write + reboot + read, several sections, replacement
preserving unrelated sections, repeated write cycles) and logs `SELFTEST PASS` or `SELFTEST FAIL`. On the third
boot it corrupts the newest slot on purpose and checks that the store reports `Recovered` and falls back to the
previous generation.

## Not in this layer

Device identity is a section of this store and is described in [Device identity](identity.md); BLE/Wi-Fi control, OTA, flash encryption and JSON configuration are out of scope. The
format is CBOR only; there is no migration from the earlier development-only binary header format.
