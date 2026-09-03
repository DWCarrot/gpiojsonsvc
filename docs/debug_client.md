# Debug client

`tools/debug_client.py` is a Python 3 Unix-socket client for a running `gpiojsonsvc` process. It speaks the live protocol (`id` + `action`) over newline-delimited JSON and prints pretty-printed request/response objects.

Request and response shapes: [protocol.md](protocol.md). How sessions and events work: [architecture.md](architecture.md). Mock chip XML files: [mock_chip.md](mock_chip.md).

## Requirements

- Python 3 (stdlib only; no extra packages)
- A listening service. Start the mock backend first, for example:

```bash
cargo run -- --mock /path/to/gpiojsonsvc.toml
```

The default socket path is `/tmp/gpiojsonsvc.sock`. Match `--socket` to `service.socket` in the TOML if it differs.

Pin strings you pass to `init` must be exact `[pins.gpiod]` keys from that config.

Offline construction tests (no service):

```bash
python3 tools/test_debug_client.py
```

## Invocation

Run from the repository root. Omitting a subcommand (or using `repl`) opens the field-by-field wizard on one socket session:

```bash
python3 tools/debug_client.py [--socket PATH] [--timeout SECONDS]
python3 tools/debug_client.py [--socket PATH] [--timeout SECONDS] <command> ...
```

| Flag | Default | Meaning |
| --- | --- | --- |
| `--socket` | `/tmp/gpiojsonsvc.sock` | Unix domain socket path |
| `--timeout` | `5` | Socket timeout in seconds (connect and reads) |

Commands: default/`repl` (wizard), `init`, `get`, `set`, `raw`, `script`.

## Interactive wizard

The client connects before the first prompt and stays connected until `quit` / `exit` / Ctrl-D. Each cycle asks for `action`, then either walks protocol fields or reads raw JSON, prints the request, sends it, and prints the response.

`help`, `quit`, and `exit` are local-only (no send). Actions are `init`, `get`, `set`, or `raw`. Request ids are allocated as `"1"`, `"2"`, ... for the session unless a pasted request already has `id`.

```bash
python3 tools/debug_client.py
# action [init/get/set/raw, help/quit]: init
# target name: LED
# mode [input/output/trigger]: output
# pin (space-split): GPIO1_B5
# drive [push_pull/open_drain/open_source, empty=omit]: push_pull
# add another target? [y/N]: n
# (prints request JSON, sends, prints response)
# action [init/get/set/raw, help/quit]: set
# form [immediate/stepped]: immediate
# target name: LED
# value: 1
# target name:
# action [init/get/set/raw, help/quit]: quit
```

Empty field-by-field lines skip or re-prompt required fields. Optional fields (`bias` / `drive` / `edge`) are omitted when left empty. Validation errors (empty `init` target map, trigger with multiple pins, and similar) print and return to the action prompt without sending or closing the socket.

### Raw JSON (`raw`)

Paste a complete request object containing `action`, then a blank line to end. The input may span multiple lines. `action` must be `init`, `get`, or `set`; a missing or blank `id` is filled from the session allocator.

```
action [...]: raw
JSON (end with an empty line):
{
  "action": "init",
  "target": {
    "LED": {
      "mode": "output",
      "pin": "GPIO1_B5"
    }
  }
}

```

One-shot `init` / `get` / `set` / `raw` and `script` stay unchanged.

## Sessions vs. one-shot commands

Each accepted connection on the service is its own session. `init` must succeed **once on that connection** before `get` or `set`. Disconnect drops the session.

| Command | Connection lifetime |
| --- | --- |
| `init`, `get`, `set`, `raw` | Connect, send **one** request, wait for the matching response, disconnect |
| default / `repl`, `script` | One connection for the whole interactive session or file |

A second process cannot continue a session started by the first. This fails:

```bash
python3 tools/debug_client.py init --name LED --mode output --pin GPIO1_B5
python3 tools/debug_client.py get --target LED
```

