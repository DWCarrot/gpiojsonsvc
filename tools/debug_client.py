#!/usr/bin/env python3

from __future__ import annotations

import argparse
import json
import select
import socket
import sys
from collections.abc import Callable, Iterable, Iterator
from pathlib import Path
from typing import Any, TextIO


DEFAULT_SOCKET_PATH = "/tmp/gpiojsonsvc.sock"

IncomingKind = str
Message = dict[str, Any]


class RequestIdAllocator:
    """Monotonic string IDs for canned commands."""

    def __init__(self, start: int = 1) -> None:
        self._next = start

    def next_id(self) -> str:
        request_id = str(self._next)
        self._next += 1
        return request_id


def canned_request_id(explicit_id: str | None, allocator: RequestIdAllocator) -> str:
    if explicit_id is not None and explicit_id.strip():
        return explicit_id
    return allocator.next_id()


def build_init_request(
    target: dict[str, Any],
    request_id: str,
) -> dict[str, Any]:
    if not target:
        raise ValueError("init target must contain at least one named target")
    return {
        "id": request_id,
        "action": "init",
        "target": target,
    }


def build_target_config(
    mode: str,
    pin: str | list[str],
    *,
    bias: str | None = None,
    drive: str | None = None,
    edge: str | None = None,
) -> dict[str, Any]:
    config: dict[str, Any] = {"mode": mode, "pin": pin}
    if mode == "input" and bias is not None:
        config["bias"] = bias
    if mode == "output" and drive is not None:
        config["drive"] = drive
    if mode == "trigger" and edge is not None:
        config["edge"] = edge
    return config


def build_get_request(
    target: str | list[str],
    request_id: str,
) -> dict[str, Any]:
    if isinstance(target, list):
        if not target:
            raise ValueError("get target list must not be empty")
        if len(target) == 1:
            target = target[0]
    elif not target:
        raise ValueError("get target must not be empty")
    return {
        "id": request_id,
        "action": "get",
        "target": target,
    }


def build_set_request(
    target: dict[str, int] | list[dict[str, Any]],
    request_id: str,
) -> dict[str, Any]:
    if not target:
        raise ValueError("set target must not be empty")
    return {
        "id": request_id,
        "action": "set",
        "target": target,
    }


class InteractiveQuit(Exception):
    """Raised when the wizard action is quit/exit."""


_INPUT_MODES = ("input", "output", "trigger")
_INPUT_BIASES = ("as_is", "disabled", "pull_up", "pull_down")
_OUTPUT_DRIVES = ("push_pull", "open_drain", "open_source")
_TRIGGER_EDGES = ("rising", "falling", "both")
_SET_FORMS = ("immediate", "stepped")

Readline = Callable[[str], str]


def _interactive_help() -> None:
    print("Interactive requests (one field at a time, or raw JSON):")
    print("  init      configure named targets")
    print("  get       read target values")
    print("  set       write immediate or stepped values")
    print("  raw       paste a request object (multi-line, end with an empty line)")
    print("  help      show this message")
    print("  quit      leave the session (also: exit, Ctrl-D)")


def _prompt_line(readline: Readline, prompt: str, *, required: bool = False) -> str:
    while True:
        value = readline(prompt).strip()
        if value or not required:
            return value


def _prompt_choice(
    readline: Readline,
    prompt: str,
    choices: tuple[str, ...],
    *,
    allow_empty: bool = False,
) -> str | None:
    allowed = ", ".join(choices)
    while True:
        value = _prompt_line(readline, prompt, required=not allow_empty)
        if not value:
            return None
        if value in choices:
            return value
        print(f"expected one of: {allowed}")


def _prompt_yes(readline: Readline, prompt: str) -> bool:
    while True:
        value = _prompt_line(readline, prompt).lower()
        if not value or value in ("n", "no"):
            return False
        if value in ("y", "yes"):
            return True
        print("expected y or n")


def _parse_space_split(raw: str) -> str | list[str]:
    tokens = raw.split()
    if len(tokens) == 1:
        return tokens[0]
    return tokens


