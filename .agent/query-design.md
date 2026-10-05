# Query feature design

Status: confirmed and implemented; validated with Rust and Python mock tests.
Date: 2026-10-05

## Purpose and proposed scope

Add a read-only `query` action that discovers configured GPIO pins and reports
their current line metadata. A client can inspect ownership before requesting
pins through `init`.

The first version supports `target: "gpio"`. The target names a subsystem for
this action; it retains its existing pin-selection meaning for `init`, `get`,
and `set`. Future subsystem targets, such as `i2c`, can use the same request
envelope with their own result payloads.

The review accepted the result shape, complete-result-or-error policy, shared
mock state prerequisite, and debug-client support. This revision adds an
optional GPIO pin filter and renames successful get responses to `get_result`.
The optional request argument is named `pin`, following review; `target` remains
the subsystem selector.

## Request

```json
{"id":"query-1","action":"query","target":"gpio"}
{"id":"query-2","action":"query","target":"gpio","pin":"GPIO1_A0"}
{"id":"query-3","action":"query","target":"gpio","pin":["GPIO1_A0","GPIO1_B5"]}
```

- `id`: non-empty string, following the current protocol's validation and
  response correlation rules. It is not a GPIO index or a session ID.
- `action`: the exact string `query`.
- `target`: a required string, matched exactly. Initially only `gpio` is
  supported; no case folding or trimming.
- `pin`: optional for `target: "gpio"`. When absent, return all configured GPIO
  pins. When present, accept one pin-name string or a non-empty array of pin-name
  strings and return only those keys. Match exact configuration keys without
  trimming, case folding, or `|` expansion; selection is independent of init.
- Proposed filter edge cases: reject null, empty strings, empty arrays,
  non-string elements, and reserved pin names as protocol errors. Repeated names
  collapse to one result entry because `pins` in the response is a keyed map.
  Validate every name against configuration before opening chips; an unmapped
  name returns one correlated session error, never a silently shortened result.
- No watch option. Follow the existing request envelope's treatment of extra
  fields; do not change parsing rules for existing actions as part of this feature.

A well-formed request with an unsupported target reaches the reactor and returns
a correlated error, leaving the connection usable:

```json
{"id":"query-4","status":"error","error":"query target `i2c` is not supported"}
```

Missing/non-string `target`, malformed JSON, and invalid IDs remain protocol
errors handled by the existing transport path, which closes the connection
rather than sending a session error response.

For example, an unmapped filter entry produces:

```json
{"id":"query-5","status":"error","error":"unmapped pin `UNKNOWN`"}
```

## Successful response

Accepted shape:

```json
{
  "id": "query-1",
  "status": "query_result",
  "target": "gpio",
  "pins": {
    "GPIO1_A0": {
      "id": 11,
      "is_used": false,
      "consumer": null,
      "direction": "input"
    },
    "GPIO1_B5": {
      "id": 26,
      "is_used": true,
      "consumer": "svc_7",
      "direction": "output"
    }
  }
}
```

The example uses illustrative mappings and state. The wire response is one JSON
line, like existing responses.

| Field | Type | Source and meaning |
| --- | --- | --- |
| `pins` key | string | Exact key from `[pins.gpiod]`, not the kernel line name. |
| Per-pin `id` | `u32` | Explicit configured `GPIODPinSpec.id`, for correlation with board GPIO data. |
| `is_used` | boolean | `LineInfo::is_used()` from a fresh line-info snapshot. |
| `consumer` | string or null | `LineInfo::get_consumer()`; preserve a present string, use null for `None`. |
| `direction` | `input` or `output` | `LineInfo::get_direction()`, independent of protocol access mode. |

`query_result` is the new response status, paired with the requested `get_result`
rename described below. `target` identifies which result schema applies.
Future subsystem results should be distinct typed variants rather than an
unstructured JSON value.

