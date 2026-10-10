# Non-root development with `gpio-sim`

> [!CAUTION]
> Never run `tools/gpio_sim_dev.py`, use this simulated-device configuration,
> or run tests that depend on either of them on a system where hardware GPIO
> controllers are present. This workflow is only for development systems with
> no hardware GPIO.

`gpio-sim` topology changes are privileged configfs operations, but opening an
existing GPIO character device does not need to be. The Python helper creates
the configured chips as root, grants device and simulated-input control access
to a normal group, and exposes stable paths for `gpiojsonsvc`. It uses only
the Python 3 standard library.

The helper is intentionally manual. It does not install a systemd unit, change
udev rules, add groups, or configure passwordless sudo.

## Configure the simulated chips

The default topology is defined in `assets/mock/gpio_sim.json`. It contains two
chips with eight lines each. Chip names become stable links under
`/run/gpiojsonsvc-gpio-sim/`; line names are the exact pin names accepted by
the `set` and `get` commands. Each line's initial `value` must be `H` (high) or
`L` (low):

```json
{
  "simulator": "gpiojsonsvc-dev",
  "chips": [
    {
      "name": "gpiochip0",
      "lines": [
        { "name": "gpiochip0:0", "value": "L" },
        { "name": "gpiochip0:1", "value": "H" }
      ]
    }
  ]
}
```

To use another topology file, put `--config` before the command:

```bash
sudo tools/gpio_sim_dev.py --config /path/to/topology.json start
```

The service's pin map is separate. Keep the chip paths and line offsets in
`assets/mock/config.toml` aligned with the topology JSON after changing it.

## Start the development topology

From the repository root:

```bash
sudo tools/gpio_sim_dev.py start
```

When `--group` is omitted, the helper uses the sudo caller's primary group.
Both `start` and `reset` assign the character devices to `root:<group>` with
mode `0660`, and each line's sysfs `pull` file to `root:<group>` with mode
`0664`. This lets group members request lines and inject simulated input
levels without sudo. Permissions are applied directly after creation; no
udev rule is needed. The default configuration creates these stable paths:

```text
/run/gpiojsonsvc-gpio-sim/gpiochip0
/run/gpiojsonsvc-gpio-sim/gpiochip1
```

The kernel's actual `/dev/gpiochipN` allocation may differ. Callers should use
the stable paths rather than infer kernel chip numbers. To grant access to a
different existing group, pass it explicitly:

```bash
sudo tools/gpio_sim_dev.py start --group gpio
```

Show the current mapping without root:

```bash
tools/gpio_sim_dev.py status
```

## Dump chip state

Dump kernel line metadata together with gpio-sim's physical level:

```bash
tools/gpio_sim_dev.py dump gpiochip0
```

This does not require root. Each output line is the corresponding `gpioinfo`
line with `value=H` or `value=L` appended. It therefore includes line names,
consumers, direction, active-low state, edge configuration, bias, and drive
flags when the kernel exposes them, as well as the gpio-sim physical value.

An idle line does not have an active libgpiod request, so request-specific bias
or drive flags may be absent from `gpioinfo`. The external gpio-sim pull is not
printed as line metadata: it determines the appended physical value, while a
bias shown by `gpioinfo` is part of the current libgpiod line configuration.

For machine-readable output, select JSON:

```bash
tools/gpio_sim_dev.py dump gpiochip0 --format json
```

Each JSON line object contains its offset, name, physical value, and normalized
`gpioinfo` attributes. Attribute names use `snake_case`, flags such as
`active-low` become booleans, and quoted values such as consumer names become
plain JSON strings. For example:

```json
{
  "offset": 1,
  "name": "gpiochip0:1",
  "value": "L",
  "info": {
    "direction": "input",
    "bias": "disabled",
    "edges": "both",
    "consumer": "gpiomon"
  }
}
```

## Run as a normal user

The development service configuration uses the stable chip paths and a
user-writable socket in `/tmp`:

```bash
cargo run -- assets/mock/config.toml
```

Do not run Cargo, VS Code, Codex, or `gpiojsonsvc` with sudo. Once the topology
is started, normal libgpiod operations—including line requests, input reads,
output writes, queries, and edge waits—use the group-accessible character
devices.

### Codex sandbox note

Codex commands normally run with an isolated `/dev` and restricted Unix-socket
access. A dynamically created host `/dev/gpiochipN` may therefore be invisible
inside the sandbox even though the Linux user and group permissions are
correct. GPIO integration commands should be approved to run outside that
sandbox. They still run as the normal user (for example, UID 1000), not as
root.

Prefer narrow reusable approvals such as `cargo test`, `cargo run`, the
`gpiojsonsvc` executable, and `python3 tools/debug_client.py`. Do not approve a
general shell or run Codex as root. Commands that do not touch the simulated
devices can continue to use the normal sandbox.

The generic pin names are `gpiochip0:0` through `gpiochip0:7` and `gpiochip1:0`
through `gpiochip1:7`. The current test suite still selects the file-backed
mock explicitly. Tests converted to `gpio-sim` can use the stable paths in
`assets/mock/config.toml`. Tests sharing these chips must serialize conflicting
line requests or allocate distinct lines.

## Set and get a simulated input

Set a line's externally simulated level by its exact pin name. This writes a
gpio-sim sysfs `pull` control and requires write permission on that file:

```bash
tools/gpio_sim_dev.py set gpiochip0:1 H
tools/gpio_sim_dev.py set gpiochip0:1 L
```

These commands work as a normal user in the group selected by `start` or
`reset`. For a topology created by an older helper, recreate it with
`sudo tools/gpio_sim_dev.py reset` (and `--group` if needed) when existing
requests have finished. The command reports the affected path if write
permission is denied.

`H` means high (`pull-up`) and `L` means low (`pull-down`). Read one physical
level without root:

```bash
tools/gpio_sim_dev.py get gpiochip0:1
```

`get` writes only `H` or `L` to standard output, which makes it convenient in
shell scripts:

```bash
level="$(tools/gpio_sim_dev.py get gpiochip0:1)"
```

Pass a chip name to read all its lines in offset order. Text output contains
one `H` or `L` value per line:

```bash
tools/gpio_sim_dev.py get gpiochip0
```

JSON output includes the pin names and offsets and is intended for tools:

```bash
tools/gpio_sim_dev.py get gpiochip0 --format json
tools/gpio_sim_dev.py get gpiochip0:1 --format json
```

Automated tests that inject input transitions can run as a normal user in the
selected group. `start` and `reset` grant access to the simulated lines' `pull`
files each time the topology is created. Tests should check actual control
access rather than require root.

Privilege summary:

- `status`, `dump`, and `get`: no root required.
- `gpioinfo`, `gpioget`, `gpiomon`, `gpiojsonsvc`, Cargo, and VS Code: no root
  required after `start` grants device access.
- `set`: write access to the line's sysfs `pull` file required; root is not
  required when that access has been granted.
- `start`, `reset`, and `stop`: root required to manage the configfs topology.

## Reset or stop

Reset all simulated line state by recreating the topology:

```bash
sudo tools/gpio_sim_dev.py reset
```

Stop it when finished:

```bash
sudo tools/gpio_sim_dev.py stop
```

The runtime directory is under `/run` and the configfs topology disappears on
WSL shutdown. Run `start` once after each WSL restart.