def _prompt_pin(readline: Readline, mode: str) -> str | list[str]:
    while True:
        raw = _prompt_line(readline, "pin (space-split): ", required=True)
        pin = _parse_space_split(raw)
        if mode == "trigger" and isinstance(pin, list):
            raise ValueError("trigger must be a single pin")
        return pin


def _prompt_name_value_pairs(readline: Readline) -> dict[str, int]:
    pairs: dict[str, int] = {}
    while True:
        name = _prompt_line(readline, "target name: ", required=not pairs)
        if not name:
            return pairs
        while True:
            raw_value = _prompt_line(readline, "value: ", required=True)
            try:
                pairs[name] = int(raw_value)
            except ValueError:
                print("value must be an integer")
                continue
            break


def _prompt_lag_ms(readline: Readline) -> int:
    while True:
        raw = _prompt_line(readline, "lag (ms): ", required=True)
        try:
            lag = int(raw)
        except ValueError:
            print("lag must be a non-zero integer")
            continue
        if lag == 0:
            print("lag must be a non-zero integer")
            continue
        return lag


def _read_multiline_json(readline: Readline) -> Any:
    """Read JSON until a blank line. Leading blank lines are ignored."""

    print("JSON (end with an empty line):")
    lines: list[str] = []
    while True:
        line = readline("")
        if line.strip() == "":
            if lines:
                break
            continue
        lines.append(line)

    try:
        return json.loads("\n".join(lines))
    except json.JSONDecodeError as error:
        raise ValueError(f"invalid JSON: {error}") from error


def _build_raw_request(request_id: str, parsed: Any) -> dict[str, Any]:
    if not isinstance(parsed, dict):
        raise ValueError("raw JSON must be a request object")
    if parsed.get("action") not in ("init", "get", "set"):
        raise ValueError("raw JSON action must be init, get, or set")

    payload = dict(parsed)
    req_id = payload.get("id")
    if not isinstance(req_id, str) or not req_id.strip():
        payload["id"] = request_id
    return payload


def _build_raw_interactively(request_id: str, readline: Readline) -> dict[str, Any]:
    return _build_raw_request(request_id, _read_multiline_json(readline))


def _build_init_interactively(request_id: str, readline: Readline) -> dict[str, Any]:
    target: dict[str, Any] = {}
    while True:
        name = _prompt_line(readline, "target name: ", required=not target)
        if not name:
            break
        mode = _prompt_choice(readline, "mode [input/output/trigger]: ", _INPUT_MODES)
        assert mode is not None
        pin = _prompt_pin(readline, mode)
        bias = drive = edge = None
        if mode == "input":
            bias = _prompt_choice(
                readline,
                "bias [as_is/disabled/pull_up/pull_down, empty=omit]: ",
                _INPUT_BIASES,
                allow_empty=True,
            )
        elif mode == "output":
            drive = _prompt_choice(
                readline,
                "drive [push_pull/open_drain/open_source, empty=omit]: ",
                _OUTPUT_DRIVES,
                allow_empty=True,
            )
        else:
            edge = _prompt_choice(
                readline,
                "edge [rising/falling/both, empty=omit]: ",
                _TRIGGER_EDGES,
                allow_empty=True,
            )
        target[name] = build_target_config(
            mode,
            pin,
            bias=bias,
            drive=drive,
            edge=edge,
        )
        if not _prompt_yes(readline, "add another target? [y/N]: "):
            break
    return build_init_request(target, request_id)


def _build_get_interactively(request_id: str, readline: Readline) -> dict[str, Any]:
    raw = _prompt_line(readline, "target (space-split names): ", required=True)
    return build_get_request(_parse_space_split(raw), request_id)


def _build_set_interactively(request_id: str, readline: Readline) -> dict[str, Any]:
    form = _prompt_choice(readline, "form [immediate/stepped]: ", _SET_FORMS)
    assert form is not None
    if form == "immediate":
        return build_set_request(_prompt_name_value_pairs(readline), request_id)

    steps: list[dict[str, Any]] = [dict(_prompt_name_value_pairs(readline))]
    while _prompt_yes(readline, "add another step? [y/N]: "):
        lag = _prompt_lag_ms(readline)
        step: dict[str, Any] = {"lag": lag}
        step.update(_prompt_name_value_pairs(readline))
        steps.append(step)
    return build_set_request(steps, request_id)


