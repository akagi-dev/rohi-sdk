# Configuration store

`rohi-config` is a hardware independent, transactional configuration store. Configuration is a set of
**independent typed sections**; there is no central `DeviceConfig`. A feature crate defines its own section and
the store persists it without knowing anything about it.

```text
feature crate
    ↓
typed ConfigSection ── #[derive(ConfigSection)] ── section ID / version
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
| `rohi-config`         | `ConfigSection`, `ConfigStorage`, `ConfigStore`, CBOR slot format, `MemoryStorage` |
| `rohi-config-derive`  | `#[derive(ConfigSection)]`, section IDs from struct names                          |
| `rohi-config-esp`     | ESP32-C3 / ESP32-C6 flash backend, bounded flash region, partition lookup          |

Each feature crate owns its own schema (for example `rohi-net` owns `WifiConfig`); `rohi-config` never learns
the set of application sections.

## Defining a section

```rust
use minicbor::{CborLen, Decode, Encode};
use rohi_config::ConfigSection;

#[derive(Encode, Decode, CborLen, ConfigSection)]
#[config(version = 1)]
pub struct WifiConfig {
    #[n(0)]
    pub ssid: WifiSsid,
    #[n(1)]
    pub password: Option<WifiPassword>,
}

store.set(&wifi)?;
let wifi = store.get::<WifiConfig>()?; // Option<WifiConfig>
```

`store.remove::<T>()` deletes one section, `store.clear()` all of them. The caller never handles section IDs,
record framing, CRCs, slots or generations.

