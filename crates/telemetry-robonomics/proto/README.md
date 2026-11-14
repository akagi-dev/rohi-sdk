# Connectivity Protocol

## Goals

- One shared structure for Altruist firmware, connectivity layer and sensors.social UI.
- **Protobuf** on the wire (MCU-friendly; high performance, no JSON canonicalization).

## Data Flow Architecture

```
┌────────────┐                ┌─────────────────┐              ┌─────────────────┐
│  IoT       │   Signed       │  Connectivity   │   Signed     │  Backend/Map    │
│  Sensor    │──────────────> │  Layer          │────────────> │  Infrastructure │
│  Device    │   Envelope     │  (Pass-through) │   Envelope   │                 │
└────────────┘                └─────────────────┘              └─────────────────┘
     │                              │                                  │
     │ Measures                     │ Validates                        │ Decodes
     │ Serializes                   │ Signature                        │ Processes
     │ Signs                        │ Forwards                         │ Stores
     └──────────────────────────────┴──────────────────────────────────┘
```

## Message Anatomy

```
┌────────────────────────────────────────────────────────────────────┐
│                         TELEMETRY MESSAGE                          │
├────────────────────────────────────────────────────────────────────┤
│                                                                    │
│  Metadata                                                          │
│  ┌────────────────────────────────────────────┐                    │
│  │ node_id: 42                                │                    │
│  │ timestamp: 1723537200000                   │                    │
│  └────────────────────────────────────────────┘                    │
│                                                                    │
│  Payload (choose one: Urban OR Insight)                            │
│  ┌─────────────────────────────────────────────────────────────┐   │
│  │                                                             │   │
│  │  Public Data (visible to everyone)                          │   │
│  │  ┌────────────┐ ┌────────────┐ ┌────────────┐               │   │
│  │  │ Temp: 21°C │ │ PM2.5: 8.1 │ │ Location   │  ...          │   │
│  │  └────────────┘ └────────────┘ └────────────┘               │   │
│  │                                                             │   │
│  │  Private Data (encrypted for specific recipients)           │   │
│  │  ┌──────────────────────────────────────────┐               │   │
│  │  │ 🔒 Encrypted for City Authority          │               │   │
│  │  │    Contains: Noise levels, detailed GPS  │               │   │
│  │  └──────────────────────────────────────────┘               │   │
│  │  ┌──────────────────────────────────────────┐               │   │
│  │  │ 🔒 Encrypted for Device Owner            │               │   │
│  │  │    Contains: All sensor readings         │               │   │
│  │  └──────────────────────────────────────────┘               │   │
│  │                                                             │   │
│  └─────────────────────────────────────────────────────────────┘   │
│                                                                    │
└────────────────────────────────────────────────────────────────────┘
```

## Versioning

Breaking schema changes go in a new folder/package (`v2/`, `v3/`, …).  
There is no `schema_version` field inside `Message` — the path / `package sensors.social.v1` is the version.

## Signed Envelope

Telemetry message wrapped in a `SignedEnvelope` that provides:
- **`sensor_id`** — Unique device identifier (persistent across restarts)
- **`nonce`** — Random value to prevent replay attacks (e.g., UUID v4)
- **`message`** — The actual measurements data, including `Meta.timestamp` (UTC, ms)
- **`signature`** — Ed25519 signature

### Signature & Byte Integrity

**The signature is computed over the exact bytes of the envelope fields.**

    singature = sing(**sensor_id** <> **nonce** <> **message**)

**DO NOT** re-encode envelope. Protobuf serialization is **not deterministic** — re-encoding may produces different bytes, which:
1. **Breaks the signature** (validation will fail)
2. **Changes the CID**

#### Quick Implementation Rules

| Layer | Operation | Safe? |
|-------|-----------|-------|
| **Device** | Serialize once, sign, transmit | ✅ |
| **Connectivity** | Verify signature, forward envelope | ✅ |
| **Backend** | Deserialize for indexing, store original bytes | ✅ |
| **Any** | Decode and re-encode | ❌ **May breaks signature & CID** |

## Encryption

Selective data sharing is supported through public and private measurement sections.

### Privacy Models

