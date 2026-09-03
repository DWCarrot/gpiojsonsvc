# Protocol Reference

The service listens on a Unix domain socket (`SOCK_STREAM`). Each request and response is one JSON object on a single line. The server frames with newline (`\n`). Clients may send `\n` or `\r\n`; the debug client writes `\r\n`. How to use `tools/debug_client.py`: [debug_client.md](debug_client.md).

Every request must include a non-empty string `id`. Responses reuse that `id`, except trigger `event` messages, which reuse the successful `init` request `id`.

Protocol `pin` values are opaque strings. The service does not parse chip names, separators, board labels, or case. A pin is valid only when it matches a `[pins.gpiod]` key exactly.

## Requests

### `init`

Configures the logical targets for this connection. It must be the first successful request. A second `init` on the same connection returns an error.

```json
{
  "id": "init-1",
  "action": "init",
  "target": {
    "GPIO4_B3": {
      "mode": "input",
      "pin": "gpiochip0:2",
      "bias": "as_is"
    },
    "CombinedIN": {
      "mode": "input",
      "pin": ["gpiochip0:3", "gpiochip1:4", "gpiochip1:5"],
      "bias": "pull_up"
    },
    "GPIO4_B5": {
      "mode": "output",
      "pin": "gpiochip2:1",
      "drive": "push_pull"
    },
    "GPIO1_A2": {
      "mode": "trigger",
      "pin": "gpiochip1:1",
      "edge": "rising"
    }
  }
}
```

`target` is a map from logical name to a tagged object with `mode`.

| Mode | `pin` | Optional fields |
| --- | --- | --- |
| `input` | string, or array of 1–8 strings | `bias`: `as_is`, `disabled`, `pull_up`, `pull_down` |
| `output` | string, or array of 1–8 strings | `drive`: `push_pull`, `open_drain`, `open_source` |
| `trigger` | single non-empty string only | `edge`: `rising` (default), `falling`, `both` |

Rules:

- Pin strings must be non-empty.
- Combined lists must be non-empty and at most eight entries.
- Combined pins that resolve to the same physical `(device, line)` are rejected as a duplicate physical location.
- Unmapped pin strings, missing device files, and missing lines are rejected at `init`.
- Distinct configured `device` paths are opened once each; pins that share a device share that chip.
- There is no protocol `default` on outputs and no `filter` on triggers.

Combined packing: array index 0 is the most significant bit of a `u8`. For `["A","B","C"]`, bit 2 is `A` and bit 0 is `C`.

### `get`

Reads one or more readable targets (`input` or `trigger`). Output targets are not readable.

```json
{ "id": "get-1", "action": "get", "target": "GPIO4_B3" }
```

```json
{ "id": "get-2", "action": "get", "target": ["GPIO4_B3", "CombinedIN"] }
```

`target` is a non-empty string or a non-empty array of non-empty strings. Unknown names fail.

A single-target reply is one `u8`. A multi-target reply is an array of `u8` in request order. Combined targets pack bits the same way as `init`.

### `set`

Writes output targets. Values are `u8`; a target of width `n` (n < 8) rejects values `>= 2^n`. Combined outputs apply the packed value to all constituent lines in one batch.

Immediate form — object of target name to value:

```json
{
  "id": "set-1",
  "action": "set",
  "target": {
    "GPIO4_B5": 1,
    "CombinedOUT": 5
  }
}
```

Stepped form — non-empty array of objects. Each object is a map of target names to values, plus `lag` in milliseconds:

```json
{
  "id": "set-2",
  "action": "set",
  "target": [
    { "GPIO4_B5": 1 },
    { "lag": 100, "GPIO4_B5": 0 },
    { "lag": 200, "GPIO4_B5": 1 }
  ]
}
```

Rules:

- Step 0 must not include `lag` (or must use lag `0`).
- Later steps must include a non-zero `lag`. The delay is relative to the previous step.
- Each step must contain at least one target value.
- All steps are compiled before any GPIO write. If compilation fails, nothing is written.
- All steps are applied in order. Step 0 runs immediately; remaining steps wait for their `lag` in the session reactor. The service replies `ok` after the last step is applied. A later-step apply failure replies `error` instead.
- A second `set` while a sequence is running returns `a set request is already in progress`. `get` remains allowed.
- Disconnect cancels an in-progress sequence. Remaining steps are not applied, the `set` reply is not sent, and outputs are not restored to a default.

## Responses

### `ok`

```json
{ "id": "init-1", "status": "ok" }
```

Used for successful `init` and `set`.

### `pin_value`

```json
{ "id": "get-1", "status": "pin_value", "value": 1 }
```

```json
{ "id": "get-2", "status": "pin_value", "value": [1, 5] }
```

### `error`

The `error` field is a string, not a structured code object.

```json
{ "id": "get-1", "status": "error", "error": "session is not initialized" }
```

Examples of session error text:

- `session is not initialized` / `session is already initialized` / `session is closed`
- `a set request is already in progress`
- `unknown target \`NAME\``
- `target \`NAME\` is not readable` / `is not writable`
- `target \`NAME\` value N exceeds W configured bits`
- `unmapped pin \`PIN\``
- `device file \`PATH\` is unavailable`
- `line N is not available on device \`PATH\``
- `pin \`PIN\` maps to a duplicate physical location`

Malformed JSON, empty `id`, or unknown `action` fail at the transport/protocol layer rather than as an `error` status object.

### `event`

Unsolicited. `id` is the `init` request id. `event.type` is `rising` or `falling` (never `both`; a `both` trigger emits the edge that occurred).

```json
{
  "id": "init-1",
  "status": "event",
  "event": { "target": "GPIO1_A2", "type": "rising" }
}
```

There is no protocol-level debounce or software filter. Kernel/mock edge detection follows the `edge` set at `init`.
