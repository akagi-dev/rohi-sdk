# Errors

Public errors are small typed enums in ROHI terms. Platform crates map every vendor error into them; raw vendor errors
never reach higher layers. Messages never contain secrets.

| Domain | Type | Where | Variants / meaning |
|--------|------|-------|--------------------|
| Wi-Fi | `WifiError` | `rohi-net` | `NotConfigured`, `Busy` (command queue full / scan in flight), `Timeout`, `ScanFailed`, `NetworkNotFound`, `AuthenticationFailed`, `AssociationFailed`, `DhcpFailed`, `Disconnected`, `InvalidConfig`, `DriverError` |
| IP data plane | `NetError` | `rohi-net` | `NetworkUnavailable`, `DnsFailed`, `ConnectFailed`, `NoSocketAvailable`, `ConnectionReset`, `Timeout`, `Backend`; implements `embedded_io_async::Error` |
| BLE | `BleError` | `rohi-net` | `Disconnected` (returned once per connection end), `BufferTooSmall`, `Busy`, `DriverError` |
| Wireless runtime | `WirelessError` | `rohi-net-esp` | `AlreadyTaken`, `BleInit` |
| Configuration store | `ConfigError<E>`, `SectionError` | `rohi-config` | see [Configuration store › Errors](../sdk/config.md#errors) |
| Configuration service | `ServiceError` | firmware `config` | `Unavailable`, `StorageHealthy`, `Store`, `Section`, `Encode` |
| Measurements | `MeasurementServiceError` | `rohi-hal` | `Busy` (only `try_snapshot`) |
| Sensors | `SensorErrorCode`, `DriverError` | `rohi-hal` | domain error codes reported in `SensorEvent::Error` |
| Control services | `ServiceError` | `rohi-control` | `InvalidArgument`, `NotFound`, `NotSupported`, `Busy`, `Unavailable`, `Internal` |
| Control protocol | `ErrorCode` | `rohi-control-proto` | `INVALID_ARGUMENT`, `NOT_FOUND`, `NOT_SUPPORTED`, `BUSY`, `UNAVAILABLE`, `PERMISSION_DENIED`, `INTERNAL` |
| Framing | `FrameError` | `rohi-control-ble` | bad version, invalid/duplicate/missing fragment, changed header, oversize, length mismatch |

## Distinguishing "network unavailable" from other failures

`NetError::NetworkUnavailable` means the network cannot carry traffic (Wi-Fi not configured or connecting, DHCP
incomplete, link lost); `DnsFailed` and `ConnectFailed` are only reported while the network is up. Use the Wi-Fi status
([Networking](../sdk/net.md)) to wait for readiness before retrying.

## Mapping at the control boundary

| Source | Control `ServiceError` | Protocol `ErrorCode` |
|--------|------------------------|----------------------|
| `WifiError::NotConfigured` | `NotFound` | `NOT_FOUND` |
| `WifiError::Busy` | `Busy` | `BUSY` |
| `WifiError::InvalidConfig` | `InvalidArgument` | `INVALID_ARGUMENT` |
| `WifiError::Timeout`, `ScanFailed` | `Unavailable` | `UNAVAILABLE` |
| other `WifiError` | `Internal` | `INTERNAL` |
| configuration store unavailable | `Unavailable` | `UNAVAILABLE` |
| other configuration failures | `Internal` | `INTERNAL` |
| missing capability | – | `NOT_SUPPORTED` |
| link policy denies a mutation | – | `PERMISSION_DENIED` |
