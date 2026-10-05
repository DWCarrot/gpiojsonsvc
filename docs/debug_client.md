# Debug client

`tools/debug_client.py` is a Python 3 Unix-socket client using only the standard library. It sends newline-delimited JSON and prints requests, responses, and unsolicited events. See [protocol.md](protocol.md) for wire rules and [architecture.md](architecture.md) for session behavior.

## Start and connect

```bash
cargo run -- --mock /path/to/gpiojsonsvc.toml
python3 tools/debug_client.py --socket /tmp/gpiojsonsvc.sock
```

`--socket` defaults to `/tmp/gpiojsonsvc.sock`; `--timeout` defaults to 5 seconds. Match the socket to the service configuration. Pin names are exact `[pins.gpiod]` keys. Use `A|B` to share init parameters; quote expressions containing `|` in shell commands.

Commands are default/`repl`, `init`, `query`, `get`, `set`, `raw`, and `script`.

## Session lifetime

`init` must succeed once on the same connection before `get` or `set`. Query requires no initialization. The wizard and `script` keep one connection for the entire session. Each one-shot command (`init`, `query`, `get`, `set`, `raw`) connects, sends one request, receives its reply, and disconnects. A later command in another process cannot reuse that initialized session.

Use one-shot init to check configuration or the wizard/script for actual init/get/set sequences. Disconnect applies any configured final output values.

## Interactive wizard

Omit the subcommand or pass `repl`. The wizard connects once and prompts for each request. `help`, `quit`, `exit`, and Ctrl-D are local actions.

```text
action [init/query/get/set/raw, help/quit]: init
pin expression (A|B): GPIO1_B5|GPIO1_B6
mode [input/output/trigger]: output
drive [push_pull/open_drain/open_source, empty=omit]: push_pull
initial [0/1 per pin, empty=omit]: 0
final [0/1 per pin, empty=omit]: 0
add another pin group? [y/N]: n
```

Each group shares its init parameters. There is no separate target-name prompt or pin binding. Trigger groups are supported, and their events identify individual pin names. Initial/final values are 0 or 1 per pin, not packed integers.

For get, enter space-separated pin names such as `GPIO1_A0 GPIO1_A1`. One name gives one value; multiple names give an array. For set, choose immediate or stepped, then enter pin name/value pairs with values of 0 or 1. End each map with an empty name. Later steps prompt for a positive millisecond delay.

Optional init fields are omitted when left empty. Invalid input prints an error and returns to the action prompt without closing the connection. The wizard strips surrounding prompt whitespace; use raw JSON for configuration keys containing significant surrounding whitespace.

The wizard reads events while waiting for keyboard input and prints them without ending the session.

## Init commands

```bash
python3 tools/debug_client.py init --mode output --pin GPIO1_B5 --initial 1 --final 0
python3 tools/debug_client.py init --mode input --pin 'GPIO1_A0|GPIO1_A1' --bias pull_up
python3 tools/debug_client.py init --mode trigger --pins GPIO1_A2 GPIO1_A3 --edge both
```

Use `--pin` for one pin or a quoted expression, or `--pins` for a list joined with `|`. Both forms support all modes; init groups have no eight-pin limit. `--name` has been removed.

Optional parameters:

- Input `--bias`: `as_is`, `disabled`, `pull_up`, `pull_down`.
- Output `--drive`: `push_pull`, `open_drain`, `open_source`.
- Output `--initial` and `--final`: integer 0 or 1, applied to every pin in the group. Omission preserves the request-time value or skips the close write respectively.
- Trigger `--edge`: `rising` (default), `falling`, `both`.

For multiple entries, supply a complete target map:

```bash
python3 tools/debug_client.py init --target-json '{"GPIO1_B5":{"mode":"output","initial":1},"GPIO1_A0":{"mode":"input"}}'
```

Do not mix `--target-json` with `--mode`, `--pin`, or `--pins`. The nested `pin` field from the old protocol is rejected.

## Query commands

Query works on a fresh connection and reports GPIO metadata, including usage by
other sessions. The request uses `target: "gpio"` and an optional `pin` filter:

```bash
python3 tools/debug_client.py query --target gpio
python3 tools/debug_client.py query --target gpio --pin GPIO1_A0
python3 tools/debug_client.py query --target gpio --pin GPIO1_A0 --pin GPIO1_B5
```

The target defaults to `gpio`. No `--pin` flags selects all configured pins;
one flag sends a string, and multiple flags send an array. The response uses
`status: "query_result"` and a `pins` map, even for one pin. Entries contain
configured `id`, `is_used`, nullable `consumer`, and `direction`.

