# Mock chip XML format

With `--mock`, each `[pins.gpiod]` `device` path is one XML file for one chip. The service loads that file at startup, keeps process-wide state keyed by the path, and writes line state back to the same file. Protocol pin strings are still exact TOML keys; the XML `id` is chip metadata only.

How those files sit in the process: [architecture.md](architecture.md). Pin map and `--mock`: [../README.md](../README.md).

## Document shape

The root must be a single `<gpiochip>`. A `<gpiochips>` wrapper, a second `<gpiochip>`, or any other root is rejected.

```xml
<gpiochip id="gpiochip0" label="mock gpiochip0">
    <line id="0" name="line0" direction="input" bias="pull_up">L</line>
    <line id="7" name="line7" direction="output" drive="push_pull">H</line>
</gpiochip>
```

Each `<line>` holds physical level as element text `H` or `L`. A self-closing `<line/>` is invalid; an empty `<line ...></line>` is treated as `L`. Line `id` values are unsigned offsets (`u32`) and must be unique within the chip.

Startup checks that every mapped `device` exists, parses as this format, and contains the mapped `line`. Several pin keys may share one file with different `line` values. Opening the same path twice shares one mock chip; different paths are independent chips.

## `<gpiochip>` attributes

| Attribute | Required | Meaning |
| --- | --- | --- |
| `id` | yes | Chip name (`get_name`). Non-empty. Not used to resolve protocol pins. |
| `label` | no | Chip label (`get_label`). Omitted or empty becomes `""`. |

Unknown chip attributes are rejected.

## `<line>` attributes

| Attribute | Required | Meaning |
| --- | --- | --- |
| `id` | yes | Line offset. Must parse as `u32`. Must match `[pins.gpiod].line` for mapped pins. |
| `name` | no | Line name. Omitted or empty becomes `""`. |
| `consumer` | no | Request consumer string. Omitted or empty becomes `""`. The mock also writes this while a request holds the line. |
| `direction` | no | `input` (default) or `output`. `as_is` is invalid. |
| `bias` | input only | `disabled` (default), `pull_up`, `pull_down`. Forbidden on output lines. `as_is` / `unknown` are invalid. |
| `drive` | output only | `push_pull` (default), `open_drain`, `open_source`. Forbidden on input lines. |
| `active_low` | no | `true` or `false` (default). |

Unknown line attributes are rejected (`event` and similar are not part of this file).

When the mock serializes a snapshot it writes a normalized form: chip `id`, `label` only if non-empty, every line with `id`, optional `name` / `consumer`, `direction`, `bias` or `drive`, and always `active_low`.

## Physical level vs logical value

Line text is the **physical** level, not the protocol 0/1 bit:

| Text | Physical |
| --- | --- |
| `H` | high |
| `L` | low |

Logical `Active` / `Inactive` follow `active_low`:

- `active_low="false"`: `H` is active, `L` is inactive
- `active_low="true"`: `L` is active, `H` is inactive

Protocol `get` / `set` and trigger events use the logical value. The file always stores `H` / `L`.

## Query metadata and ownership

Within one backend, repeated opens and backend clones share chip state and one
polling watcher by the exact configured device path. Independent backend
instances remain isolated. Active requests determine `is_used` and `consumer`,
including requests from other service sessions. Dropping a request releases its
ownership. A consumer attribute stored in XML does not by itself mark a line
used; a free line reports a null consumer.

Query returns the effective input/output direction and does not request lines,
rewrite XML, or append write-log entries. Chip snapshots and watchers remain
cached until the backend and outstanding chip handles are dropped. The mock
does not simulate kernel or external-process reservations. External file edits
only update input levels through polling; metadata and structure are not merged
into a cached chip snapshot. To change those fixtures, restart the mock service.

## Persistence and external edits

Output `set` and line-request property changes rewrite the chip XML on disk. `GPIOJSONSVC_MOCK_LOG` records snapshots after output writes, including explicit initial and final values. Property-only changes are not logged.

A file watcher reloads the XML. Changing an **input** line’s `H`/`L` is treated as an external edge (converted with the **baseline** `active_low`). Changing only names, labels, direction, bias, drive, or `active_low`, or changing an **output** line’s level, does not emit an input event.

## Minimal examples

Input with defaults (`direction` omitted → input, `bias` omitted → disabled, `active_low` omitted → false):

```xml
<gpiochip id="gpiochip0">
    <line id="0">L</line>
</gpiochip>
```

Active-low output:

```xml
<gpiochip id="gpiochip0" label="mock">
    <line id="13" name="GPIO1_B5" direction="output" drive="open_drain" active_low="true">L</line>
</gpiochip>
```
