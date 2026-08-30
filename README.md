# gpiojsonsvc

`gpiojsonsvc` is a Rust service that exposes GPIO over a Unix domain socket using newline-delimited JSON. Protocol pin strings are opaque: the service never infers a chip or line from spelling. Every `pin` value must appear as an exact key in the TOML pin map.

## Current status

The runnable path is the mock backend plus a Unix-socket listener:

- Load TOML (positional path, `GPIOJSONSVC_CONFIG`, or `gpiojsonsvc.toml`)
- Select the mock backend with `--mock` (optionally `GPIOJSONSVC_MOCK_LOG` to append chip XML dumps after writes)
- Accept clients on `service.socket`
- Handle `init`, `get`, and `set` (immediate and stepped) per connection
- Emit trigger `event` messages for configured edge targets

Without `--mock`, startup fails with `real backend unavailable; use --mock`. `src/gpio/sys.rs` is a stub; Rock5B `libgpiod` FFI is not implemented.

## Configuration

Discovery order:

1. positional `CONFIG` path
2. environment variable `GPIOJSONSVC_CONFIG`
3. `gpiojsonsvc.toml` in the process working directory

The file holds service settings and a portable `[pins.gpiod]` map. Backend selection is a CLI flag, not a TOML field. Each mapped `device` is interpreted by the selected backend: a one-chip XML path in mock mode, a device path (for example `/dev/gpiochip0`) once the real backend exists.

```toml
[service]
socket = "/tmp/gpiojsonsvc.sock"

[pins.gpiod]
"gpiochip0:7" = { device = "/path/to/gpiochip0.xml", line = 7 }
"GPIO1_B5" = { device = "/path/to/gpiochip1.xml", line = 13 }
```

Rules:

- Pin keys are matched exactly (no case folding, trimming, or `chip:line` parsing).
- Keys and `device` strings must be non-empty; `line` must be a `u32`.
- At least one mapping is required.
- Several pin strings may share one `device` and different `line` values.

## Mock chips

With `--mock`, each distinct `device` path is one XML document whose root is a single `<gpiochip id="...">` (not a `<gpiochips>` wrapper). The `id` attribute is chip metadata and is not used to resolve protocol pins. Full element and attribute rules: [docs/mock_chip.md](docs/mock_chip.md).

```xml
<gpiochip id="gpiochip0" label="mock gpiochip0">
    <line id="0" name="line0" direction="input" bias="pull_up">L</line>
    <line id="7" name="line7" direction="output" drive="push_pull">H</line>
</gpiochip>
```

Line text is the physical level (`H` or `L`). Startup validates that every mapped path exists, parses as one-chip XML, and contains the configured `line`. Opening the same path twice in one session reuses one session chip; different paths open independent chips.

## Run

```bash
gpiojsonsvc
gpiojsonsvc /path/to/gpiojsonsvc.toml
gpiojsonsvc --mock /path/to/gpiojsonsvc.toml
GPIOJSONSVC_MOCK_LOG=/tmp/mock-write.log gpiojsonsvc --mock /path/to/gpiojsonsvc.toml
```

`--mock` is required until the real backend exists. `--mock` enables the file-backed mock. `GPIOJSONSVC_MOCK_LOG` also enables the mock backend write log at that path; it is an error if that variable is set without `--mock`. The process listens until SIGINT; it removes a stale socket file before bind and removes the socket on shutdown.

## Protocol (summary)

Each request has a non-empty `id` and an `action`. `init` must succeed once per connection before `get` or `set`. Pin strings in `init` must match `[pins.gpiod]` keys.

```json
{"id":"1","action":"init","target":{"LED":{"mode":"output","pin":"GPIO1_B5"}}}
{"id":"2","action":"get","target":"IN"}
{"id":"3","action":"set","target":{"LED":1}}
```

Combined `input`/`output` targets take up to eight unduplicated pins packed into a `u8` (first pin is the high bit). Trigger targets take a single pin. Stepped `set` is an array of objects; step 0 must not include `lag`, later steps must. The `ok` reply is sent after the first step; remaining lags run in the session reactor. Trigger events reuse the `init` request `id`.

Full request and response shapes: [docs/protocol.md](docs/protocol.md). Layers and session flow: [docs/architecture.md](docs/architecture.md).

## Debug client

Usage, session rules, and examples: [docs/debug_client.md](docs/debug_client.md).

With no subcommand (or `repl`), the client opens one socket and walks `init` / `get` / `set` field by field. Use `init raw` / `get raw` / `set raw` to paste multi-line JSON ended by a blank line.

```bash
python3 tools/debug_client.py --socket /tmp/gpiojsonsvc.sock
python3 tools/debug_client.py --socket /tmp/gpiojsonsvc.sock script /path/to/session.jsonl
python3 tools/debug_client.py --socket /tmp/gpiojsonsvc.sock init --name LED --mode output --pin GPIO1_B5
python3 tools/debug_client.py --socket /tmp/gpiojsonsvc.sock raw '{"id":"1","action":"get","target":"LED"}'
```

`init`/`get`/`set`/`raw` each open a new connection and send one request. Use the wizard or `script` for `init` followed by `get`/`set` on the same session. The wizard and canned commands allocate increasing string IDs unless `--id` is set on a one-shot command. `raw` and `script` send JSON as written.

Python unit tests (no live service):

```bash
python3 tools/test_debug_client.py
```

## Tests

```bash
cargo fmt
cargo test
```

## Deferred

Not implemented yet:

- Output-default semantics and restore-on-disconnect
- Process-wide shared-read / exclusive-write locks
- Protocol trigger `filter`
- Real `libgpiod` backend (`src/gpio/sys.rs`)
- Optional Cargo feature gate for the mock backend

`.cursor/general-instrument.md` records earlier product intent; it is not the live protocol.
