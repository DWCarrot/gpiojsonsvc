# Protocol Reference

The service listens on a Unix domain socket (`SOCK_STREAM`). Each request and response is one JSON object on a single line. The server frames with `\n`; clients may send `\n` or `\r\n`. Every request requires a non-empty string `id` and an `action`. Responses reuse the request ID; unsolicited trigger events reuse the successful `init` ID.

## Pin expressions

The `target` field addresses configured pin names directly. There are no user-defined target bindings. In `init` only, `A|B|C` shares parameters across three pins. Each component must exactly match a `[pins.gpiod]` key: no trimming, case folding, or inference of chip/line from its spelling. `|` is reserved as a separator; configuration keys containing it and the exact key `lag` are rejected. `lag` is reserved for stepped-write timing.

Empty components (`A||B`, `|A`, `A|`) and repeated pins within an expression are invalid. Physical locations are identified by configured `(device, line)`; aliases resolving to the same location cannot be initialized together.

Init sharing creates no lasting group. Get and set accept only individual pin names; `|` is rejected. Each value is 0 or 1.

## `init`

Must succeed once per connection before `get` or `set`. A second successful initialization on the same connection is rejected. `target` is a non-empty map from pin expression to parameters:

```json
{
  "id": "init-1",
  "action": "init",
  "target": {
    "GPIO4_B3|GPIO4_B2": {
      "mode": "output",
      "drive": "push_pull",
      "initial": 0,
      "final": 0
    },
    "GPIO3_C3": { "mode": "output", "drive": "open_drain" },
    "GPIO1_A0|GPIO1_A1": { "mode": "input", "bias": "pull_up" },
    "GPIO1_A2|GPIO1_A3": { "mode": "trigger", "edge": "both" }
  }
}
```

Supported parameters:

- `input`: optional `bias` (`as_is`, `disabled`, `pull_up`, `pull_down`).
- `output`: optional `drive` (`push_pull`, `open_drain`, `open_source`); optional `initial` and `final`, each an integer **0 or 1 applied to every pin in the entry**.
- `trigger`: optional `edge` (`rising`, the default; `falling`; `both`). Each pin produces its own events.

There is no eight-pin limit on init groups. Each pin/physical location may appear only once across the entire init request, even when repeated entries would use the same parameters. Duplicate JSON init keys are also rejected. Unmapped pins, unavailable devices, and missing lines fail initialization. Devices are opened once per distinct configured path within the session.

Omitting `initial` leaves the existing line value unchanged when requested. Omitting `final` skips a close write for that pin. To assign different initial/final values, use separate init entries. Address each pin individually in subsequent get/set requests.

Graceful close applies configured final values, then releases GPIO. This runs for disconnect, explicit session shutdown, service shutdown, command-channel closure, and response-write failure. A failed final write is logged and teardown continues; there is no close reply. Init validation is performed before requesting lines, but backend failures across multiple chips do not provide transactional rollback of initial writes.

Unknown init parameters, including the removed `pin` field, are rejected. There is no protocol `default` on outputs or `filter` on triggers.

## `get`

Reads initialized `input` and `trigger` pins. A request may read both modes. Output pins are not readable.

```json
{"id":"get-1","action":"get","target":"GPIO1_A1"}
{"id":"get-2","action":"get","target":["GPIO1_A0","GPIO1_A2"]}
```

A string returns one value (0 or 1). A non-empty array of pin names returns an array of values in request order, including for a one-element array. Repeated names return a value at each requested position. There is no eight-pin limit.

All referenced pins must have been initialized on this connection, even if they exist in the service configuration. Init grouping does not affect reads.

## `set`

Writes initialized output pins. Each entry accepts only the integer 0 or 1.

```json
{"id":"set-1","action":"set","target":{"GPIO4_B3":1,"GPIO3_C3":0,"GPIO4_B2":1}}
```

Here `GPIO4_B3=1`, `GPIO3_C3=0`, and `GPIO4_B2=1`. Each pin retains its initialization settings.

Stepped writes use a non-empty array:

```json
{
  "id":"set-2",
  "action":"set",
  "target":[
    {"GPIO4_B3":1,"GPIO3_C3":0},
    {"lag":100,"GPIO3_C3":0,"GPIO4_B2":1},
    {"lag":200,"GPIO4_B3":0}
  ]
}
```

Rules:

- Immediate maps and each step must contain at least one pin value.
- Step 0 omits `lag` or uses zero. Later steps require a positive `u32` `lag` in milliseconds, relative to the previous step.
- Duplicate pin keys within an immediate map or a step are rejected. Pins may be written again in later steps.
- Every step is validated and compiled before step 0 executes. Later steps can address different pins.
- Writes are batched per chip. Multiple chips are applied sequentially; there is no atomicity or rollback guarantee across chips. A backend apply failure can leave earlier chip writes applied.
- The `ok` reply follows the last applied step. An apply failure produces an `error` reply.
- A second `set` while a sequence is running returns `a set request is already in progress`; `get` remains allowed.
- Disconnect cancels remaining steps without a set reply; configured per-pin final values are still applied.

## Responses

Successful init or set:

```json
{"id":"init-1","status":"ok"}
```

Get results:

```json
{"id":"get-1","status":"pin_value","value":1}
{"id":"get-2","status":"pin_value","value":[0,1]}
```

Session errors use a string field:

```json
{"id":"get-1","status":"error","error":"pin `GPIO1_A0` is not initialized"}
```

Other errors include uninitialized/already initialized/closed sessions, unreadable/unwritable pins, unmapped pins, duplicate physical locations, unavailable devices. Malformed JSON, invalid init expressions, combined get/set names, set values other than 0 or 1, invalid init parameters, empty IDs, and unknown actions fail at the transport/protocol layer rather than returning a session `error` status.

Events identify the individual configured pin, even when triggers were initialized together:

```json
{"id":"init-1","status":"event","event":{"target":"GPIO1_A2","type":"rising"}}
```

`event.type` is `rising` or `falling`, never `both`. There is no protocol debounce or software filter.

## Migrating from named targets

Replace `"LED":{"mode":"output","pin":"GPIO1_B5"}` with `"GPIO1_B5":{"mode":"output"}` and address `GPIO1_B5` directly thereafter. For init sharing, replace pin arrays with `|`-separated keys. Replace combined get targets with arrays of individual names, and expand packed set values into separate per-pin entries. Packed initial/final values must be expanded into separate per-pin entries when bits differ; grouped initial/final values now broadcast one bit. Remove debug-client `--name` arguments. Legacy bindings are not accepted.
