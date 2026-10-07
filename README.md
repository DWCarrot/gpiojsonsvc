# gpiojsonsvc

`gpiojsonsvc` is a Rust service that exposes GPIO over a Unix domain socket using newline-delimited JSON. Protocol pin strings are opaque: the service never infers a chip or line from spelling. Each component of a `|`-separated pin expression must appear as an exact key in the TOML pin map.

## Current status

The service supports real GPIO on Linux through libgpiod 2.x and a file-backed mock backend:

- Load TOML (positional path, `GPIOJSONSVC_CONFIG`, or `gpiojsonsvc.toml`)
- Use the real backend by default on Linux, or select the mock with `--mock` (optionally `GPIOJSONSVC_MOCK_LOG` to append chip XML dumps after writes)
- Accept clients on `service.socket`
- Handle `init`, `query`, `get`, and `set` (immediate and stepped) per connection
- Emit trigger `event` messages for configured trigger pins

The real backend is implemented in `src/gpio/sys/`. Startup checks that each distinct mapped device is a GPIO chip; line availability and access are checked when queried or requested.

## Configuration

Discovery order:

1. positional `CONFIG` path
2. environment variable `GPIOJSONSVC_CONFIG`
3. `gpiojsonsvc.toml` in the process working directory

The file holds service settings and a portable `[pins.gpiod]` map. Backend selection is a CLI flag, not a TOML field. Each mapped `device` is interpreted by the selected backend: a one-chip XML path in mock mode, a GPIO chip device path (for example `/dev/gpiochip0`) in real mode.

```toml
[service]
socket = "/tmp/gpiojsonsvc.sock"
gpio-consumer = "svc_{id}"

[pins.gpiod]
"gpiochip0:7" = { id = 7, device = "/path/to/gpiochip0.xml", line = 7 }
"GPIO1_B5" = { id = 26, device = "/path/to/gpiochip1.xml", line = 13 }
```

Rules:

- Pin keys are matched exactly (no case folding, trimming, or `chip:line` parsing).
- Each mapping requires `id`, `device`, and `line`. Keys and `device` strings must be non-empty; `id` and `line` must be `u32` integers (0–4294967295).
- `id` is an explicit GPIO index: the integer value of the pin's key in the board's `gpio.json` (the physical header-pin index for Rock5B). It is reserved for future occupation checks across features such as I2C. It is stored without affecting current device/line resolution or conflict checks. It is never inferred from the pin key, device path, or line offset; existing configurations must add it. This pin ID is separate from the session ID used in `service.gpio-consumer`.
- Pin keys cannot contain `|` or equal `lag`; these are reserved by the protocol.
- At least one mapping is required.
- Several pin strings may share one `device` and different `line` values.
- `service.gpio-consumer` is the libgpiod request consumer for every chip request owned by a session. When omitted, it defaults to `svc_{id}`. At most one `{id}` placeholder may appear; it is replaced with that session's monotonically increasing reactor `session_id`. Allowed characters are ASCII letters, digits, `-`, and `_`, plus that exact `{id}` placeholder. The value must be non-empty and at most 12 characters; that limit applies only to the configured string, not to the rendered result after `{id}` expansion.

## Mock chips

With `--mock`, each distinct `device` path is one XML document whose root is a single `<gpiochip id="...">` (not a `<gpiochips>` wrapper). The `id` attribute is chip metadata and is not used to resolve protocol pins. Full element and attribute rules: [docs/mock_chip.md](docs/mock_chip.md).

```xml
<gpiochip id="gpiochip0" label="mock gpiochip0">
    <line id="0" name="line0" direction="input" bias="pull_up">L</line>
    <line id="7" name="line7" direction="output" drive="push_pull">H</line>
</gpiochip>
```

Line text is the physical level (`H` or `L`). Startup validates that every mapped path exists, parses as one-chip XML, and contains the configured `line`. Opening the same path twice in one session reuses one session chip; different paths open independent chips.

## Build

Prerequisites for Linux builds, including mock usage:

- A Rust toolchain with Cargo supporting edition 2024.
- `pkg-config` and libgpiod **2.x** development headers and libraries (`libgpiod-dev` or the equivalent package; 1.x is insufficient).
- `libclang` for generating the FFI bindings with bindgen.