The second process is not initialized. Use the wizard (`python3 tools/debug_client.py` or `repl`) or `script` for any sequence that needs `init` plus later `get`/`set`, or to observe trigger `event` messages after `init`.

Canned `init`/`get`/`set` are still useful to inspect the JSON they would send (`request:` is printed before the wire write) and to exercise a single request when you already know the session state, for example sending a lone `init` to check pin mapping.

## Wire format and output

The client writes each request as compact JSON plus `\r\n`. The server frames with `\n`. Incoming lines are parsed as JSON objects.

Stdout looks like:

```
request:
{
  "action": "init",
  "id": "1",
  ...
}
response:
{
  "id": "1",
  "status": "ok"
}
```

Keys in printed objects are sorted. That is display only; the payload on the wire is not reordered for protocol meaning.

Connect failures, missing socket files, and similar OS errors print `debug client error: ...` on stderr and exit `1`.

## Request ids

- The wizard and canned `init` / `get` / `set` allocate string ids `"1"`, `"2"`, `"3"`, ... unless you pass `--id` on a one-shot command.
- Interactive `raw` keeps a pasted `id`; a missing or blank `id` uses the allocator.
- One-shot commands send a single request, so the default is almost always `"1"`.
- One-shot `raw` and `script` send the `id` field as written. They do not rewrite it.

Trigger `event` messages reuse the successful `init` request `id`. The client still treats `status: "event"` as unsolicited, not as the reply to the current request.

## Unsolicited messages

While waiting for a matching response, the client keeps reading. A line is:

- **response** if it is not an event and (`id` matches the request, or the request had no usable `id`)
- **event** if `status` is `"event"` (even when `id` equals the `init` id)
- **unsolicited** otherwise (for example a late reply for a different id)

Events and other non-matching messages are printed with prefix `event:` or `unsolicited:` and then skipped. If the server closes before a matching non-event reply, the client raises `server closed the connection without a matching response`.

One-shot commands disconnect as soon as the matching reply arrives, so they will not sit and print later trigger events. Use the wizard (leave the prompt open) or a `script` that ends with a request that stays blocked only until its own reply.

In the wizard, socket reads use `select`. While a field prompt is waiting, trigger `event` lines are printed as they arrive and the prompt is reprinted. One-shot `init` / `get` / `set` / `raw` and `script` wait on the socket only (stdin is not multiplexed). Events that arrive **while** a later request is waiting are still printed as `event:`.

## `init`

Configure logical targets. Must be the first successful request on a connection. A second `init` on the same connection is an error.

Single named target:

```bash
python3 tools/debug_client.py init --name LED --mode output --pin GPIO1_B5
python3 tools/debug_client.py init --name IN --mode input --pin gpiochip0:7 --bias pull_up
python3 tools/debug_client.py init --name IRQ --mode trigger --pin GPIO1_A2 --edge rising
```

Combined `input`/`output` (up to eight pins; first pin is the high bit):

```bash
python3 tools/debug_client.py init --name BUS --mode output --pins GPIO1_B5 GPIO1_B6 GPIO1_B7
```

`--mode` is one of `input`, `output`, `trigger`. Optional flags:

| Flag | Applies to | Values |
| --- | --- | --- |
| `--bias` | `input` | `as_is`, `disabled`, `pull_up`, `pull_down` |
| `--drive` | `output` | `push_pull`, `open_drain`, `open_source` |
| `--edge` | `trigger` | `rising`, `falling`, `both` |

Use either `--pin` or `--pins`, not both. `--pins` is for combined targets; `trigger` still needs a single pin.

Multiple targets, or a full `target` object, via JSON:

```bash
python3 tools/debug_client.py init --target-json '{"LED":{"mode":"output","pin":"GPIO1_B5","drive":"push_pull"},"IN":{"mode":"input","pin":"gpiochip0:7"}}'
```

Do not mix `--target-json` with `--name` / `--mode` / `--pin` / `--pins`.

```bash
python3 tools/debug_client.py init --id init-1 --name LED --mode output --pin GPIO1_B5
```

## `get`