In the wizard, choose `query`, accept `gpio` (or leave target empty), then enter
space-separated pin names or leave the pin prompt empty for all. Query is
available before init and afterward. Use raw JSON for names containing spaces.

## Get and set commands

These examples show command syntax; get/set on a fresh connection return `session is not initialized`. Use equivalent requests in the wizard or script after init.

```bash
python3 tools/debug_client.py get --target GPIO1_A0
python3 tools/debug_client.py get --target GPIO1_A0 GPIO1_A1
python3 tools/debug_client.py set --target GPIO1_B6 --value 1
python3 tools/debug_client.py set --target-json '{"GPIO1_B5":1,"GPIO1_B6":1,"GPIO1_B7":0}'
python3 tools/debug_client.py set --steps-json '[{"GPIO1_B5":1,"GPIO1_B6":0},{"lag":100,"GPIO1_B6":1}]'
```

Reads permit input and trigger pins; writes permit outputs. Get and set accept only individual names, with values of 0 or 1. Init sharing does not restrict which pins a later request can address. Successful get replies use `status: "get_result"` with the existing scalar/array `value` field; `pin_value` is the previous status.

Set accepts exactly one of `--target` plus `--value`, `--target-json`, or `--steps-json`. Step 0 omits lag or uses zero; later steps require positive `u32` millisecond delays. All steps are validated before execution. The reply follows the last step; another set while a sequence is running is rejected, while get and query remain allowed. Disconnect cancels remaining steps and applies final values.

## Raw JSON

One-shot raw sends the object as written:

```bash
python3 tools/debug_client.py raw '{"id":"1","action":"get","target":"GPIO1_A0"}'
```

In the wizard, choose `raw`, paste a complete object with an `action`, and finish with a blank line. Multi-line JSON is supported. A missing/blank ID is filled by the wizard's allocator:

```json
{
  "action":"init",
  "target":{
    "GPIO1_B5|GPIO1_B6":{"mode":"output","initial":0,"final":0}
  }
}
```

Raw requests bypass the canned builders' validation, which is useful for testing malformed requests. The service still validates them.

## Script

```bash
python3 tools/debug_client.py script /path/to/session.jsonl
```

The file contains one request object per line; empty lines and lines beginning with `#` after whitespace are skipped. All requests use one connection, and each waits for its matching response before the next is sent.

With the corresponding configuration keys:

```json
{"id":"1","action":"init","target":{"GPIO1_B5|GPIO1_B6":{"mode":"output","initial":0,"final":0},"GPIO1_A0":{"mode":"input"},"GPIO1_A2":{"mode":"trigger","edge":"both"}}}
{"id":"2","action":"set","target":{"GPIO1_B6":1,"GPIO1_B5":0}}
{"id":"3","action":"get","target":["GPIO1_A0","GPIO1_A2"]}
{"id":"4","action":"set","target":[{"GPIO1_B5":1},{"lag":50,"GPIO1_B5":0,"GPIO1_B6":0}]}
```

## IDs, output, and events

The wizard and canned commands allocate increasing string IDs (`"1"`, `"2"`, …). Use `--id` to override an ID on a one-shot command. One-shot raw and script preserve IDs as written.

Requests are sent with `\r\n`; responses are read as newline-delimited JSON. Printed objects are formatted and sorted for display. `request:` precedes the outgoing object; `response:` precedes its matching non-event reply.

Trigger events reuse the init ID but are always unsolicited, even when their ID matches the awaited request. They print as `event:` and do not replace a response. Other unmatched messages print as `unsolicited:`. Events arriving during a later request are still displayed. One-shot commands leave after the matching reply; use the wizard to keep observing events.

Connection failures and similar OS errors print `debug client error: ...` to stderr and exit 1. Closing before a matching reply raises `server closed the connection without a matching response`.

## Importing and testing

Put `tools/` on `PYTHONPATH` to use the client as a library:

```python
from debug_client import DebugClient, build_init_request, build_set_request

with DebugClient("/tmp/gpiojsonsvc.sock", timeout=5.0) as client:
    client.send(build_init_request({"GPIO1_B5|GPIO1_B6": {"mode": "output"}}, "1"))
    client.send(build_set_request({"GPIO1_B6": 1, "GPIO1_B5": 0}, "2"))
```

`build_target_config` now accepts the mode and optional parameters only; put the pin expression in the surrounding init map. `DebugClient.send` waits for the matching non-event reply and displays unsolicited messages.

Run the tests (no live service needed):

```bash
python3 tools/test_debug_client.py
```
