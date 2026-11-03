# Errors

Public errors are small typed enums in ROHI terms. Platform crates map every vendor error into them; raw vendor errors
never reach higher layers. Messages never contain secrets.

| Domain | Type | Where | Variants / meaning |
|--------|------|-------|--------------------|
| Wi-Fi | `WifiError` | `rohi-net` | `NotConfigured`, `Busy` (command queue full / scan in flight), `Timeout`, `ScanFailed`, `NetworkNotFound`, `AuthenticationFailed`, `AssociationFailed`, `DhcpFailed`, `Disconnected`, `InvalidConfig`, `DriverError` |
| IP data plane | `NetError` | `rohi-net` | `NetworkUnavailable`, `DnsFailed`, `ConnectFailed`, `NoSocketAvailable`, `ConnectionReset`, `Timeout`, `Backend`; implements `embedded_io_async::Error` |
| BLE | `BleError` | `rohi-net` | `Disconnected` (returned once per connection end), `BufferTooSmall`, `Busy`, `DriverError` |
| Wireless runtime | `WirelessError` | `rohi-net-esp` | `AlreadyTaken`, `BleInit` |
| Configuration store | `ConfigError<E>`, `SectionError` | `rohi-config` | storage, codec and section failures; see the [`rohi-config` rustdoc](https://docs.rs/rohi-config) |
| Identity | `IdentityError` | `rohi-identity` | `NotProvisioned`, `AlreadyProvisioned`, `InvalidSecret`, `InvalidPublicKey`, `InvalidSignature` |
| Configuration service | `ConfigError`, `StoreError` | `rohi-service` | `ConfigError`: `UnknownSection`, `Field(FieldError)`, `Unavailable`, `Internal`; `StoreError`: `Unavailable`, `Internal` |
| Actions and status | `ActionError`, `StatusError` | `rohi-service` | `ActionError`: `UnknownService`, `UnknownAction`, `InvalidArgument`, `Busy`, `Unavailable`, `Rejected(reason)`, `Internal`; `StatusError`: `UnknownService` |
| Measurements | `MeasurementServiceError` | `rohi-hal` | `Busy` (only `try_snapshot`) |
| Sensors | `SensorErrorCode`, `DriverError` | `rohi-hal` | domain error codes reported in `SensorEvent::Error` |
| Control protocol | `ErrorCode` | `rohi-control-proto` | `INVALID_ARGUMENT`, `NOT_FOUND`, `NOT_SUPPORTED`, `BUSY`, `UNAVAILABLE`, `PERMISSION_DENIED`, `INTERNAL`, `REJECTED` (with a stable `reason`) |
| Framing | `FrameError` | `rohi-control-ble` | `Truncated`, `Version`, `InvalidIndex`, `TooLarge`, `OutOfOrder`, `Inconsistent`, `LengthMismatch`, `PayloadTooSmall` |
| Flash backend | `EspStorageError` | `rohi-config-esp` | `OutOfBounds`, `InvalidAlignment`, `InvalidRegion`, `ReadFailed`, `WriteFailed`, `EraseFailed` |

## Distinguishing "network unavailable" from other failures

`NetError::NetworkUnavailable` means the network cannot carry traffic (Wi-Fi not configured or connecting, DHCP
incomplete, link lost); `DnsFailed` and `ConnectFailed` are only reported while the network is up. Use the Wi-Fi status
([Networking](../sdk/net.md), [`rohi-net` rustdoc](https://docs.rs/rohi-net)) to wait for readiness before retrying.

## Mapping at the control boundary

`rohi-control` maps the typed errors of `rohi-service` onto the protocol `ErrorCode`; protocol types never leak into
other crates.

| Source | Protocol `ErrorCode` |
|--------|----------------------|
| `ConfigError::UnknownSection`, `ActionError::UnknownService`, `ActionError::UnknownAction`, unknown service | `NOT_FOUND` |
| `ConfigError::Field(_)`, `ActionError::InvalidArgument`, malformed request | `INVALID_ARGUMENT` |
| `ActionError::Busy` | `BUSY` |
| `ConfigError::Unavailable`, `ActionError::Unavailable` | `UNAVAILABLE` |
| `ActionError::Rejected(reason)` | `REJECTED`, with `reason` |
| `ConfigError::Internal`, `ActionError::Internal` | `INTERNAL` |
| unknown request or missing capability | `NOT_SUPPORTED` |
| link policy denies a mutation | `PERMISSION_DENIED` |
