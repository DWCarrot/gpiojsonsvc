# I²C design for review

Status: proposal, 2026-10-07. No implementation is authorized by this draft.
The original [ideas](i2c.ideals.md) remain unchanged. These documents describe
proposed behavior, not the current protocol.

## Reading order

1. This document: scope, corrections, and decisions for discussion.
2. [Research and tool semantics](i2c-research.md).
3. [Architecture and configuration](i2c-architecture.md).
4. [Protocol](i2c-protocol.md).
5. [Backend contracts, mock model, and Linux implementation](i2c-backends.md).

## Recommended direction

Add controller/master-mode I²C as a sibling of GPIO. Preserve GPIO requests
and allow a single `init` to acquire GPIO pins and named I²C adapters together.
An I²C session selects an adapter at init; each subsequent request chooses its
device address. The byte-transfer primitive is an ordered list of messages
executed in one combined transfer. `read`, `write`, and `fetch` compile to this
primitive; an explicit `transfer` action exposes the general case.

Use Linux `/dev/i2c-*` ioctls directly behind a small backend contract. Do not
execute command-line tools from the service. Mock adapters implement that same
contract, including repeated-START boundaries, failure results, and ownership.

## Corrections to the ideas

| Idea | Proposed interpretation or correction |
| --- | --- |
| `[pins.i2c]`, `device`, `sda`, `scl` | Keep. SDA/SCL are resource IDs matching GPIO `id`, not libgpiod offsets. They document and reserve resources; they do not configure pin multiplexing. |
| List buses like `i2cdetect -l` | List only configured names, not every adapter installed on the host. |
| Named query like `i2cdetect -F` | Report adapter functionality separately from features exposed by this service. |
| `query target=i2c:scan` | Keep the spelling, but classify it as active I/O. Require an initialized bus and configuration opt-in. A scan is not proof of presence/absence and is not passive metadata. |
| `mode=i2c`, dynamic address | Keep. No address, frequency, drive, bias, initial, or final fields on I²C init. |
| `register` | Optional explicit byte prefix in wire order. No universal integer width or byte order can be inferred. |
| `read` | Require a byte count. With a register prefix, issue write-prefix + repeated-START read. |
| `write` without response | No device readback, but still return the service's correlated `ok` or `error` after completion. |
| `fetch` waits for a response | A combined write/read transfer with a specified read length, not asynchronous response arrival or a device-ready wait. |
| `raw` string without validation | JSON strings are Unicode. Recommend `bytes`, `hex`, and `base64` in v1; reject `raw`. An optional future `utf8` format must have explicit encoding and read-error rules. |
| Reuse verbs for SPI later | Reuse routing and byte codecs. Do not assume SPI has I²C address, register, or repeated-START semantics. |

The reasons and primary sources for the hardware/tool corrections are in
[research](i2c-research.md); protocol choices above are recommendations.

## Review decisions

D1 and D2 are accepted by the user: exclusive bus ownership per session and
raw I²C first. D3–D6 remain proposals for discussion. These decisions do not
authorize implementation.

| Decision | Recommendation | Alternative / consequence |
| --- | --- | --- |
| D1: ownership — accepted | One session exclusively leases an entire adapter. The architecture associates its SDA/SCL resource claims with that lease. | Sharing between sessions is excluded from this design. |
| D2: transaction scope — accepted | Ship raw I²C first. Defer explicit Linux SMBus transaction operations and PEC. | Raw byte operations do not provide full `i2cget`/`i2cset` compatibility. The two internal SMBus scan probes remain conditional on D4. |
| D3: register representation | An array of prefix bytes, e.g. `[18,52]` for a device expecting big-endian `0x1234`. | An integer needs explicit width and byte order; a default can silently access the wrong register. |
| D4: active scan | Keep `i2c:scan`, require init, default `allow_scan=false`; report each probe outcome. | Drop remote scanning entirely, or allow a temporary exclusive lease before init. Never silently treat it as passive query. |
| D5: payload formats | `bytes` default; strict `hex` and `base64`; defer textual `raw`. | Add `utf8`, with the possibility that a read consumes device data but cannot be represented as a valid string. |
| D6: general transfer | Include `transfer` in v1, with an address on every message. | Start with only the three convenience verbs; this cannot express all useful `i2ctransfer` combinations. |

D1 establishes exclusive adapter leases so that separate write/read requests
from one session cannot be interleaved by another service session. This cannot
exclude external processes or kernel drivers. D2 and D4 are scope choices, not
limitations imposed by I²C. Deferred SMBus support means the Linux userspace
transaction family, not every SMBus feature such as ARP, Alert, or Host Notify.

## Planned implementation after review

1. Resolve D3–D6 and revise these drafts; D1 and D2 are accepted. Keep public
   documentation describing current behavior until the implementation lands.
2. Add configuration types, canonical resource identities, and atomic resource
   leases. Cover GPIO/I²C conflicts, aliases, rollback, and I²C-only config.
3. Add transfer types, byte codecs, protocol validation, typed responses, and
   compilation tests independent of either backend.
4. Add mock adapter/device models and the asynchronous execution path. Verify
   lifecycle and GPIO timing/event behavior while an I²C operation is blocked.
5. Implement and verify the Linux ABI wrapper, capability checks, driver-busy
   checks, scan primitives, and error mapping.
6. Add end-to-end tests and debug-client support; update `README.md`,
   `docs/protocol.md`, `docs/architecture.md`, a new `docs/mock_i2c.md`, mock
   fixtures, and Rock5B examples. Document OS adapter setup and permissions.

## Acceptance checks

- Existing GPIO configurations and request/response examples still work;
  compatibility exceptions are explicitly listed in the architecture draft.
- A combined fetch is one transfer with two messages. The mock trace and an
  ioctl fake both distinguish it from two separate operations.
- Invalid later messages cause no earlier bus traffic; runtime failures do not
  claim to roll back effects already produced on the device.
- Distinct addresses can be used through the same initialized bus. No address
  is inferred from names or shifted from an 8-bit datasheet address.
- Concurrent sessions, aliases, mixed init failure, disconnect during I/O,
  stale completions, and GPIO final writes retain correct lease lifetimes.
- Capability-disabled mocks and fake Linux calls exercise unsupported, busy,
  NACK, short transfer, timeout, and adapter-disappearance paths.
- Run `cargo fmt --check` and `cargo test` after Rust implementation; run
  `python3 tools/test_debug_client.py` after debug-client changes.
- Hardware verification uses a known test device and its datasheet. Compare
  selected operations against the documented tools, and inspect repeated START
  with a logic analyzer where available. Mock success alone proves no electrical
  timing, pinmux, clock, or adapter-quirk behavior.

No tests of the running service are required for this documentation-only draft.