```
┌─────────────────────────────────────────────────────────────────────┐
│  MODEL 1: Fully Public                                              │
├─────────────────────────────────────────────────────────────────────┤
│                                                                     │
│  Public: [Temp, PM2.5, Location]        🌍 Everyone can see         │
│  Private: [ ]                                                       │
│                                                                     │
└─────────────────────────────────────────────────────────────────────┘

┌─────────────────────────────────────────────────────────────────────┐
│  MODEL 2: Fully Private                                             │
├─────────────────────────────────────────────────────────────────────┤
│                                                                     │
│  Public: [ ]                                                        │
│  Private: [🔒 Encrypted(All sensors)]  🔐 Only owner can see        │
│                                                                     │
└─────────────────────────────────────────────────────────────────────┘

┌─────────────────────────────────────────────────────────────────────┐
│  MODEL 3: Mixed (Most Common)                                       │
├─────────────────────────────────────────────────────────────────────┤
│                                                                     │
│  Public: [Temp, PM2.5, Approx Location]  🌍 Everyone can see        │
│  Private: [                                                         │
│    🔒 Encrypted(Precise GPS, Noise) → City Authority                │
│    🔒 Encrypted(All sensors)        → Device Owner                  │
│  ]                                                                  │
│                                                                     │
└─────────────────────────────────────────────────────────────────────┘
```

### Data Sharing Model

Sensors can implement flexible privacy by:
1. **Fully public**: Only populate the `public` array with Urban/Insight messages, leave `private` empty
2. **Fully private**: Leave `public` empty, put all measurements in `private` sections (ciphertext contains serialized EncryptedUrban/EncryptedInsight)
3. **Mixed sharing**: Share basic metrics publicly, detailed metrics in private sections
4. **Multiple recipients**: Create separate private sections for different users/groups

### Encryption Process

```
┌─────────────────────────────────────────────────────────────────────┐
│              How Private Data is Encrypted                          │
├─────────────────────────────────────────────────────────────────────┤
│                                                                     │
│  Step 1: Prepare Measurements                                       │
│  ┌────────────────────────────────────────────┐                     │
│  │ Urban: [Noise: 85dB, GPS: (51.5, -0.1)]    │                     │
│  └────────────────────────────────────────────┘                     │
│                    ▼                                                │
│  Step 2: Wrap in EncryptedUrban                                     │
│  ┌────────────────────────────────────────────┐                     │
│  │ EncryptedUrban {                           │                     │
│  │   sensors: [UrbanSensor, UrbanSensor]      │                     │
│  │ }                                          │                     │
│  └────────────────────────────────────────────┘                     │
│                    ▼                                                │
│  Step 3: Serialize to Binary                                        │
│  ┌────────────────────────────────────────────┐                     │
│  │ [0x0A, 0x12, 0x08, 0x15, ...]              │  Protobuf bytes     │
│  └────────────────────────────────────────────┘                     │
│                    ▼                                                │
│  Step 4: Key Agreement (ECDH)                                       │
│  ┌──────────────────┐     ┌──────────────────┐                      │
│  │ Sensor Private   │  +  │ Recipient Public │  → Shared Secret     │
│  │ Key              │     │ Key              │                      │
│  └──────────────────┘     └──────────────────┘                      │
│                    ▼                                                │
│  Step 5: Encrypt with AEAD (XChaCha20-Poly1305)                     │
│  ┌────────────────────────────────────────────┐                     │
│  │ Ciphertext + Auth Tag                      │  🔒 Encrypted       │
│  └────────────────────────────────────────────┘                     │
│                    ▼                                                │
│  Step 6: Add to Private Section                                     │
│  ┌────────────────────────────────────────────┐                     │
│  │ Encrypted {                                │                     │
│  │   version: 1                               │                     │
│  │   algorithm: "xchacha20"                   │                     │
│  │   from: <sensor_pubkey>                    │                     │
│  │   nonce: <random_24_bytes>                 │                     │
│  │   ciphertext: <encrypted_data>             │                     │
│  │ }                                          │                     │
│  └────────────────────────────────────────────┘                     │
│                                                                     │
└─────────────────────────────────────────────────────────────────────┘
```

1. **Create wrapper**: Place Urban/Insight measurements in EncryptedUrban/EncryptedInsight wrapper
2. **Serialize**: Convert wrapper to protobuf binary format
3. **Key Agreement**: ECDH between sender's private key and recipient's public key
4. **Key Derivation**: HKDF-SHA256 to derive encryption key from shared secret
5. **AEAD Encryption**: Encrypt serialized protobuf with chosen algorithm
6. **Authentication**: 16-byte Poly1305 or GCM tag appended to ciphertext