Check that `pkg-config --modversion libgpiod` reports 2.x, then build:

```bash
cargo build
cargo build --release
```

## Run

```bash
gpiojsonsvc
gpiojsonsvc /path/to/gpiojsonsvc.toml
gpiojsonsvc --mock /path/to/gpiojsonsvc.toml
GPIOJSONSVC_MOCK_LOG=/tmp/mock-write.log gpiojsonsvc --mock /path/to/gpiojsonsvc.toml
```

Without `--mock`, the service uses the real backend on Linux. `--mock` enables the file-backed mock. `GPIOJSONSVC_MOCK_LOG` also enables the mock backend write log at that path; it is an error if that variable is set without `--mock`. The process listens until SIGINT or SIGTERM, then closes live sessions, removes the socket, and exits. It also removes a stale socket file before bind.

## Group access to the socket

Opening GPIO chip devices needs root, so the service process stays root. Clients do not. A Unix socket is a filesystem object: `connect` needs search (`x`) on every directory in the path and write (`w`) on the socket itself. Put the socket in a setgid directory owned by a dedicated group, and start the process with a umask that leaves the socket group-writable.

`/run` is a tmpfs and is empty after reboot. The steps below create the directory for the current boot. The systemd unit in the next section recreates it on each start.

1. Create a group for the clients. The name `gpio` in the commands below is an example; any group name works if the directory, the socket, and `Group=` in the systemd unit all use that same name. Skip `groupadd` if `getent group gpio` already prints a line:

```bash
sudo groupadd --system gpio
```

2. Add the client account to that group. Group membership is read at login, so that user must log in again (or start a new `login` / `sudo -u` session) before it applies:

```bash
sudo usermod -aG gpio alice
```

3. Create `/run/gpiojsonsvc` owned by `root:gpio`, mode `0750` (owner `rwx`, group `r-x`):

```bash
sudo install -d -o root -g gpio -m 0750 /run/gpiojsonsvc
```

4. Set the setgid bit. A socket created in that directory then inherits group `gpio` instead of the creating process's primary group:

```bash
sudo chmod g+s /run/gpiojsonsvc
```

`ls -ld /run/gpiojsonsvc` should show `drwxr-s---`. The `s` in the group execute column is setgid. Mode is now `2750`.

Point the service at that path:

```toml
[service]
socket = "/run/gpiojsonsvc/service.sock"
```

Setgid does not change the socket mode. `bind` creates the inode as `0777` masked by the process umask. Root's usual umask `0022` produces `srwxr-xr-x`, and the group still cannot connect. `sudo` also resets the umask, so set `0117` in the root shell. That yields `srw-rw----` (`0660`):

```bash
sudo sh -c 'umask 0117; exec gpiojsonsvc /etc/gpiojsonsvc/config.toml'
```

After start, `ls -l /run/gpiojsonsvc/service.sock` should show `srw-rw---- root gpio`. A user in group `gpio` can then connect; other accounts cannot traverse the directory.

## systemd

`systemd/gpiojsonsvc.service` encodes the same directory and umask. `Group=gpio` sets the process group. `UMask=0117` makes the socket `0660`. `RuntimeDirectory=gpiojsonsvc` and `RuntimeDirectoryMode=2750` create `/run/gpiojsonsvc` as `root:gpio` with setgid before the process binds, and remove that directory when the service stops.

Install the binary and unit, then enable it:

```bash
sudo install -D -m 755 target/release/gpiojsonsvc /usr/local/bin/gpiojsonsvc
sudo install -D -m 644 assets/rock5b/config.toml /etc/gpiojsonsvc/config.toml
sudo install -D -m 644 systemd/gpiojsonsvc.service /etc/systemd/system/gpiojsonsvc.service
sudo systemctl daemon-reload
sudo systemctl enable --now gpiojsonsvc.service
```

The unit's `ExecStart` uses the real backend. For a mock deployment, add `--mock` to `ExecStart` and point the configured devices at mock XML files. The config's `socket` must be `/run/gpiojsonsvc/service.sock`, matching the runtime directory.

## Protocol (summary)

