# Proposed I²C protocol

Status: draft, using the recommendations in [review decisions](i2c-design.md).
Examples are new proposed messages. Existing GPIO wire forms remain valid.

D2 is accepted: raw I²C first. Explicit SMBus transactions and PEC are deferred.
The proposed scan's internal SMBus probes remain subject to the D4 review.

## Common rules

Keep newline-delimited JSON, non-empty string `id`, and correlated responses.
`target` for I/O is one exact initialized bus name. `name` is the filter used
by I²C queries. Neither accepts arrays or `|` expressions. No client-supplied
device paths are accepted. The server routes through the configured map.

V1 supports unshifted 7-bit device addresses in the regular range 8–119
(`0x08–0x77`). Wire addresses are JSON integers, not hexadecimal strings; for
example `80` means `0x50`, not an 8-bit address byte containing the R/W bit.
Reserved addresses and 10-bit addressing are rejected, with no force option.

Every raw read has an explicit positive byte `length`. Every write has a
non-empty decoded payload. There are no zero-length messages, automatic device
scans, unbounded reads, implicit word interpretation, retries by the service,
or implicit delays. A device needing write–STOP–wait–read uses separate requests
and client-side timing; it must not use `fetch` for that sequence.

## Query configured buses

```json
{"id":"q1","action":"query","target":"i2c"}
```

```json
{"id":"q1","status":"query_result","target":"i2c","kind":"buses","buses":["I2C_1"]}
```

List configured names sorted lexically. This form performs no device access,
is available before init and during other operations, and returns an empty
array when I²C is not configured. It lists configuration, not proven hardware
availability or attached devices.

## Query one adapter

```json
{"id":"q2","action":"query","target":"i2c","name":"I2C_1"}
```

```json
{
  "id":"q2",
  "status":"query_result",
  "target":"i2c",
  "kind":"capabilities",
  "name":"I2C_1",
  "capabilities":["i2c","smbus_quick","smbus_read_byte"],
  "functionality_mask":"0x00030001",
  "service_features":["fetch","read","transfer","write"],
  "limits":{"messages":42,"message_bytes":4096,"transfer_bytes":16384},
  "sda":10,
  "scl":8,
  "lease":"free",
  "allow_scan":false
}
```

This illustrative adapter supports only the three shown functionality bits.
`capabilities` is the sorted set of supported known individual `I2C_FUNC_*`
names with the prefix removed and lowercased; omit aggregate convenience masks.
Keep unknown bits in the lowercase hexadecimal `functionality_mask` string.
Do not truncate native capability bits through a JSON float. `service_features`
is a sorted set of currently available service verbs, derived from adapter
capabilities and configuration policy. It contains `scan` only if enabled and
at least one supported probe method exists. Full SMBus operations, 10-bit
addressing, and protocol flags are not advertised as service features in v1.

`lease` is `free`, `self`, or `other`, and describes only this service's adapter
lease. It does not report kernel drivers at individual addresses. The capability
and lease snapshot is not atomic and does not reserve the adapter. This query
requires no init, sends no address probes, and obtains capability information
through the shared worker. Busy worker or unavailable adapter returns an error;
never silently fall back to stale data. `pin` is invalid on both I²C query forms.

## Initialize

```json
{"id":"i1","action":"init","target":{"I2C_1":{"mode":"i2c"},"GPIO4_B3":{"mode":"output","initial":0,"final":0}}}
```

```json
{"id":"i1","status":"ok"}
```

I²C entries allow only `mode`. Reject address, frequency, GPIO settings, and
unknown fields. GPIO-only and I²C-only init are valid; require at least one
entry overall. The existing one-successful-init-per-connection rule remains.
Attempting GPIO use of SDA/SCL in the same or another session conflicts with the
I²C resource lease. Init does not probe or reset connected devices.

## Byte formats and register prefixes

`format` defaults to `bytes` and controls `data` on both request and response:

| Format | Input | Output |
| --- | --- | --- |
| `bytes` | Array of integers 0–255; reject floats, booleans, nulls, negatives, and overflow. | Array of integers. |
| `hex` | String of exactly two ASCII hex digits per byte; either case accepted; no prefix, separators, whitespace, or odd length. | Lowercase contiguous hex. |
| `base64` | Canonical RFC 4648 standard alphabet, required padding when needed; no whitespace or URL alphabet. | Canonical padded base64. |