### Supported Algorithms

```
  AES-GCM-256 (Hardware)  ████████████████████████████  2-3 GB/s
  XChaCha20-Poly1305      ██████                        ~600 MB/s
  
  ✅ Use AES-GCM on devices with hardware acceleration
  ✅ Use XChaCha20 on MCUs / mobile devices
```

### Access Revocation

Revocation only applies going forward.

A private section, once published, is stored in IPFS and its hash is anchored in the
chain. A recipient who held the key keeps everything they were able to decrypt up to the
moment access was taken back. Rotating keys or dropping a recipient from future messages
withdraws access to what comes next — not to what has already been released.

Applications built on this format should present the distinction to the user, rather than
implying that sharing can be undone.

## Partial Decoding

Protobuf supports **zero-copy pass-through** at the connectivity layer:

```
┌─────────────────────────────────────────────────────────────────────┐
│                   Layer Responsibilities                            │
├─────────────────────────────────────────────────────────────────────┤
│                                                                     │
│  FIRMWARE (Creates & Signs)                                         │
│  ┌────────────────────────────────────────────┐                     │
│  │ 1. Measure sensors                         │                     │
│  │ 2. Create Message                          │                     │
│  │ 3. Serialize to bytes                      │                     │
│  │ 4. Wrap in SignedEnvelope                  │                     │
│  │ 5. Sign with Ed25519                       │                     │
│  └────────────────────────────────────────────┘                     │
│                         │                                           │
│                         ▼                                           │
│  CONNECTIVITY LAYER (Pass-through)                                  │
│  ┌────────────────────────────────────────────┐                     │
│  │ 1. Verify Ed25519 signature    ✓           │                     │
│  │ 2. Forward envelope            →           │  (No parsing!)      │
│  │                                            │                     │
│  │ ❌ Does NOT decode Message                 │                     │
│  │ ❌ Does NOT parse measurements             │                     │
│  └────────────────────────────────────────────┘                     │
│                         │                                           │
│                         ▼                                           │
│  BACKEND/MAP (Full Processing)                                      │
│  ┌────────────────────────────────────────────┐                     │
│  │ 1. Deserialize envelope and message        │                     │
│  │ 2. Extract metadata                        │                     │
│  │ 3. Process public measurements             │                     │
│  │ 4. Try decrypt private sections            │                     │
│  │ 5. Store in database                       │                     │
│  │ 6. Update map visualization                │                     │
│  └────────────────────────────────────────────┘                     │
│                                                                     │
└─────────────────────────────────────────────────────────────────────┘
```

## Blockchain Anchoring

Beyond signed envelope delivery through connectivity layers, measurements can be anchored on-chain for **immutable timestamping** and **verifiable data provenance**.

### Delivery Methods Comparison

```
┌─────────────────────────────────────────────────────────────────────┐
│                    ENVELOPE DELIVERY METHODS                        │
├─────────────────────────────────────────────────────────────────────┤
│                                                                     │
│  HTTP/MQTT (Standard)          Blockchain (Anchored)                │
│  ════════════════════          ═════════════════════                │
│                                                                     │
│  ┌──────────┐                  ┌──────────┐                         │
│  │  Sensor  │                  │  Sensor  │                         │
│  └────┬─────┘                  └────┬─────┘                         │
│       │ SignedEnvelope              │ SignedEnvelope                │
│       ▼                             ▼                               │
│  ┌──────────┐                  ┌──────────┐                         │
│  │  HTTP    │                  │ Send     │─────┐                   │
│  │ Gateway  │                  │ Extrinsic│     │                   │
│  └────┬─────┘                  └──────────┘     │                   │
│       │                                         │                   │
│       ▼                    OR                   │                   │
│  ┌──────────┐                                   │                   │
│  │ Backend  │                  ┌──────────┐     │                   │
│  │ Database │                  │ Batch    │     │                   │
│  └──────────┘                  │ to IPFS  │─────┤                   │
│                                └──────────┘     │                   │
│  ✓ Fast                                         │                   │
│  ✓ Cheap                                        ▼                   │
│  ✗ Mutable                     ┌────────────────────┐               │
│  ✗ No proof                    │   📦 Blockchain    │               │
│                                │   (Robonomics)     │               │
│                                └────────────────────┘               │
│                                                                     │
│                                ✓ Immutable timestamp                │
│                                ✓ Proof of existence                 │
│                                ✓ Verifiable integrity               │
│                                ✗ Higher cost                        │
│                                                                     │
└─────────────────────────────────────────────────────────────────────┘
```

