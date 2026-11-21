# Action result formats

Every [action](action.md) returns its result in **one of five formats**. The service picks the format when it declares the
action; the control protocol, discovery and every client then apply the same rules, whatever the service is. The format
describes what the result *is* and what is guaranteed about it, not how many messages carried it: a collection is a
collection whether it had zero, three or sixteen records.

| Format | What the action returns | Lifecycle | Example |
|--------|-------------------------|-----------|---------|
| **Empty** | nothing: the operation succeeded and has no data, by contract | one reply | `wireless.wifi_reconnect`, `system.reboot` |
| **Inline** | one record of named, typed fields | one reply | `control.tcp_generate_psk` (`psk`, `fingerprint`) |
| **Records** | a finite collection of records with one schema | snapshot, read page by page | `wireless.wifi_scan` (`ssid`, `rssi`, `channel`, `security`) |
| **Feed** | one bounded page of a source that keeps changing, addressed by a cursor | stateless polling | `logging.tail` |
| **Stream** | a binary snapshot of one or more named files | open, read chunks, close | `logging.download`, `connectivity.get_payload` |

An action has one format for good: a result never turns from `Inline` into `Records` or `Stream` because it got larger.
`rohi action SERVICE ACTION --help -o json` shows it as `format` in the action description, so a client knows what to
expect before it calls.

## Choosing a format

- Nothing to report? **Empty**. An action that found nothing is not Empty: a scan that sees no network returns an empty
  **Records** result, because "no networks" is an answer.
- One bounded object (a generated key, the public key of a new identity)? **Inline**. It is one object, not a collection
  of one.
- A list whose length is not known in advance but is finite and produced by one operation (networks, catalog entries)?
  **Records**. The control plane pages it; the service writes no paging code and the action takes no `offset`.
- Recent data of something that goes on while you read it (the log tail)? **Feed**.
- Binary data that is larger than a message, or that must be saved as files (the log archive, a signed packet)? **Stream**.

## Empty, Inline and an empty collection

These are three different answers and `rohi` shows them differently:

| Result | Text | `-o json` |
|--------|------|-----------|
| Empty | `Done` | `{"format": "empty"}` |
| Inline | one `name: value` line per field | `{"format": "inline", "record": {...}}` |
| Records, none | `No results` | `{"format": "records", "records": []}` |
| Records, some | a table, one row per record | `{"format": "records", "records": [...]}` |

## Records: one operation, any number of pages

`InvokeAction` runs the operation **once** and returns the first page of the result. A page holds whole records only,
never part of one, and is limited both in records and in bytes, so a longer result continues on further pages:

```bash
rohi action wireless wifi_scan          # all networks of one scan, the pages followed for you
rohi -o json action wireless wifi_scan
```

- **One scan, not many.** Reading the next page does not run the action again; an expensive operation (a Wi-Fi scan)
  never repeats to produce page two. A scan of 16 networks arrives as 6 + 6 + 4 records.
- **Stable.** While the result is held, the same page read again returns the same records in the same order.
- **Held for you only.** The device keeps the rest of a long result (in its compact native form, not as protocol records)
  for the connection that ran the action. Another connection that presents the page token, a closed or expired token
  or a malformed one gets `not found` and no records; pages need the same access as the action that created them.
- **Released.** `rohi` closes the result when it has read it. A client that stops early can close it, or just
  stop: the device releases it after a period without a read, when the connection ends and when the link is lost. A device holds a
  few results at once; while one is held, an action that needs another (a second `wifi_scan`) answers `busy`.
- **Retry.** A lost reply to the first call is not retried by itself, because a new call could run the operation again. A
  lost reply to a later page is simply asked again.
- **A result that fits one page** is never held on the device.

Configuration and status are **not** paged by this mechanism: their responses always fit one message.

## Feed: a cursor over a changing source

A feed is not a snapshot. It gains entries while you read and loses old ones when its retention is exceeded, so the page
says where the reader stands: the **session** of the source (a boot), the **cursor** (the last entry returned), the range
still held, whether the request started **over** (`reset`, because the session changed) and how many entries were lost to
overwriting (`gap`). An **empty page means "caught up"**, not "finished". Nothing is kept for the reader, so any number of
clients can poll, and access is checked on every call. `rohi logs --follow` is a client of the log tail feed.

## Stream: files read in chunks

A stream is a snapshot of files. The client reads each file by offset, in chunks, and closes the stream; the reads are
repeatable. The device holds a stream for the connection that opened it and releases it on close, after a period without
a read and when the connection ends. `rohi logs download` and the signed-packet export use streams; binary transfers never
use Records or Feed.

## Access, secrets and cancellation

- The format changes nothing about **access**: the action declares what the link must be allowed to do, the policy of the
  link decides, and a page, a chunk or a close needs the same access as the call that created it. Whether a request
  arrived over an encrypted or authenticated link is established by the transport, never by anything in the request.
- A secret **Inline** result (a generated key) travels only on a link that the action requires (`secure_link`) and is wiped
  after the reply is encoded.
- A call that is cancelled or fails before its result is owned leaves nothing held for good: the device releases a result
  nobody owns after its period without a read.

## Adding an action

[Add a control operation or an action](../guides/add-control-operation.md) shows where a format is declared; the contract of the five
formats, the limits and the writer API are in the [`rohi-service` rustdoc](https://docs.rs/rohi-service), the wire form in
the [`rohi-control-proto` rustdoc](https://docs.rs/rohi-control-proto), and the Rust client (`invoke_outcome`,
`records_pages`, `records_all`) in the [`rohi` library](https://docs.rs/rohi).