Each request has a non-empty `id` and an `action`. `init` must succeed once per connection before `get` or `set`. For these actions, `target` uses exact configuration pin names directly; custom target bindings and the nested init `pin` field have been removed. Query uses `target: "gpio"` and an optional top-level `pin` filter and does not require init.

```json
{"id":"1","action":"init","target":{"GPIO1_B5|GPIO1_B6":{"mode":"output","initial":1,"final":0},"GPIO1_A0":{"mode":"input"}}}
{"id":"2","action":"get","target":"GPIO1_A0"}
{"id":"3","action":"set","target":{"GPIO1_B6":1,"GPIO1_B5":0}}
{"id":"4","action":"query","target":"gpio"}
{"id":"5","action":"query","target":"gpio","pin":"GPIO1_A0"}
{"id":"6","action":"query","target":"gpio","pin":["GPIO1_A0","GPIO1_B5"]}
```

In `init`, `A|B` applies the same parameters to each pin. Output `initial` and `final` are optional single-bit values (0 or 1), broadcast to every pin in that entry. Omit `initial` to preserve the existing value when requesting the line; omit `final` to skip a close write. Init groups can also configure multiple triggers, each emitting events under its own pin name.

In `get` and `set`, use individual pin names and values of 0 or 1. Get accepts one name or an array of names; set accepts separate name/value entries. `|` is supported only in init. Reads allow input/trigger pins; writes require outputs. Each physical pin may appear only once in init. Stepped `set` uses an array of objects: step 0 omits `lag` (or uses 0), and later steps require positive millisecond delays. The `ok` reply follows the last applied step. Trigger events reuse the init request ID; final values are applied on graceful session close.

Query omits `pin` to return all configured GPIO pins; a string or non-empty array
returns only the selected exact keys. The `query_result` response contains
`target: "gpio"` and a `pins` map. Each entry contains the configured GPIO `id`,
`is_used`, nullable `consumer`, and input/output `direction`. It observes current
ownership, including other sessions, without requesting lines or changing
settings. Unknown pins or selected backend failures return one error rather than
a partial result. Query is also allowed during stepped sets.

Successful get replies now use `status: "get_result"` instead of `pin_value`,
with the same scalar/array `value` field. This is a breaking response-status
change; update clients and see the migration notes in the protocol reference.

Full request and response shapes: [docs/protocol.md](docs/protocol.md). Layers and session flow: [docs/architecture.md](docs/architecture.md).

## Debug client

Usage, session rules, and examples: [docs/debug_client.md](docs/debug_client.md).

With no subcommand (or `repl`), the client opens one socket and walks `init` / `query` / `get` / `set` field by field. Query is available before init. Use `raw` to paste a multi-line request object, including its `action`, ended by a blank line.

```bash
python3 tools/debug_client.py --socket /tmp/gpiojsonsvc.sock
python3 tools/debug_client.py --socket /tmp/gpiojsonsvc.sock script /path/to/session.jsonl
python3 tools/debug_client.py --socket /tmp/gpiojsonsvc.sock init --mode output --pin GPIO1_B5
python3 tools/debug_client.py --socket /tmp/gpiojsonsvc.sock query --target gpio
python3 tools/debug_client.py --socket /tmp/gpiojsonsvc.sock query --target gpio --pin GPIO1_A0 --pin GPIO1_B5
python3 tools/debug_client.py --socket /tmp/gpiojsonsvc.sock raw '{"id":"1","action":"get","target":"GPIO1_A0"}'
```

`init`/`query`/`get`/`set`/`raw` each open a new connection and send one request. Use the wizard or `script` for `init` followed by `get`/`set` on the same session. Query's repeatable `--pin` selects names; omit it for all. The wizard and canned commands allocate increasing string IDs unless `--id` is set on a one-shot command. `raw` and `script` send JSON as written.

Python unit tests (no live service):

```bash
python3 tools/test_debug_client.py
```

## Tests

```bash
cargo fmt
cargo test
cargo clippy --all-targets
```

## Deferred

Not implemented yet:

- Process-wide shared-read / exclusive-write locks
- Protocol trigger `filter`
- Optional Cargo feature gate for the mock backend

The live protocol is documented in [docs/protocol.md](docs/protocol.md). Outstanding query behavior is tracked in [docs/TODO](docs/TODO).