When `pin` is absent, return every configured GPIO key. Otherwise return only
selected keys. Both cases include selected pins not initialized by this client
and pins held by other clients or the kernel. Do not enumerate unmapped hardware
lines or infer device/line from pin spelling. Serialize result keys in lexical
order (`BTreeMap`), including filtered requests; array order does not determine
result object order. Clients should identify entries by key.

The result shape is the same for all selection forms: even one selected pin
returns a `pins` map. For the single-pin request above:

```json
{"id":"query-2","status":"query_result","target":"gpio","pins":{"GPIO1_A0":{"id":11,"is_used":false,"consumer":null,"direction":"input"}}}
```

Selected configuration aliases sharing `(device, line)` each appear under their own key,
with their own configured `id` and identical observed metadata. Take one snapshot
per distinct physical location and reuse it for aliases. Configuration `id`
does not replace `(device, line)` as the physical-location key.

Keep per-pin configured `id` as shown in the accepted draft. Device paths,
offsets, kernel line names, values, bias, drive, and edge settings are outside
this initial response.

## Get response status rename

Change successful `get` responses from `status: "pin_value"` to
`status: "get_result"` for consistency with `query_result`:

```json
{"id":"get-1","status":"get_result","value":1}
{"id":"get-2","status":"get_result","value":[0,1]}
```

Preserve the existing `value` field and scalar/array rules: a single target
string returns one bit, and an array returns an array in request order, even for
one element. Read permissions, initialization requirements, errors, and event
responses keep their existing behavior. Do not add `target` or `pins` to get
results or adopt query's keyed response shape.

This is a breaking wire-status rename. Emit only `get_result`; update response
deserialization, examples, Rust assertions, and debug-client fixtures to the new
status. A legacy alias is outside this revision. Rename the Rust status variant
and response constructor to `GetResult` and `get_result` for consistency;
the existing value payload type can remain `PinValuePayload`.

## Metadata semantics