Read one or more `input` or `trigger` targets. Output targets are not readable.

```bash
python3 tools/debug_client.py get --target IN
python3 tools/debug_client.py get --target IN IRQ
python3 tools/debug_client.py get --id get-1 --target IN
```

`--target` is required. One name becomes a JSON string; two or more become a JSON array. A successful reply is `status: pin_value` with `value` a `u8` or an array of `u8` in request order.

On a fresh connection this returns `session is not initialized` unless you send `init` first on the same socket (wizard / `script`).

## `set`

Write `output` targets. Values are integers in the `u8` range; a target of width `n` rejects values `>= 2^n`.

Choose **one** of:

Immediate single target:

```bash
python3 tools/debug_client.py set --target LED --value 1
```

Immediate map (several targets at once):

```bash
python3 tools/debug_client.py set --target-json '{"LED":1,"BUS":5}'
```

Stepped sequence (JSON array). Step 0 has no `lag`; later steps include `lag` in milliseconds relative to the previous step. The service replies `ok` after the last step is applied.

```bash
python3 tools/debug_client.py set --steps-json '[{"LED":1},{"lag":100,"LED":0},{"lag":200,"LED":1}]'
```

`--target` without `--value` is an error. Mixing `--target`/`--value`, `--target-json`, and `--steps-json` is an error.

A second `set` while a sequence is still running on that connection returns `a set request is already in progress`. Disconnect cancels remaining steps.

## `raw`

Send one JSON object exactly as given. The `id` is not rewritten.

```bash
python3 tools/debug_client.py raw '{"id":"1","action":"get","target":"LED"}'
```

Useful for malformed or edge-case payloads. Same one-shot connection rules as `init`/`get`/`set`.

## `repl`

Same as omitting a command: the field-by-field wizard on one connection. See [Interactive wizard](#interactive-wizard).

```bash
python3 tools/debug_client.py repl
```

## `script`

Same as `repl`, but requests come from a file: one JSON object per line. Blank lines and lines whose first non-whitespace character is `#` are skipped.

```bash
python3 tools/debug_client.py script /path/to/session.jsonl
```

Example file:

```json
{"id":"1","action":"init","target":{"LED":{"mode":"output","pin":"GPIO1_B5"},"IN":{"mode":"input","pin":"gpiochip0:7"}}}
{"id":"2","action":"set","target":{"LED":1}}
{"id":"3","action":"get","target":"IN"}
```

The file must contain at least one JSON object. Each object is sent in order on a single connection; the next line waits until the previous matching response arrives.

## Typical session (`script`)

With a mock config that maps `GPIO1_B5` and `gpiochip0:7`:

```json
{"id":"init-1","action":"init","target":{"LED":{"mode":"output","pin":"GPIO1_B5","drive":"push_pull"},"IN":{"mode":"input","pin":"gpiochip0:7"},"IRQ":{"mode":"trigger","pin":"gpiochip0:7","edge":"both"}}}
{"id":"set-1","action":"set","target":{"LED":1}}
{"id":"get-1","action":"get","target":["IN","IRQ"]}
{"id":"set-2","action":"set","target":[{"LED":0},{"lag":50,"LED":1}]}
```

Trigger `event` objects that arrive while a later request is waiting are printed as `event:` and do not replace that request’s `response:`.

## Importing as a library

`tools/test_debug_client.py` imports the module (`debug_client`) and calls helpers such as `build_init_request`, `build_get_request`, `build_set_request`, and `wait_for_matching_response`. Put `tools/` on `PYTHONPATH` if you import from elsewhere:

```python
from debug_client import DebugClient, build_init_request, build_set_request

with DebugClient("/tmp/gpiojsonsvc.sock", timeout=5.0) as client:
    client.send(build_init_request({"LED": {"mode": "output", "pin": "GPIO1_B5"}}, "1"))
    client.send(build_set_request({"LED": 1}, "2"))
```

`DebugClient.send` waits for the matching non-event response and, in the CLI path, prints unsolicited traffic via `print_unsolicited`.