def build_request_interactively(
    request_id: str,
    readline: Readline | None = None,
) -> dict[str, Any]:
    """Walk protocol fields and return one init/get/set request.

    ``readline`` defaults to :func:`input`. Empty optional fields are omitted.
    Invalid actions and validation errors print and return to the action prompt.
    ``help`` is local-only. ``quit`` / ``exit`` raise :class:`InteractiveQuit`.
    ``raw`` reads a multi-line request object ended by a blank line.
    """

    if readline is None:
        readline = input

    while True:
        action = _prompt_line(
            readline,
            "action [init/get/set/raw, help/quit]: ",
        ).lower()
        if not action:
            continue
        if action in ("quit", "exit"):
            raise InteractiveQuit()
        if action == "help":
            _interactive_help()
            continue

        if action not in ("init", "get", "set", "raw"):
            print("expected init, get, set, raw, help, or quit")
            continue

        try:
            if action == "raw":
                return _build_raw_interactively(request_id, readline)
            if action == "init":
                return _build_init_interactively(request_id, readline)
            if action == "get":
                return _build_get_interactively(request_id, readline)
            return _build_set_interactively(request_id, readline)
        except ValueError as error:
            print(error)


def is_event_message(payload: dict[str, Any]) -> bool:
    return payload.get("status") == "event"


def classify_incoming(payload: dict[str, Any], request_id: str | None) -> IncomingKind:
    if is_event_message(payload):
        return "event"
    if request_id is None or payload.get("id") == request_id:
        return "response"
    return "unsolicited"


def wait_for_matching_response(
    messages: Iterable[dict[str, Any]],
    request_id: str | None,
    on_unsolicited: Callable[[IncomingKind, dict[str, Any]], None] | None = None,
) -> dict[str, Any]:
    """Consume messages until a non-event response matching ``request_id``.

    Event messages (including those that reuse an init request id) are reported
    through ``on_unsolicited`` and skipped. Other non-matching messages are
    reported the same way. If ``request_id`` is None (raw payloads without an
    id), the first non-event message is treated as the response.
    """

    for payload in messages:
        kind = classify_incoming(payload, request_id)
        if kind == "response":
            return payload
        if on_unsolicited is not None:
            on_unsolicited(kind, payload)

    raise RuntimeError("server closed the connection without a matching response")