The upstream [GPIO chip API](https://libgpiod.readthedocs.io/en/master/core_chips.html)
provides a newly allocated snapshot through `gpiod_chip_get_line_info`, without
requesting the line. The existing Rust wrapper already owns and frees this
snapshot.

The [libgpiod line-info API](https://libgpiod.readthedocs.io/en/v2.2.4/core_line_info.html)
supports inspection of requested and free lines. Its usage flag does not identify
why a line is busy; the consumer string can be absent. Direction is input/output,
and line values are excluded from line info.

Consequences for this feature:

- `is_used` includes this client's own requests. It is not a comparison against
  the caller's consumer name.
- A consumer name is a descriptive label, not proof of ownership or a unique
  process/session identity. Do not parse it or infer usage from it.
- A protocol trigger is reported as direction `input`; `trigger` is a session
  mode, not a hardware direction.
- `as_is` is a configuration instruction, not a query result. Treat an unexpected
  backend `AsIs` result as an error rather than invent a direction.
- `is_used: false` does not reserve the pin or guarantee a later `init` succeeds.
  Requests and metadata may change between query and initialization.
- Each physical line has its own snapshot. The whole response is not an atomic
  snapshot across lines or chips.

## Session behavior and errors

| Session state | Query behavior |
| --- | --- |
| `Connected` | Allowed without `init`; remain connected. |
| `Initialized` | Allowed for any configured pins, with the optional filter; not limited to initialized pins. |
| `SetSequenceRunning` | Allowed; leave the pending set and its eventual reply intact. |
| `Closing` / `Closed` | Reject if dispatched, using the existing closed-session error. |

Query never calls `request_lines`, reconfigures a line, applies final values,
registers a line-info watch, or changes session state. Trigger events may appear
between responses; clients keep using IDs and statuses for correlation. A query
reply may precede the reply for an earlier stepped set.

Accepted failure policy: one complete result or one existing string `error`
response. If opening any selected chip or inspecting any selected line fails, discard
the collected result and return a contextual error containing the configured
device/line or pin. Do not replace failed observations with `is_used: false`.
The connection remains usable and any initialized session stays intact.
Only selected devices/lines are inspected for a filtered query; a failure in an
unselected device must not fail that query. Startup validation remains separate
and still applies to the loaded configuration.

Reuse existing device-unavailable and missing-line error categories where
appropriate; retain context for other backend errors, including permission or
I/O failures. Release temporary resources on success and failure.

Partial success and per-pin errors are excluded from the first version.

## Fit with the current repository

The source code is the implementation baseline. Some existing documentation is
stale: `src/gpio/sys/` already implements libgpiod FFI, and `src/app.rs` selects
`SysBackend` on Linux without `--mock`. Hardware validation still requires a
supported Linux host/device; local behavior should be verified with the mock.

Existing interfaces cover all requested metadata: `Backend::open_chip`,
`Chip::get_line_info`, and `LineInfo::{is_used, get_consumer, get_direction}`.
No direct FFI calls belong in protocol or session code.

Proposed implementation changes, after review:

1. `src/protocol/request.rs`: add a query payload containing `target: String` and
   optional `pin: TargetSelector`. Reuse single/array pin-selector validation,
   while distinguishing an absent filter from explicit null (null is rejected).
   `src/protocol/mod.rs`: recognize `query`, with target support checked by the
   query handler so future/unsupported subsystem names receive a normal error.
2. `src/protocol/response.rs`: add a typed GPIO query payload, an input/output
   response direction enum, a `query_result` status, and its response constructor.
   Use a target-tagged result type that can grow with new subsystem variants.
   Rename the existing get status/constructor to `GetResult` / `get_result` and
   update all callers and response serialization/deserialization tests.
3. `src/session/initialized.rs`: extend object-safe `SessionConfig` with
   `gpiod_pins(&self) -> &BTreeMap<String, GPIODPinSpec>`, implemented by both
   `ServiceConfig` and the test pin-map implementation. Keep exact resolution.
4. Add `src/session/query.rs` for backend-generic collection: resolve all selected
   names before GPIO I/O, deduplicate repeated names, group selected pins by device
   and line, open each selected device once per query, collect fresh metadata,
   and assemble the keyed result. Use temporary chip handles, avoiding any
   changes to initialized chip indices or line requests.
5. `src/session/reactor.rs`: dispatch the query, validate state/target, call the
   collector, and send one correlated result or error. Query does not use
   `require_initialized`.

Follow the existing synchronous GPIO execution pattern initially. Collection
and response writes can delay timer handling in the same reactor; allowing query
during a sequence does not provide stricter timing guarantees. If supported
configurations make collection materially slow, evaluate a blocking worker with
completion delivered through the reactor before implementation is finalized.

## Mock backend prerequisite

Inspection found a correctness gap relevant to this design:
`MockBackend::open_chip` currently calls `open_chip_state` on every open; that
function constructs a new state with an empty request registry. Separate opens
therefore do not see each other's active requests, despite the sharing described
in `docs/architecture.md`.

Opening a chip for query would consequently report active service-owned lines
as unused. The XML consumer attribute cannot repair this: an unrequested mock
snapshot intentionally reports `is_used: false` and `consumer: None`. Reusing
only the querying session's chips would still miss other sessions.

Accepted prerequisite: share mock chip state and its watcher by exact device
path within the service's `MockBackend`, including clones. Keep independent
backend instances isolated for tests. A backend-owned synchronized registry of
cached `MockChip` handles can retain one state/watcher per opened path until the
backend is dropped; service paths are bounded by the loaded configuration.
Serialize first-open creation to avoid competing states/watchers for one path.
Do not canonicalize configured paths as part of this change.

This also makes mock exclusivity effective across sessions sharing the backend;
include that behavior explicitly in implementation review. Preserve request
drop/release, XML write logging, and watcher shutdown when backend/handles are
dropped. Do not add a separate service-wide lock system for the real backend.

Mock line info then reports active in-process registrations and effective
direction using the existing methods. It does not emulate external processes
or kernel reservations. External XML edits update input levels through polling;
metadata and structure are not merged into cached snapshots. Query itself must
not rewrite XML or append write-log snapshots.

## Compatibility, documentation, and debug client

Existing init/set/error/event responses, initialization rules, and configuration
files keep their current behavior. Clients issuing query must understand its new
response status; clients consuming get results must migrate from `pin_value` to
`get_result`. Add all/single/multiple-pin query examples, pre-init query rules,
and get-status migration notes to `docs/protocol.md` and `README.md`; document
collection and actual mock sharing in `docs/architecture.md`, and ownership
semantics in `docs/mock_chip.md`.

Add a one-shot `query --target gpio` command and a REPL query choice, with no
automatic initialization. Add repeatable `--pin NAME`: no flags omits `pin`,
one emits a string, and multiple emit an array. In the REPL allow all pins or
one/multiple names, and offer query before initialization as well as afterward.
Keep existing request-ID allocation and response/event correlation. For example:

```bash
python3 tools/debug_client.py query --target gpio
python3 tools/debug_client.py query --target gpio --pin GPIO1_A0
python3 tools/debug_client.py query --target gpio --pin GPIO1_A0 --pin GPIO1_B5
```

Existing raw/script commands also support query. Update `docs/debug_client.md`
and Python tests, including get-result fixtures and correlation coverage.

## Validation plan after approval

- Protocol: parse/round-trip unfiltered, single-pin, and array-filter requests;
  validate ID/target/filter types and empty/null/reserved-name cases; serialize
  query results including null consumers; verify get scalar/array results use
  `get_result`, with unchanged value shapes and rejection of legacy response
  status. Preserve existing init/set/error/event parsing and response shapes.
- Collector: return all configured keys when unfiltered and exactly selected
  keys when filtered; honor opaque names and configured IDs; validate unmapped
  names before I/O; collapse repeated names; handle multiple devices; reuse
  snapshots for selected aliases without including unselected aliases. Reject
  unexpected direction; verify a selected device/line failure produces only an
  error with cleanup, while unselected failing devices are never opened.
- Reactor: query before init, then init on the same connection; query after init
  includes uninitialized pins; query during a pending set leaves the sequence
  running; unsupported targets return correlated errors without closing.
  An unmapped filter error leaves the connection usable for another request.
- Shared mock: one session requests a line and another queries it; verify usage,
  consumer, input/output direction, and exclusivity across opens. After owner
  disconnect/final writes finish, a subsequent query reports it unused. Repeated
  queries do not alter XML, write logs, or active requests. Verify one watcher per
  path and cleanup when the backend is dropped.
- Socket integration: query-only connections succeed for every selection form;
  trigger correlation and existing init/get/set semantics remain intact, with
  successful get responses using `get_result`.
- Debug client: verify all/single/multiple query request shapes, pre-init REPL
  access, ID allocation, and interleaved events with query/get results.
- Run `cargo fmt --check`, `cargo test`, and
  `python3 tools/test_debug_client.py` after implementation.
- On available hardware, compare free, service-owned, and externally requested
  lines against `gpioinfo`; verify no line request/reconfiguration occurs during
  query. Mock tests alone do not establish real-device behavior.

## Review record and remaining details

Accepted in discussion:

1. Optional single/array pin filter; absent means all configured GPIO pins.
2. Draft query-result shape, including configured per-pin `id`.
3. Complete result or one error for the first version.
4. Shared mock state as a prerequisite.
5. Debug-client command and REPL support.
6. Rename successful get status from `pin_value` to `get_result`.
7. Name the optional request filter `pin`, accepting a string or array.

New details proposed in this revision: reject empty and null filters;
collapse repeated names in the result map; fail on unmapped
names before GPIO I/O; expose the client filter through repeatable `--pin`.

Implementation was authorized after this design was confirmed.
