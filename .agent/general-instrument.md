# Historical intent (not the live protocol)

This file is an early product sketch. Authoritative behavior is implemented in the crate and documented in `README.md`, `docs/protocol.md`, and `docs/architecture.md`.

Differences that landed in the running service include: request field `action` (`init` / `get` / `set`) plus a required `id`; opaque pin strings resolved only through TOML `[pins.gpiod]`; combined pin arrays packed into `u8`; stepped `set` as a top-level array that replies `ok` after the first step; trigger `event` messages that reuse the `init` id.

**Still deferred (do not treat the sections below as implemented):**

1. Output-default semantics, cancelling in-flight sequences on disconnect, and restoring pins to defaults when the client leaves.
2. Process-wide shared-read / exclusive-write locks keyed by physical pin.
3. Trigger software `filter` (debounce or similar) on the wire protocol.
4. Real `libgpiod` FFI (`src/gpio/sys.rs`); without `--mock` the process refuses to start.

---

# Description

A simple service for controlling gpio pins with json requests over unix domain socket.
Written in Rust.
Backend with libgpiod or libmraa, with c-bindings.

# General Principles

- connect with SOCK_STREAM
- echo line is a json and end with "\r\n" for send and receive

## Workflow

1. when connect, client send "setup" to setup the service

```json
{
    "type": "setup",
    "pins": {
        "GPIO4_B3": {
            "pin": 4,
            "role": "input",
        },
        "GPIO4_B4": {
            "pin": 5,
            "role": "output",
            "default": 0,
        },
        "GPIO4_B5": {
            "pin": 6,
            "role": "trigger",
            "condition": "rising" | "falling" | "both",
            "filter": ...
        },
        ...
    }
}
```

- setup must be done before any other command, otherwise the service will return "error".
- "setup" is a one-time command, means after the setup, the service will not accept any other "setup" command.
- pin names can be specific any name, but must be unique.
- "pin" in single pins item is the physical pin index of the pin, it can be pin number, or gpio name + line number for libgpiod.
- "role" in single pins item can be "input", "output", "trigger"
 - "input" means to set the pin as input mode, and later read the value of the pin
 - "output" means to set the pin as output mode, and later write the value to the pin
 - "trigger" means to set the pin as trigger mode, and later when the pin is triggered, the service will send the trigger event to the client
- when a client setup, it will create a "lock" for pins.
 - "input" and "trigger" will use a "read lock", means other clients can read the value of the pin, but not write to it.
 - "output" will use a "write lock", means other clients can not read the value of the pin, nor write to it.
- when a client is done  (disconnect), it will release the "lock".


2. client send "get" to get the current state of specific pin

```json
{
    "type": "get",
    "pin": "GPIO4_B3",
}

{
    "type": "get",
    "pins": [
        "GPIO4_B3",
        "GPIO4_B4",
    ]
}
```

- "get" will return the current state of the pin/pins, and the state is 1 or 0.
- pin/pins must be setup before "get", allowing "input", "output" or "trigger", otherwise the service will return "error".

3. client send "set" to set the state of specific pin

```json
{
    "type": "set",
    "pins": {
        "GPIO4_B3": 1,
        "GPIO4_B4": 0,
        "GPIO2_B5": [
            {
                "value": 1,
            },
            {
                "lag": 100,
                "value": 0,
            },
            {
                "lag": 200,
                "value": 1,
            },
        ],
    },  
}
```

- "set" will set the state of the pin/pins, and the state is 1 or 0.
- pin/pins must be setup before "set", allowing "output" only, otherwise the service will return "error".
- "lag" is the time in milliseconds to delay the state change.
- "value" is the state to set the pin/pins to.
- if "lag" is not set, the state change will be immediate.
- if "lag" is set, the state change will be delayed by the "lag" time.
- if "lag" is set, the state change will be delayed by the "lag" time.
- this request will not response until all the state changes are completed.
- when the client is done (disconnect) and a set action is in progress, the service will stop immediately and set the pin/pins to the default value.


4. service send "event" to send the event to the client

```json
{
    "type": "event",
    "pin": "GPIO4_B3",
    "event": "rising" | "falling" | "both",
}
```

- "event" will send the event to the client, and the event is "rising", "falling" or "both".
- "event" will be sent when the pin is triggered.

## interface

interface design for GPIO backend. *Notice: this is not the final code*

```

interface GPIOHandle {

    async read(this, pins: Iterator<GPIOPin>) -> Result<[PinLevel], GpioError>;

    async write(this, values: Iterator<(GPIOPin, PinLevel)>) -> Result<(), GpioError>;

    async wait_event(this, timeout_ms?) -> Result<EdgeEvent, GpioError>;
}

interface GPIOBackend {

    specific-type: GPIOPin; // each GPIOBackend type have its own type of GPIOPin
    specific-type: GPIOHandle; // each GPIOBackend type have its own type of GPIOHandle

    async pin(this, pin_name: &str) -> Result<GPIOPin, GpioError>;

    async config(this, options: Iterator<(GPIOPin, PinConfig)>) -> Result<GPIOHandle, GpioError>;
}

```

pin name is a string representing one single pin name exsit in setup item value, not the setup item key.

when a setup happend, pin name will be extract from the setup item value, and the corresponding `GPIOPin` will be find with `GPIOBackend::pin`. then the `GPIOPin` will be used to config the `GPIOHandle`, read or write. when read or write operation, target name will be mapped to the `GPIOPin`, and used in the `GPIOHandle::read` or `GPIOHandle::write` operation.

so that, `GPIOPin` should be easy to clone.



# coding style

- each import occupying one line, and each line is a single import.