class DebugClient:
    def __init__(self, socket_path: str, timeout: float) -> None:
        self.socket_path = socket_path
        self.timeout = timeout
        self.sock: socket.socket | None = None
        self._recv_buffer = bytearray()

    def __enter__(self) -> DebugClient:
        sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        sock.settimeout(self.timeout)
        sock.connect(self.socket_path)
        sock.settimeout(None)
        self.sock = sock
        return self

    def __exit__(self, exc_type, exc, traceback) -> None:
        if self.sock is not None:
            self.sock.close()
            self.sock = None

    def _require_sock(self) -> socket.socket:
        if self.sock is None:
            raise RuntimeError("client is not connected")
        return self.sock

    def _pop_complete_line(self) -> str | None:
        newline = self._recv_buffer.find(b"\n")
        if newline < 0:
            return None
        line = self._recv_buffer[: newline + 1]
        del self._recv_buffer[: newline + 1]
        return line.decode("utf-8")

    def _socket_readable(self, timeout: float | None) -> bool:
        sock = self._require_sock()
        ready, _, _ = select.select([sock], [], [], timeout)
        return sock in ready

    def _recv_into_buffer(self) -> bool:
        """Read one chunk into the recv buffer. Returns False on EOF."""

        sock = self._require_sock()
        try:
            chunk = sock.recv(4096)
        except BlockingIOError:
            return True
        if not chunk:
            return False
        self._recv_buffer.extend(chunk)
        return True

    def _take_eof_remainder(self) -> str | None:
        if not self._recv_buffer:
            return None
        line = bytes(self._recv_buffer).decode("utf-8")
        self._recv_buffer.clear()
        return line

    def _readline(self, timeout: float | None = None) -> str | None:
        """Read one UTF-8 line, waiting with ``select`` until data or timeout.

        ``timeout`` of ``None`` uses the client timeout. Partial lines stay
        buffered across timeouts.
        """

        wait = self.timeout if timeout is None else timeout
        while True:
            line = self._pop_complete_line()
            if line is not None:
                return line
            if not self._socket_readable(wait):
                raise TimeoutError("timed out waiting for socket data")
            if not self._recv_into_buffer():
                return self._take_eof_remainder()

    def _print_socket_line(self, response_line: str) -> None:
        payload = json.loads(response_line)
        kind: IncomingKind = "event" if is_event_message(payload) else "unsolicited"
        print_unsolicited(kind, payload)

    def _flush_socket_messages(self) -> None:
        """Print complete messages that are already buffered or immediately readable."""

        while True:
            line = self._pop_complete_line()
            if line is not None:
                self._print_socket_line(line)
                continue
            if not self._socket_readable(0):
                return
            if not self._recv_into_buffer():
                remainder = self._take_eof_remainder()
                if remainder is not None:
                    self._print_socket_line(remainder)
                raise RuntimeError("server closed the connection")

    def prompt_line(self, prompt: str = "", *, stdin: TextIO | None = None) -> str:
        """Read one stdin line while printing live socket events."""

        sock = self._require_sock()
        input_stream = sys.stdin if stdin is None else stdin
        self._flush_socket_messages()
        sys.stdout.write(prompt)
        sys.stdout.flush()
        while True:
            ready, _, _ = select.select([input_stream, sock], [], [])
            if sock in ready:
                if not self._recv_into_buffer():
                    remainder = self._take_eof_remainder()
                    if remainder is not None:
                        self._print_socket_line(remainder)
                    raise RuntimeError("server closed the connection")
                while True:
                    line = self._pop_complete_line()
                    if line is None:
                        break
                    self._print_socket_line(line)
                    sys.stdout.write(prompt)
                    sys.stdout.flush()
            if input_stream in ready:
                typed = input_stream.readline()
                if typed == "":
                    raise EOFError
                return typed.rstrip("\r\n")

    def _incoming_messages(self) -> Iterator[dict[str, Any]]:
        self._require_sock()
        while True:
            response_line = self._readline()
            if response_line is None:
                return
            yield json.loads(response_line)

    def send(self, payload: dict[str, Any]) -> dict[str, Any]:
        sock = self._require_sock()
        message = json.dumps(payload, separators=(",", ":")) + "\r\n"
        sock.sendall(message.encode("utf-8"))

        request_id = payload.get("id")
        if not isinstance(request_id, str) or not request_id.strip():
            request_id = None

        return wait_for_matching_response(
            self._incoming_messages(),
            request_id,
            on_unsolicited=print_unsolicited,
        )

    def drain_pending(self, timeout: float = 1) -> None:
        """Print buffered events without waiting long for more traffic."""

        self._require_sock()
        try:
            while True:
                response_line = self._readline(timeout)
                if response_line is None:
                    raise RuntimeError("server closed the connection")
                self._print_socket_line(response_line)
        except TimeoutError:
            return


def parse_json_object(raw: str, context: str) -> dict[str, Any]:
    try:
        value = json.loads(raw)
    except json.JSONDecodeError as error:
        raise SystemExit(f"invalid JSON for {context}: {error}") from error

    if not isinstance(value, dict):
        raise SystemExit(f"{context} must be a JSON object")

    return value


def parse_json_array(raw: str, context: str) -> list[Any]:
    try:
        value = json.loads(raw)
    except json.JSONDecodeError as error:
        raise SystemExit(f"invalid JSON for {context}: {error}") from error

    if not isinstance(value, list):
        raise SystemExit(f"{context} must be a JSON array")

    return value


def parse_pin_selector(args: argparse.Namespace) -> str | list[str]:
    if args.pin and args.pins:
        raise SystemExit("use either --pin or --pins, not both")
    if args.pin:
        return args.pin
    if args.pins:
        return list(args.pins)
    raise SystemExit("init requires --pin, --pins, or --target-json")