#### Method 1: Direct Anchoring

**Device pushes serialized envelope directly to blockchain**

```
┌─────────────────────────────────────────────────────────────────────┐
│                                                                     │
│   📡 IoT Device                                                     │
│   ┌───────────────────────────────────────────────────────────┐     │
│   │                                                           │     │
│   │  1️⃣  Measure                                              │     │
│   │     ┌─────────┐                                           │     │
│   │     │ Temp    │  21.5°C                                   │     │
│   │     │ PM2.5   │  8.1 µg/m³                                │     │
│   │     │ GPS     │  (51.5, -0.1)                             │     │
│   │     └─────────┘                                           │     │
│   │          │                                                │     │
│   │          ▼                                                │     │
│   │  2️⃣  Create SignedEnvelope                                │     │
│   │     ┌──────────────────────────────────┐                  │     │
│   │     │ sensor_id:  0x4c7f...            │                  │     │
│   │     │ nonce:      0xf3a9...            │                  │     │
│   │     │ message:    [proto bytes,        │                  │     │
│   │     │              incl. timestamp]    │                  │     │
│   │     │ signature:  0x8b2d... (64 bytes) │                  │     │
│   │     └──────────────────────────────────┘                  │     │
│   │          │                                                │     │
│   │          ▼                                                │     │
│   │  3️⃣  Serialize to bytes                                   │     │
│   │     [0x0a, 0x20, 0x4c, 0x7f, ...]  (~200 bytes)           │     │
│   │          │                                                │     │
│   └──────────┼────────────────────────────────────────────────┘     │
│              │                                                      │
│              │ Submit Extrinsic                                     │
│              ▼                                                      │
│   ⛓️  Robonomics Blockchain                                         │
│   ┌───────────────────────────────────────────────────────────┐     │
│   │                                                           │     │
│   │  pallet_cps::set_payload(                                 │     │
│   │    origin: Signed(device_account),                        │     │
│   │    payload: [0x0a, 0x20, 0x4c, ...]  ← Full envelope      │     │
│   │  )                                                        │     │
│   │                                                           │     │
│   │  ┌─────────────────────────────────────────────────┐      │     │
│   │  │  Block #2847593                                 │      │     │
│   │  │  ┌───────────────────────────────────────────┐  │      │     │
│   │  │  │ Extrinsic: CPS.set_payload                │  │      │     │
│   │  │  │ Account: 4CvP46mxFm54eBb...               │  │      │     │
│   │  │  │ Payload: [0x0a, 0x20, 0x4c, ...] 200 B    │  │      │     │
│   │  │  │ Timestamp: 2026-08-13 09:36:00 UTC        │  │      │     │
│   │  │  └───────────────────────────────────────────┘  │      │     │
│   │  └─────────────────────────────────────────────────┘      │     │
│   │                                                           │     │
│   │  ✅ Stored on-chain forever                               │     │
│   │  ✅ Immutable timestamp                                   │     │
│   │  ✅ Immediate finality                                    │     │
│   │                                                           │     │
│   └───────────────────────────────────────────────────────────┘     │
│                                                                     │
│   💰 Cost: ~0.001 XRT per envelope (~200 bytes storage)             │
│   ⏱️  Latency: 6-12 seconds                                         │
│                                                                     │
└─────────────────────────────────────────────────────────────────────┘
```

#### Method 2: Batch Anchoring

**Service collects envelopes, compresses, stores in IPFS, anchors CID**

