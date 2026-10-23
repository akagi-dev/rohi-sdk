# Persistent ABI

Everything the SDK writes to non-volatile storage is a **persistent ABI**: a firmware update must keep reading what
an older firmware stored, and an older firmware must not destroy what a newer one stored. This page is the single
reference for those rules. How to *use* the configuration store is described in [Configuration store](../sdk/config.md);
step by step instructions for a new section are in [Add a ConfigSection](../guides/add-config-section.md).

What is covered:

| Topic | Section |
|-------|---------|
| Field numbers, explicit CBOR representation, strict decode, secrets | [Section payload](#section-payload) |
| Section versioning and migration | [Section versioning](#section-versioning) |
| Section ID derivation and rename implications | [Section ID ABI](#section-id-abi) |
| Container, slots, CRC and commit word | [Container format](#container-format) |
| Unknown-section preservation | [Unknown sections](#unknown-sections) |
| Golden tests | [Golden tests](#golden-tests) |

## Section payload

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

## Section versioning

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


## Section ID ABI

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

```rust,ignore
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

## Unknown sections

Sections whose ID the running firmware does not know are never decoded; their `SectionRecord` bytes are copied
unchanged on every update, so older firmware preserves a newer firmware's sections. Updating one known section
never alters the encoded bytes of unrelated records. They can be listed (`for_each_section`), read raw
(`read_raw`) and explicitly removed (`remove_raw`).


## Golden tests

Persistent encodings are pinned by golden tests, so an accidental change of a field number, representation or ID
fails the test suite instead of silently orphaning stored data:

- `rohi-config` tests pin the container (envelope, payload, record bytes, CRC) and the section ID hash values listed
  above.
- Every production section pins its encoded bytes; `WifiConfig` in `rohi-net` is the reference example.
- A new section must add its own golden test before it is persisted by a release, see
  [Add a ConfigSection](../guides/add-config-section.md).

Changing a golden value is an ABI break and needs an explicit version bump and, where old data must stay readable, a
migration hook.