def build_init_from_args(args: argparse.Namespace) -> dict[str, Any]:
    request_id = canned_request_id(args.request_id, args.id_allocator)

    if args.target_json:
        if args.name or args.mode or args.pin or args.pins:
            raise SystemExit("use either --target-json or --name/--mode/--pin, not both")
        target = parse_json_object(args.target_json, "init target")
        try:
            return build_init_request(target, request_id)
        except ValueError as error:
            raise SystemExit(str(error)) from error

    if not args.name or not args.mode:
        raise SystemExit("init requires --target-json, or --name and --mode with --pin/--pins")

    pin = parse_pin_selector(args)
    config = build_target_config(
        args.mode,
        pin,
        bias=args.bias,
        drive=args.drive,
        edge=args.edge,
    )
    try:
        return build_init_request({args.name: config}, request_id)
    except ValueError as error:
        raise SystemExit(str(error)) from error


def build_get_from_args(args: argparse.Namespace) -> dict[str, Any]:
    request_id = canned_request_id(args.request_id, args.id_allocator)
    try:
        return build_get_request(list(args.target), request_id)
    except ValueError as error:
        raise SystemExit(str(error)) from error


def build_set_from_args(args: argparse.Namespace) -> dict[str, Any]:
    request_id = canned_request_id(args.request_id, args.id_allocator)
    specified = sum(
        1
        for value in (args.target_json, args.steps_json, args.target)
        if value is not None
    )
    if specified > 1:
        raise SystemExit("use one of --target/--value, --target-json, or --steps-json")

    if args.steps_json is not None:
        steps = parse_json_array(args.steps_json, "set steps")
        try:
            return build_set_request(steps, request_id)
        except ValueError as error:
            raise SystemExit(str(error)) from error

    if args.target_json is not None:
        target = parse_json_object(args.target_json, "set target")
        try:
            return build_set_request(target, request_id)
        except ValueError as error:
            raise SystemExit(str(error)) from error

    if args.target is not None:
        if args.value is None:
            raise SystemExit("set with --target requires --value")
        try:
            return build_set_request({args.target: args.value}, request_id)
        except ValueError as error:
            raise SystemExit(str(error)) from error

    raise SystemExit("set requires --target/--value, --target-json, or --steps-json")


def read_payload_lines_from_file(path: str) -> list[dict[str, Any]]:
    payloads = []

    for line_number, line in enumerate(Path(path).read_text(encoding="utf-8").splitlines(), start=1):
        stripped = line.strip()
        if not stripped or stripped.startswith("#"):
            continue

        payloads.append(parse_json_object(stripped, f"{path}:{line_number}"))

    if not payloads:
        raise SystemExit(f"no JSON requests found in {path}")

    return payloads


def print_message(prefix: str, payload: dict[str, Any]) -> None:
    formatted = json.dumps(payload, indent=2, sort_keys=True)
    print(f"{prefix}:\n{formatted}")


def print_unsolicited(kind: IncomingKind, payload: dict[str, Any]) -> None:
    print_message(kind, payload)


def run_single_request(args: argparse.Namespace, payload: dict[str, Any]) -> int:
    with DebugClient(args.socket, args.timeout) as client:
        print_message("request", payload)
        response = client.send(payload)
        print_message("response", response)
    return 0


def run_script(args: argparse.Namespace) -> int:
    payloads = read_payload_lines_from_file(args.file)

    with DebugClient(args.socket, args.timeout) as client:
        for payload in payloads:
            print_message("request", payload)
            response = client.send(payload)
            print_message("response", response)

    return 0


def run_repl(
    args: argparse.Namespace,
    readline: Readline | None = None,
) -> int:
    with DebugClient(args.socket, args.timeout) as client:
        if readline is None:
            readline = client.prompt_line

        print(f"Connected to {args.socket}")
        print("Field-by-field requests on this session, or raw for a JSON request.")
        print("Type help or quit.")

        while True:
            try:
                payload = build_request_interactively(
                    canned_request_id(None, args.id_allocator),
                    readline,
                )
            except InteractiveQuit:
                break
            except EOFError:
                print()
                break

            print_message("request", payload)
            response = client.send(payload)
            print_message("response", response)

    return 0