```
┌─────────────────────────────────────────────────────────────────────┐
│                                                                     │
│   📡 Multiple Devices                                               │
│   ┌──────────┐  ┌──────────┐  ┌──────────┐                          │
│   │ Sensor 1 │  │ Sensor 2 │  │ Sensor N │                          │
│   └────┬─────┘  └────┬─────┘  └────┬─────┘                          │
│        │ Envelope₁   │ Envelope₂   │ EnvelopeN                      │
│        └─────────────┴─────────────┘                                │
│                      │                                              │
│                      ▼                                              │
│   🌐 Connectivity Service                                           │
│   ┌───────────────────────────────────────────────────────────┐     │
│   │                                                           │     │
│   │  1️⃣  Collect envelopes (e.g., 100 messages)               │     │
│   │     ┌─────┐ ┌─────┐       ┌─────┐                         │     │
│   │     │ E₁  │ │ E₂  │  ...  │ E₁₀₀│                         │     │
│   │     └─────┘ └─────┘       └─────┘                         │     │
│   │          │                                                │     │
│   │          ▼                                                │     │
│   │  2️⃣  Create SignedEnvelopeBatch                           │     │
│   │     ┌──────────────────────────────────┐                  │     │
│   │     │ batch: [                         │                  │     │
│   │     │   SignedEnvelope { ... },        │                  │     │
│   │     │   SignedEnvelope { ... },        │                  │     │
│   │     │   ...                            │                  │     │
│   │     │   SignedEnvelope { ... }         │                  │     │
│   │     │ ]                                │                  │     │
│   │     └──────────────────────────────────┘                  │     │
│   │          │                                                │     │
│   │          ▼                                                │     │
│   │  3️⃣  Serialize to protobuf                                │     │
│   │     [0x0a, 0xc8, 0x01, ...]  (~20 KB for 100)             │     │
│   │          │                                                │     │
│   │          ▼                                                │     │
│   │  4️⃣  Compress with zlib                                   │     │
│   │     [0x78, 0x9c, 0xed, ...]  (~8 KB, 60% reduction)       │     │
│   │          │                                                │     │
│   └──────────┼────────────────────────────────────────────────┘     │
│              │ Store                                                │
│              ▼                                                      │
│   📦 IPFS Storage                                                   │
│   ┌───────────────────────────────────────────────────────────┐     │
│   │                                                           │     │
│   │  ipfs.add(compressed_batch)                               │     │
│   │                                                           │     │
│   │  ┌─────────────────────────────────────────────────┐      │     │
│   │  │  CID: bafybeigdyrzt5sfp7udm7hu76uh7y26nf...     │      │     │
│   │  │                                                 │      │     │
│   │  │  Content: [0x78, 0x9c, 0xed, ...] (8 KB)        │      │     │
│   │  │                                                 │      │     │
│   │  │  Contains: 100 signed envelopes                 │      │     │
│   │  └─────────────────────────────────────────────────┘      │     │
│   │                                                           │     │
│   └───────────────────────────────────────────────────────────┘     │
│              │ Return CID                                           │
│              ▼                                                      │
│   ⛓️  Robonomics Blockchain                                         │
│   ┌───────────────────────────────────────────────────────────┐     │
│   │                                                           │     │
│   │  pallet_cps::set_payload(                                 │     │
│   │    origin: Signed(service_account),                       │     │
│   │    payload: b"bafybeigdyrzt5sfp7udm7hu..."  ← Just CID    │     │
│   │  )                                                        │     │
│   │                                                           │     │
│   │  ┌─────────────────────────────────────────────────┐      │     │
│   │  │  Block #2847610                                 │      │     │
│   │  │  ┌───────────────────────────────────────────┐  │      │     │
│   │  │  │ Extrinsic: CPS.set_payload                │  │      │     │
│   │  │  │ Account: ConnectivityService              │  │      │     │
│   │  │  │ Payload: bafybeigdyrzt... (59 B)          │  │      │     │
│   │  │  │ Batch Size: 100 envelopes                 │  │      │     │
│   │  │  └───────────────────────────────────────────┘  │      │     │
│   │  └─────────────────────────────────────────────────┘      │     │
│   │                                                           │     │
│   │  ✅ Only CID stored on-chain                              │     │
│   │  ✅ Batch proof for 100 measurements                      │     │
│   │  ✅ Cost amortized across batch                           │     │
│   │                                                           │     │
│   └───────────────────────────────────────────────────────────┘     │
│                                                                     │
│   💰 Cost: ~0.00001 XRT per envelope (100x cheaper)                 │
│   ⏱️  Latency: 30-60 seconds (batch interval)                       │
│   💾 Storage: IPFS (distributed, needs pinning)                     │
│                                                                     │
└─────────────────────────────────────────────────────────────────────┘
```

**Recommendation**:
- **Direct Anchoring** → Time-critical, high-value measurements requiring guaranteed permanent storage
- **Batch Anchoring** → Routine telemetry at scale where cost efficiency and throughput matter most