All formats decode to an owned byte buffer before submission. Limit encoded
input size before decoding; count the decoded size against transfer limits.
`raw` is rejected in this draft. If text is added later, call it `utf8` and
define invalid-UTF-8 read behavior explicitly; JSON source escape sequences
must never be transmitted literally or mistaken for arbitrary binary bytes.

`register`, when present, is a non-empty `bytes` array independent of `format`.
It is an exact prefix, not an integer or an address interpreted by the backend.
Its length counts toward the write-message and total-transfer limits. For a
16-bit register index `0x1234`, choose `[18,52]` or `[52,18]` according to the
device's datasheet. No universal register width, byte order, or auto-increment
behavior is assumed.

## Read

```json
{"id":"r1","action":"read","target":"I2C_1","address":80,"length":4,"format":"hex"}
```

```json
{"id":"r1","status":"read_result","target":"I2C_1","address":80,"format":"hex","length":4,"data":"11223344"}
```

Without `register`, compile to one read message. With `register`, compile to
one write-prefix message followed by one read message in the same transfer:

```json
{"id":"r2","action":"read","target":"I2C_1","address":80,"register":[18,52],"length":4}
```

Both forms require `I2C_FUNC_I2C`; they do not silently choose an SMBus call
on an SMBus-only adapter. `data` is not permitted on read requests.

## Write

```json
{"id":"w1","action":"write","target":"I2C_1","address":80,"register":[16],"format":"bytes","data":[171,205]}
```

```json
{"id":"w1","status":"ok"}
```

Compile to one write message containing `[16,171,205]`. If `register` is
absent, transmit only `data`. `data` is required and non-empty; to send a
pointer-setting or command-only write use that command as `data`, e.g. `[16]`.
`length` is invalid for write. The reply acknowledges transfer completion, not
application-level device acceptance or completion of an EEPROM write cycle.

“Without response” means no I²C read message. Suppressing the JSON reply would
discard errors and break the service's normal request correlation, so the draft
does not introduce fire-and-forget writes.

## Fetch

```json
{"id":"f1","action":"fetch","target":"I2C_1","address":80,"format":"hex","data":"1234","length":4}
```

```json
{"id":"f1","status":"fetch_result","target":"I2C_1","address":80,"format":"hex","length":4,"data":"11223344"}
```

Compile to exactly two messages: write non-empty `data`, then repeated-START
read of `length` bytes at the same address. An optional `register` is prepended
to the write data, using the same rule as write. There is one final STOP.
`read` with register `[18,52]` and `fetch` with data `[18,52]` produce the same
transfer. Fetch does not poll for readiness, sleep between messages, or wait
for an unsolicited reply. No bus operation is automatically replayed on error.

## General transfer

```json
{
  "id":"t1",
  "action":"transfer",
  "target":"I2C_1",
  "format":"bytes",
  "messages":[
    {"direction":"write","address":80,"data":[18,52]},
    {"direction":"read","address":80,"length":4},
    {"direction":"read","address":81,"length":1}
  ]
}
```

```json
{"id":"t1","status":"transfer_result","target":"I2C_1","format":"bytes","completed_messages":3,"reads":[{"message":1,"length":4,"data":[17,34,51,68]},{"message":2,"length":1,"data":[7]}]}
```

Require 1–42 messages. Every message has its own required address; do not inherit
it from the previous message. Read messages have `length` only, write messages
have `data` only. Top-level `address`, `register`, `data`, and `length` are
invalid on this action. `format` applies to all message payloads. Each message
boundary is a repeated START, including write-to-write and read-to-read.

Read results retain zero-based input-message indices and message order. A
write-only transfer returns `transfer_result` with an empty `reads` array.
The whole list must validate before bus I/O; all addresses must pass driver-busy
checks before submission. An adapter may reject a valid list because of its
own hardware restrictions. Do not split such a list into multiple transfers.

## Active scan

```json
{"id":"s1","action":"query","target":"i2c:scan","name":"I2C_1","first":80,"last":83,"method":"auto"}
```