def add_request_id_argument(parser: argparse.ArgumentParser) -> None:
    parser.add_argument(
        "--id",
        dest="request_id",
        default=None,
        help="Request id (default: monotonically increasing 1, 2, ...)",
    )


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        description="Simple Unix socket debug client for gpiojsonsvc."
    )
    parser.add_argument(
        "--socket",
        default=DEFAULT_SOCKET_PATH,
        help=f"Unix socket path (default: {DEFAULT_SOCKET_PATH})",
    )
    parser.add_argument(
        "--timeout",
        type=float,
        default=5.0,
        help="Socket timeout in seconds",
    )

    parser.set_defaults(handler=run_repl)
    subparsers = parser.add_subparsers(dest="command", required=False)

    repl_parser = subparsers.add_parser(
        "repl",
        help="Open an interactive wizard session (same as omitting a command)",
    )
    repl_parser.set_defaults(handler=run_repl)

    script_parser = subparsers.add_parser(
        "script",
        help="Send one JSON request per line from a file",
    )
    script_parser.add_argument("file", help="Path to a script file")
    script_parser.set_defaults(handler=run_script)

    raw_parser = subparsers.add_parser(
        "raw",
        help="Send one raw JSON request object (id is not rewritten)",
    )
    raw_parser.add_argument("json", help="JSON object to send")
    raw_parser.set_defaults(
        build_payload=lambda args: parse_json_object(args.json, "raw request"),
        handler=lambda args: run_single_request(args, args.build_payload(args)),
    )

    init_parser = subparsers.add_parser(
        "init",
        help="Build and send an init request",
    )
    add_request_id_argument(init_parser)
    init_parser.add_argument(
        "--target-json",
        help="JSON object for the init target field",
    )
    init_parser.add_argument("--name", help="Single logical target name")
    init_parser.add_argument(
        "--mode",
        choices=("input", "output", "trigger"),
        help="Mode for a single --name target",
    )
    init_parser.add_argument("--pin", help="Single physical pin string")
    init_parser.add_argument(
        "--pins",
        nargs="+",
        help="Combined physical pin list (input/output)",
    )
    init_parser.add_argument("--bias", help="Optional input bias (as_is, disabled, pull_up, pull_down)")
    init_parser.add_argument("--drive", help="Optional output drive (push_pull, open_drain, open_source)")
    init_parser.add_argument("--edge", help="Optional trigger edge (rising, falling, both)")
    init_parser.set_defaults(
        build_payload=build_init_from_args,
        handler=lambda args: run_single_request(args, args.build_payload(args)),
    )

    get_parser = subparsers.add_parser(
        "get",
        help="Build and send a get request",
    )
    add_request_id_argument(get_parser)
    get_parser.add_argument(
        "--target",
        nargs="+",
        required=True,
        help="One or more logical target names",
    )
    get_parser.set_defaults(
        build_payload=build_get_from_args,
        handler=lambda args: run_single_request(args, args.build_payload(args)),
    )

    set_parser = subparsers.add_parser(
        "set",
        help="Build and send a set request",
    )
    add_request_id_argument(set_parser)
    set_parser.add_argument("--target", help="Single output target name")
    set_parser.add_argument(
        "--value",
        type=int,
        help="Immediate output value for --target",
    )
    set_parser.add_argument(
        "--target-json",
        help="JSON object of immediate target-to-value pairs",
    )
    set_parser.add_argument(
        "--steps-json",
        help="JSON array of stepped-set objects (first step has no lag)",
    )
    set_parser.set_defaults(
        build_payload=build_set_from_args,
        handler=lambda args: run_single_request(args, args.build_payload(args)),
    )

    return parser


def main(argv: list[str] | None = None) -> int:
    parser = build_parser()
    args = parser.parse_args(argv)
    args.id_allocator = RequestIdAllocator()
    return args.handler(args)


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (FileNotFoundError, ConnectionError, OSError) as error:
        print(f"debug client error: {error}", file=sys.stderr)
        raise SystemExit(1) from error
