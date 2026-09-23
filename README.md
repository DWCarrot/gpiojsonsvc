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
gpio-consumer = "svc_{id}"

[pins.gpiod]
"gpiochip0:7" = { device = "/path/to/gpiochip0.xml", line = 7 }
"GPIO1_B5" = { device = "/path/to/gpiochip1.xml", line = 13 }
```

Rules:

- Pin keys are matched exactly (no case folding, trimming, or `chip:line` parsing).
- Keys and `device` strings must be non-empty; `line` must be a `u32`.
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

## Run

```bash
gpiojsonsvc
gpiojsonsvc /path/to/gpiojsonsvc.toml
gpiojsonsvc --mock /path/to/gpiojsonsvc.toml
GPIOJSONSVC_MOCK_LOG=/tmp/mock-write.log gpiojsonsvc --mock /path/to/gpiojsonsvc.toml
```

`--mock` is required until the real backend exists. `--mock` enables the file-backed mock. `GPIOJSONSVC_MOCK_LOG` also enables the mock backend write log at that path; it is an error if that variable is set without `--mock`. The process listens until SIGINT, then closes live sessions, removes the socket, and exits. It also removes a stale socket file before bind.

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

The unit's `ExecStart` does not pass `--mock`. Current builds still fail without it until the real backend exists; add `--mock` to `ExecStart` for a mock deployment. The config's `socket` must be `/run/gpiojsonsvc/service.sock`, matching the runtime directory.

## Protocol (summary)

Each request has a non-empty `id` and an `action`. `init` must succeed once per connection before `get` or `set`. Pin strings in `init` must match `[pins.gpiod]` keys.

```json
{"id":"1","action":"init","target":{"LED":{"mode":"output","pin":"GPIO1_B5","initial":1,"final":0}}}
{"id":"2","action":"get","target":"IN"}
{"id":"3","action":"set","target":{"LED":1}}
```

Combined `input`/`output` targets take up to eight unduplicated pins packed into a `u8` (first pin is the high bit). Output targets may also set optional packed `initial` and `final` `u8` fields; omit `initial` to leave the line unchanged at request time, and omit `final` to skip a close write. Configured `final` values are applied on graceful session close (disconnect, session shutdown, and service shutdown). Trigger targets take a single pin. Stepped `set` is an array of objects; step 0 must not include `lag`, later steps must. The `ok` reply is sent after the last step is applied. Trigger events reuse the `init` request `id`.

Full request and response shapes: [docs/protocol.md](docs/protocol.md). Layers and session flow: [docs/architecture.md](docs/architecture.md).

## Debug client

Usage, session rules, and examples: [docs/debug_client.md](docs/debug_client.md).

With no subcommand (or `repl`), the client opens one socket and walks `init` / `get` / `set` field by field. Use `raw` to paste a multi-line request object, including its `action`, ended by a blank line.

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

- Process-wide shared-read / exclusive-write locks
- Protocol trigger `filter`
- Real `libgpiod` backend (`src/gpio/sys.rs`)
- Optional Cargo feature gate for the mock backend

`.cursor/general-instrument.md` records earlier product intent; it is not the live protocol.