```json
{
  "id":"s1",
  "status":"query_result",
  "target":"i2c:scan",
  "name":"I2C_1",
  "first":80,
  "last":83,
  "method":"auto",
  "results":[
    {"address":80,"probe":"receive_byte","state":"ack"},
    {"address":81,"probe":"receive_byte","state":"no_ack"},
    {"address":82,"probe":"receive_byte","state":"busy"},
    {"address":83,"probe":"receive_byte","state":"error","os_errno":5}
  ]
}
```

Unlike other queries, require successful init of this exact bus, its exclusive
lease, and `allow_scan=true`. Before init return `not_initialized`; an
uninitialized bus on an otherwise initialized session returns `unknown_target`.
This deliberate difference exists because probing sends traffic and can change
device state. Default range is 8–119 inclusive, default method is `auto`.
Require integer bounds in that range and `first <= last`.

Methods are `auto`, `quick_write`, or `receive_byte`. Auto uses the address
selection documented in [research](i2c-research.md). A forced method requires
its capability or fails before any probe. If auto has neither capability,
fail before scanning; otherwise unsupported per-address methods yield
`unsupported`, with no fallback to a different probe at that address.

Return exactly one sorted observation per address in the requested range:
`ack`, `no_ack`, `busy`, `unsupported`, or `error`. `probe` records the selected
method even when skipped. `busy` means the kernel-driver address check blocked
the probe. `no_ack` is used only for an address-NACK result that the backend can
identify (Linux `ENXIO`); ambiguous failures remain `error`, including
`EREMOTEIO` when no phase information exists. `error` includes `os_errno` for
Linux failures, otherwise a diagnostic `reason`. Neither `ack` nor `busy`
identifies a device model, and `no_ack` does not prove an address is unused.

Continue after per-address failures and include them in the result. On fatal
adapter removal (`ENODEV`) or worker failure, return one error instead of a
partial scan result; previous probes may have had effects. On disconnect,
cancel between probes and send no result. Do not automatically scan at startup
or from any other query. `pin` is invalid on scan requests.

## Validation, errors, and ordering

Follow the existing transport policy: malformed JSON, invalid field types,
missing required fields, invalid encodings/ranges, duplicate new object fields,
and unknown fields in the new strict request forms are protocol errors that
close the connection. Adding typed query variants must preserve the existing
unsupported-target correlated-error behavior; do not parse all unknown targets
as fatal enum errors. Avoid tightening unrelated legacy request forms here.

Well-formed operations can fail semantically and leave the session usable:
unknown name, missing init, occupied resource, busy worker, unsupported adapter,
scan disabled, kernel-driver ownership, or hardware transfer failure. Keep the
existing `status:error` and human-readable `error` string. Add optional `details`
to that response variant, present for I²C operational errors only:

```json
{"id":"f1","status":"error","error":"I2C_1 transfer failed: address did not acknowledge","details":{"subsystem":"i2c","code":"no_ack","effect":"possible","completed_messages":null,"os_errno":6}}
```

Codes: `not_initialized`, `unknown_target`, `already_initialized`,
`resource_busy`, `i2c_busy`, `scan_disabled`, `unsupported`, `device_busy`,
`no_ack`, `timeout`, `partial_transfer`, `io`, and `internal`. Mixed-init
resource conflicts may use `subsystem:resources` instead. Existing GPIO errors
keep their current fields. `os_errno` and `completed_messages` are nullable;
positive short Linux results set `completed_messages` to the actual count.
Do not infer a completed byte count or failing message index from a negative
errno. `effect:none` means rejected before bus submission; `effect:possible`
means bus traffic started or its effects are uncertain. Prior scan probes also
require `effect:possible` on an eventual fatal scan error.

A combined transfer prevents interleaving between its messages, but provides
no rollback. A short/failed transfer returns no successful read payload. Do not
retry any part of it automatically, even if a read was requested: reads can
consume FIFOs or clear flags. Lost replies/disconnects leave clients uncertain
about effects; request IDs are correlation only, not deduplication keys.

Only one I²C worker operation is pending per session. A second receives
`i2c_busy`; GPIO get/set/query, events, and timers can still progress. There is
no total order between GPIO and I²C effects. Clients needing ordering wait for
one reply before submitting the dependent operation. No `cancel` or
`timeout_ms` field is introduced in v1.