> **Codec.** Section payloads are CBOR encoded with [`minicbor`](https://docs.rs/minicbor). Types derive
> `Encode`, `Decode` and `CborLen`. Field numbers are explicit (`#[n(0)]`, `#[n(1)]`...) and are the persistent
> schema: never reuse or renumber a field, only append new ones. There is no per-section size attribute: the
> encoded size is calculated from the actual value (`minicbor::len`) and checked against `MAX_SECTION_PAYLOAD`
> (1024 bytes) and the slot capacity before anything is written. Prefer bounded types (fixed arrays, bounded
> byte strings) over unbounded ones; the store itself never allocates.
>
> **Explicit representation.** Every production `ConfigSection` must declare its CBOR representation with
> `#[cbor(array)]` or `#[cbor(map)]` instead of relying on the `minicbor` derive default, and pin the encoded bytes
> with a golden test (see `WifiConfig` in `rohi-net`). The encoding is persistent ABI.
>
> **Strict decode.** `get` requires the decoder to consume the whole payload. A valid value followed by extra
> bytes fails with `SectionError::TrailingBytes`, never with an unrelated error.
>
> **Secrets.** Do not derive `Debug` for secret-bearing sections (Wi-Fi passwords, private keys); implement it by
> hand with redaction so secrets never reach logs or panic output.

### `#[config(...)]` attributes

| Attribute                        | Meaning                                                                         |
|----------------------------------|---------------------------------------------------------------------------------|
| `version = N`                    | **Required.** Schema version of this section (`u16`).                           |
| `id = 0xAABB…`                   | Explicit 64-bit ID, replacing the one derived from the struct name.             |
| `decode_versioned = "path::fn"`  | Migration hook `fn(version: u16, payload: &[u8]) -> Result<Self, SectionError>`. |

There is no `name` attribute. There are no global migrations: each section versions itself, and the default
decoder accepts only `stored version == T::VERSION` (`SectionError::UnsupportedSectionVersion` otherwise). CBOR's
tolerance of unknown fields never replaces this explicit version policy. A migration hook can use
`rohi_config::decode_exact` to decode older payload layouts strictly.

### Section ID ABI

`SectionId(u64)` is the **FNV-1a 64-bit** hash (offset basis `0xcbf29ce484222325`, prime `0x100000001b3`) of the
UTF-8 bytes of the Rust **struct name**. The module path and crate name are not hashed, so moving a type between
modules does not change its identity. The algorithm is part of the persistent storage ABI and must never change.
Golden values:

| Struct name          | `SectionId`            |
|----------------------|------------------------|
| `Identity`           | `0x94d63573d312fe9b`   |
| `WifiConfig`         | `0x335246ad20248b26`   |
| `ConnectivityConfig` | `0x9c57015a40662d0c`   |

> **Renaming a persisted `ConfigSection` type is a storage ABI change.** `WifiConfig` renamed to `WirelessConfig`
> gets a different ID and the stored data is no longer found.

A raw identifier (`r#Type`) hashes as `Type`. Types with type or const parameters are rejected unless they give an
explicit `id`: the name-derived ID would be shared by every instantiation (`Foo<A>`, `Foo<B>`).

The explicit `id` is the escape hatch for the cases where the ID must stay fixed or be chosen:

```rust
#[derive(Encode, Decode, CborLen, ConfigSection)]
#[config(id = 0x335246ad20248b26, version = 2)] // formerly named `WifiConfig`
pub struct WirelessConfig { /* ... */ }
```

Use it only to keep an existing ID across a rename, to resolve a (very unlikely) hash collision or to import a
legacy ID.

## Container format

Everything inside a slot is CBOR. The region is split into two equal slots, **A** and **B**.

```text
slot  = SlotEnvelope | 0xFF padding | commit word (last 4 bytes of the slot)

SlotEnvelope  = { 0: bstr(ConfigPayload), 1: crc32 }
ConfigPayload = { 0: format_version, 1: generation, 2: [ bstr(SectionRecord)* ] }
SectionRecord = [ section_id (u64), section_version (u16), bstr(typed section CBOR) ]
```

`ConfigPayload` is stored as an opaque byte string inside the envelope, so the **CRC-32 (IEEE) covers the exact
encoded payload bytes** and validation never needs to decode and re-encode anything. Likewise every
`SectionRecord` is an opaque byte string, which lets the store copy records it does not understand verbatim.
Fields are written in the order shown, with minimal-length CBOR heads, so the encoder is deterministic. CRC-32
detects accidental corruption; it is not authentication.

Golden example (`Golden { value: 42, flag: true }` with explicit ID `0x1122334455667788`, generation 1):

```text
typed section   82 18 2A F5                                   [42, true]
SectionRecord   83 1B 1122334455667788 01 44 82182AF5
ConfigPayload   A3 00 01 01 01 02 81 50 <record, 16 B>
SlotEnvelope    A2 00 58 18 <payload, 24 B> 01 1A E47B3143
```

### Commit word

The only non-CBOR bytes are a single 4-byte word (`"COMT"`) in the **last word of each slot**. It is flash
transaction metadata, not schema: it holds no fields, lives outside the CBOR document and is written last, which is
what makes NOR flash programming (one-way, word by word) atomic from the store's point of view. A slot without the
commit word is ignored.

Before a committed slot is erased its commit word is programmed to `0x00000000`. NOR programming only clears bits, so
this needs no erase, and the slot is uncommitted *before* any cell of it is erased. Flash erases are not atomic and
make no promise about the order in which cells are cleared, so erasing alone could leave a commit word in front of
a half-erased document.

### Limits

`MAX_SECTIONS = 16`, `MAX_SECTION_PAYLOAD = 1024` bytes (typed payload), slot size at most `MAX_SLOT_SIZE = 64 KiB`
(half of the region, rounded down to the erase size). All length arithmetic is checked. A slot is valid only if the
commit word, the envelope, the CRC, the payload structure and all records check out, the format version is
supported, there are at most `MAX_SECTIONS` records and **no duplicate section ID** (a duplicate is an error, never
first-wins or last-wins).

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

### Unknown sections

Sections whose ID the running firmware does not know are never decoded; their `SectionRecord` bytes are copied
unchanged on every update, so older firmware preserves a newer firmware's sections. Updating one known section
never alters the encoded bytes of unrelated records. They can be listed (`for_each_section`), read raw
(`read_raw`) and explicitly removed (`remove_raw`).

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

Corruption is never reported as an empty store, so a corrupted `Identity` cannot look like "not provisioned".
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

The firmware opens the store once at boot and hands it to a **configuration task**, the single logical owner. Other
code (Wi-Fi at boot, the future control layer) uses a cloneable `ConfigHandle`:

```text
ConfigStore ← config task ← bounded request channel ← ConfigHandle::{get, set, remove, status}
```

Nothing outside `altruist_urban::config` touches the configuration flash and there is no global mutable store. If
the partition is missing or committed configuration is corrupt the firmware keeps running, and every request fails
with `ServiceError::Unavailable` (`status()` tells why) instead of behaving as if the configuration were empty.
`status()` reports the store's current `state()`, so `Recovered` becomes `Valid` once an update repaired the slot.

`ConfigHandle` is section-oriented only: there is deliberately no "erase everything" operation, so ordinary
maintenance such as removing `WifiConfig` never touches unrelated or future protected sections (for example an
identity). The way out of corruption without reflashing is the separate `config::maintenance()` handle:
`StorageMaintenance::format_storage()` formats an unavailable store (corrupt or unreadable, partition present) with
`ConfigStore::format` and the service becomes ready with an empty store. It refuses a ready store
(`ServiceError::StorageHealthy`) and cannot help a missing partition. A full factory reset, if it ever exists, must
be its own explicit, privileged operation.

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

| Item                               | Size                                                            |
|------------------------------------|-----------------------------------------------------------------|
| Configuration flash region         | 16 KiB (`rohi-cfg`)                                             |
| Slot size                          | 8 KiB (commit word included)                                    |
| Envelope / payload overhead        | about 10 B + 8 B plus 2-4 B per record                          |
| Maximum section payload            | 1 024 B                                                         |
| Maximum sections                   | 16                                                              |
| `ConfigStore` RAM                  | a few words of state                                            |
| `get` stack workspace              | 1 KiB (one `MAX_SECTION_PAYLOAD` buffer, only during the call)  |
| Commit stack workspace             | < 0.5 KiB (64 B write buffer, 64 B copy chunk, 128 B ID set)     |

Commit streams CBOR straight into flash and copies unrelated records in 64 byte chunks, so there is never a second
copy of the configuration in RAM, and no heap or dynamic CBOR tree is used.

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

Device identity (WP4.1), BLE/Wi-Fi control, OTA, flash encryption and JSON configuration are out of scope. The
format is CBOR only; there is no migration from the earlier development-only binary header format.
