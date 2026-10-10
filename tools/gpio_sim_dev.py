#!/usr/bin/env python3
"""Create and control the gpio-sim topology used for local development.

WARNING: Never run this helper, or tests that depend on it, on a system with
hardware GPIO controllers.
"""

from __future__ import annotations

import argparse
import grp
import json
import os
from pathlib import Path
import pwd
import re
import shlex
import shutil
import stat
import subprocess
import sys
import time
from typing import Any


CONFIGFS_ROOT = Path("/sys/kernel/config/gpio-sim")
STATE_DIR = Path("/run/gpiojsonsvc-gpio-sim")
STATE_FILE = STATE_DIR / "state.json"
DEFAULT_CONFIG = (
    Path(__file__).resolve().parents[1] / "assets" / "mock" / "gpio_sim.json"
)
SAFE_NAME = re.compile(r"^[A-Za-z0-9_.-]+$")
LEVEL_TO_PULL = {"H": "pull-up", "L": "pull-down"}
VALUE_TO_LEVEL = {"1": "H", "0": "L"}


class DevError(Exception):
    """An expected command-line or environment error."""


def write_control(path: Path, value: str) -> None:
    with path.open("w", encoding="utf-8") as control:
        control.write(f"{value}\n")


def read_control(path: Path) -> str:
    return path.read_text(encoding="utf-8").strip()


def require_root() -> None:
    if os.geteuid() != 0:
        raise DevError("this command must run as root (use sudo)")


def load_config(path: Path) -> dict[str, Any]:
    try:
        data = json.loads(path.read_text(encoding="utf-8"))
    except FileNotFoundError as error:
        raise DevError(f"configuration file not found: {path}") from error
    except json.JSONDecodeError as error:
        raise DevError(f"invalid JSON in {path}: {error}") from error

    if not isinstance(data, dict):
        raise DevError("configuration must be a JSON object")

    simulator = data.get("simulator")
    if not isinstance(simulator, str) or not SAFE_NAME.fullmatch(simulator):
        raise DevError("'simulator' must contain only letters, digits, '.', '_' or '-'")

    chips = data.get("chips")
    if not isinstance(chips, list) or not chips:
        raise DevError("'chips' must be a non-empty array")

    chip_names: set[str] = set()
    line_names: set[str] = set()
    for chip_index, chip in enumerate(chips):
        if not isinstance(chip, dict):
            raise DevError(f"chips[{chip_index}] must be an object")

        chip_name = chip.get("name")
        if not isinstance(chip_name, str) or not SAFE_NAME.fullmatch(chip_name):
            raise DevError(
                f"chips[{chip_index}].name must contain only letters, digits, '.', '_' or '-'"
            )
        if chip_name in chip_names:
            raise DevError(f"duplicate chip name: {chip_name}")
        chip_names.add(chip_name)

        lines = chip.get("lines")
        if not isinstance(lines, list) or not lines:
            raise DevError(f"chip '{chip_name}' must contain a non-empty lines array")

        for line_index, line in enumerate(lines):
            if not isinstance(line, dict):
                raise DevError(f"{chip_name}.lines[{line_index}] must be an object")
            line_name = line.get("name")
            level = line.get("value")
            if (
                not isinstance(line_name, str)
                or not line_name
                or "\n" in line_name
                or "\0" in line_name
            ):
                raise DevError(
                    f"{chip_name}.lines[{line_index}].name must be a non-empty single-line string"
                )
            if line_name in line_names:
                raise DevError(f"duplicate line name: {line_name}")
            if level not in LEVEL_TO_PULL:
                raise DevError(f"line '{line_name}' value must be 'H' or 'L'")
            line_names.add(line_name)

    return data


def load_state() -> dict[str, Any]:
    try:
        state = json.loads(STATE_FILE.read_text(encoding="utf-8"))
    except FileNotFoundError as error:
        raise DevError("development topology is not running") from error
    except json.JSONDecodeError as error:
        raise DevError(f"invalid runtime state in {STATE_FILE}: {error}") from error

    if not isinstance(state, dict) or state.get("version") != 1:
        raise DevError(f"unsupported runtime state in {STATE_FILE}")
    return state


def resolve_access_group(requested: str | None) -> tuple[str, int]:
    if requested:
        try:
            entry = grp.getgrnam(requested)
        except KeyError as error:
            raise DevError(f"group '{requested}' does not exist") from error
        return entry.gr_name, entry.gr_gid

    caller = os.environ.get("SUDO_USER")
    if not caller or caller == "root":
        raise DevError("cannot infer an access group; pass --group explicitly")
    try:
        user = pwd.getpwnam(caller)
        entry = grp.getgrgid(user.pw_gid)
    except KeyError as error:
        raise DevError(f"cannot find the primary group for sudo user '{caller}'") from error
    return entry.gr_name, entry.gr_gid


def wait_for_device(device: Path) -> None:
    for _ in range(100):
        try:
            if stat.S_ISCHR(device.stat().st_mode):
                return
        except FileNotFoundError:
            pass
        time.sleep(0.05)
    raise DevError(f"timed out waiting for character device '{device}'")


def sysfs_line_dir(state: dict[str, Any], chip: dict[str, Any], offset: int) -> Path:
    return (
        Path("/sys/devices/platform")
        / state["device_name"]
        / chip["kernel_chip"]
        / f"sim_gpio{offset}"
    )


def find_chip(state: dict[str, Any], requested: str) -> dict[str, Any]:
    for chip in state["chips"]:
        if chip["name"] == requested:
            return chip
    choices = ", ".join(chip["name"] for chip in state["chips"])
    raise DevError(f"unknown chip '{requested}' (expected one of: {choices})")


def find_line(state: dict[str, Any], requested: str) -> tuple[dict[str, Any], int]:
    for chip in state["chips"]:
        for offset, line_name in enumerate(chip["lines"]):
            if line_name == requested:
                return chip, offset
    raise DevError(f"unknown pin '{requested}'")


def read_level(state: dict[str, Any], chip: dict[str, Any], offset: int) -> str:
    value_path = sysfs_line_dir(state, chip, offset) / "value"
    try:
        value = read_control(value_path)
    except FileNotFoundError as error:
        raise DevError(f"gpio-sim value not found: {value_path}") from error
    try:
        return VALUE_TO_LEVEL[value]
    except KeyError as error:
        raise DevError(f"unexpected gpio-sim value '{value}' in {value_path}") from error


def chip_values(state: dict[str, Any], chip: dict[str, Any]) -> list[dict[str, Any]]:
    return [
        {
            "offset": offset,
            "pin": line_name,
            "value": read_level(state, chip, offset),
        }
        for offset, line_name in enumerate(chip["lines"])
    ]


def remove_runtime(chips: list[dict[str, Any]]) -> None:
    for chip in chips:
        link = STATE_DIR / chip["name"]
        if link.is_symlink():
            link.unlink()
    if STATE_FILE.exists():
        STATE_FILE.unlink()
    try:
        STATE_DIR.rmdir()
    except FileNotFoundError:
        pass


def remove_configfs(simulator: str, chips: list[dict[str, Any]]) -> None:
    sim_root = CONFIGFS_ROOT / simulator
    if not sim_root.is_dir():
        return

    live = sim_root / "live"
    if live.exists():
        try:
            write_control(live, "0")
        except OSError:
            pass

    for chip_index, chip in enumerate(chips):
        bank = sim_root / f"gpio-bank{chip_index}"
        for offset in range(len(chip["lines"])):
            try:
                (bank / f"line{offset}").rmdir()
            except FileNotFoundError:
                pass
        try:
            bank.rmdir()
        except FileNotFoundError:
            pass
    sim_root.rmdir()


def start(config: dict[str, Any], requested_group: str | None) -> None:
    require_root()
    if not CONFIGFS_ROOT.is_dir():
        raise DevError(
            f"{CONFIGFS_ROOT} is unavailable; boot a kernel with CONFIG_GPIO_SIM=y"
        )

    simulator = config["simulator"]
    sim_root = CONFIGFS_ROOT / simulator
    if sim_root.exists():
        raise DevError(f"{sim_root} already exists; use status or reset")
    if STATE_DIR.exists():
        raise DevError(f"{STATE_DIR} already exists; use status or reset")

    group_name, group_id = resolve_access_group(requested_group)
    runtime_chips: list[dict[str, Any]] = []
    created_chips: list[dict[str, Any]] = []

    try:
        STATE_DIR.mkdir(mode=0o755)
        sim_root.mkdir()

        for chip_index, chip_config in enumerate(config["chips"]):
            bank = sim_root / f"gpio-bank{chip_index}"
            bank.mkdir()
            created_chips.append(chip_config)
            write_control(bank / "num_lines", str(len(chip_config["lines"])))
            for offset, line in enumerate(chip_config["lines"]):
                line_dir = bank / f"line{offset}"
                line_dir.mkdir()
                write_control(line_dir / "name", line["name"])

        write_control(sim_root / "live", "1")
        if shutil.which("udevadm"):
            subprocess.run(["udevadm", "settle"], check=False)

        device_name = read_control(sim_root / "dev_name")
        for chip_index, chip_config in enumerate(config["chips"]):
            bank = sim_root / f"gpio-bank{chip_index}"
            kernel_chip = read_control(bank / "chip_name")
            if not re.fullmatch(r"gpiochip[0-9]+", kernel_chip):
                raise DevError(f"unexpected gpio-sim chip name '{kernel_chip}'")

            device = Path("/dev") / kernel_chip
            wait_for_device(device)
            os.chown(device, 0, group_id)
            os.chmod(device, 0o660)

            link = STATE_DIR / chip_config["name"]
            link.symlink_to(device)
            runtime_chips.append(
                {
                    "name": chip_config["name"],
                    "bank": f"gpio-bank{chip_index}",
                    "kernel_chip": kernel_chip,
                    "device": str(device),
                    "link": str(link),
                    "lines": [line["name"] for line in chip_config["lines"]],
                }
            )

        state = {
            "version": 1,
            "simulator": simulator,
            "device_name": device_name,
            "access_group": group_name,
            "chips": runtime_chips,
        }

        for chip_index, chip_config in enumerate(config["chips"]):
            chip = runtime_chips[chip_index]
            for offset, line in enumerate(chip_config["lines"]):
                pull_path = sysfs_line_dir(state, chip, offset) / "pull"
                write_control(pull_path, LEVEL_TO_PULL[line["value"]])
                os.chown(pull_path, 0, group_id)
                os.chmod(pull_path, 0o664)

        STATE_FILE.write_text(json.dumps(state, indent=2) + "\n", encoding="utf-8")
        os.chmod(STATE_FILE, 0o644)
    except BaseException:
        try:
            remove_configfs(simulator, created_chips)
        finally:
            remove_runtime(runtime_chips)
        raise

    print("gpio-sim development topology started")
    status()


def stop(config: dict[str, Any] | None) -> None:
    require_root()
    if STATE_FILE.exists():
        state = load_state()
        remove_configfs(state["simulator"], state["chips"])
        remove_runtime(state["chips"])
    else:
        # This also cleans up a topology left behind by an interrupted start.
        if config is None:
            raise DevError("configuration is required to clean up an incomplete start")
        simulator = config["simulator"]
        sim_root = CONFIGFS_ROOT / simulator
        if sim_root.exists():
            remove_configfs(simulator, config["chips"])
        if STATE_DIR.exists():
            STATE_DIR.rmdir()
    print("gpio-sim development topology stopped")


def reset(config: dict[str, Any], requested_group: str | None) -> None:
    require_root()
    stop(config)
    start(config, requested_group)


def status() -> None:
    state = load_state()
    live_path = CONFIGFS_ROOT / state["simulator"] / "live"
    live = read_control(live_path) if live_path.is_file() else "missing"
    print(f"gpio-sim development topology is running (live={live})")
    print(f"access group: {state['access_group']}")
    print(f"{'LOGICAL':<16} {'KERNEL':<12} PATH")
    for chip in state["chips"]:
        print(f"{chip['name']:<16} {chip['kernel_chip']:<12} {chip['link']}")


def get_level(target: str, output_format: str) -> None:
    state = load_state()

    try:
        chip = find_chip(state, target)
    except DevError:
        chip = None

    if chip is not None:
        values = chip_values(state, chip)
        if output_format == "json":
            print(json.dumps({"chip": chip["name"], "lines": values}, indent=2))
        else:
            for line in values:
                print(line["value"])
        return

    chip, offset = find_line(state, target)
    value = read_level(state, chip, offset)
    if output_format == "json":
        print(json.dumps({"pin": target, "value": value}))
    else:
        print(value)


def set_level(pin: str, level: str) -> None:
    state = load_state()
    chip, offset = find_line(state, pin)
    pull_path = sysfs_line_dir(state, chip, offset) / "pull"
    try:
        write_control(pull_path, LEVEL_TO_PULL[level])
    except FileNotFoundError as error:
        raise DevError(f"gpio-sim pull control not found: {pull_path}") from error
    except PermissionError as error:
        raise DevError(
            f"permission denied writing gpio-sim pull control: {pull_path}; "
            "grant write access to this file or run set with sudo"
        ) from error
    print(f"{pin}={level}")


def parse_gpioinfo(detail: str) -> tuple[str, dict[str, Any]]:
    try:
        tokens = shlex.split(detail)
    except ValueError as error:
        raise DevError(f"cannot parse gpioinfo line '{detail}': {error}") from error
    if len(tokens) < 2:
        raise DevError(f"cannot parse gpioinfo line '{detail}'")

    line_name = tokens[0]
    direction = tokens[1]
    if direction not in {"input", "output"}:
        raise DevError(f"unexpected gpioinfo direction '{direction}'")

    attributes: dict[str, Any] = {"direction": direction}
    for token in tokens[2:]:
        if "=" in token:
            key, value = token.split("=", 1)
        else:
            key, value = token, True
        key = key.replace("-", "_")
        if not key or key in attributes:
            raise DevError(f"invalid or duplicate gpioinfo attribute '{token}'")
        attributes[key] = value

    return line_name, attributes


def gpioinfo_records(chip: dict[str, Any]) -> tuple[str, list[dict[str, Any]]]:
    device = Path(chip["link"])
    result = subprocess.run(
        ["gpioinfo", "-c", str(device)],
        check=False,
        capture_output=True,
        text=True,
    )
    if result.returncode != 0:
        detail = result.stderr.strip()
        suffix = f": {detail}" if detail else ""
        raise DevError(f"gpioinfo exited with status {result.returncode}{suffix}")

    header = ""
    by_offset: dict[int, dict[str, Any]] = {}
    for output_line in result.stdout.splitlines():
        match = re.match(r"^\s*line\s+(\d+):\s*(.*)$", output_line)
        if match is None:
            if output_line.strip() and not header:
                header = output_line
            continue
        offset = int(match.group(1))
        line_name, attributes = parse_gpioinfo(match.group(2))
        by_offset[offset] = {
            "text": output_line,
            "name": line_name,
            "info": attributes,
        }

    records = []
    for offset in range(len(chip["lines"])):
        try:
            record = by_offset[offset]
        except KeyError as error:
            raise DevError(f"gpioinfo did not report line offset {offset}") from error
        records.append(
            {
                "offset": offset,
                "name": record["name"],
                "info": record["info"],
                "text": record["text"],
            }
        )
    return header, records


def dump(chip_name: str, output_format: str) -> None:
    state = load_state()
    chip = find_chip(state, chip_name)
    device = Path(chip["link"])
    try:
        if not stat.S_ISCHR(device.stat().st_mode):
            raise DevError(f"GPIO character device is unavailable: {device}")
    except FileNotFoundError as error:
        raise DevError(f"GPIO character device is unavailable: {device}") from error
    if not shutil.which("gpioinfo"):
        raise DevError("gpioinfo is not installed")

    header, records = gpioinfo_records(chip)
    for record in records:
        record["value"] = read_level(state, chip, record["offset"])

    if output_format == "json":
        print(
            json.dumps(
                {
                    "chip": chip["name"],
                    "kernel_chip": chip["kernel_chip"],
                    "path": chip["link"],
                    "lines": [
                        {
                            "offset": record["offset"],
                            "name": record["name"],
                            "value": record["value"],
                            "info": record["info"],
                        }
                        for record in records
                    ],
                },
                indent=2,
            )
        )
        return

    if header:
        print(header)
    for record in records:
        print(f"{record['text']} value={record['value']}")


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        description="Create and control gpio-sim devices for gpiojsonsvc development.",
        epilog=(
            "WARNING: Never run this helper, or tests that depend on it, "
            "when hardware GPIO controllers are present."
        ),
    )
    parser.add_argument(
        "--config",
        type=Path,
        default=DEFAULT_CONFIG,
        help=f"topology JSON file (default: {DEFAULT_CONFIG})",
    )
    subparsers = parser.add_subparsers(dest="command", required=True)

    start_parser = subparsers.add_parser("start", help="create the configured topology")
    start_parser.add_argument(
        "--group", help="existing group allowed to open GPIO devices and set simulated inputs"
    )

    reset_parser = subparsers.add_parser("reset", help="stop and recreate the topology")
    reset_parser.add_argument(
        "--group", help="existing group allowed to open GPIO devices and set simulated inputs"
    )

    subparsers.add_parser("stop", help="remove the topology and stable links")
    subparsers.add_parser("status", help="show logical-to-kernel chip mappings")

    dump_parser = subparsers.add_parser(
        "dump", help="show kernel metadata and physical state for a chip"
    )
    dump_parser.add_argument("chip", help="configured chip name, such as gpiochip0")
    dump_parser.add_argument(
        "--format", choices=("text", "json"), default="text", help="output format"
    )

    set_parser = subparsers.add_parser("set", help="set a simulated input level")
    set_parser.add_argument("pin", help="configured pin name, such as gpiochip0:1")
    set_parser.add_argument("level", choices=("H", "L"), help="H=high, L=low")

    get_parser = subparsers.add_parser("get", help="read one pin or an entire chip")
    get_parser.add_argument(
        "target", help="configured pin or chip name, such as gpiochip0:1 or gpiochip0"
    )
    get_parser.add_argument(
        "--format", choices=("text", "json"), default="text", help="output format"
    )
    return parser


def run(args: argparse.Namespace) -> None:
    if args.command in {"start", "reset"}:
        config = load_config(args.config)
    elif args.command == "stop" and not STATE_FILE.exists():
        config = load_config(args.config)
    else:
        config = None

    if args.command == "start":
        start(config, args.group)
    elif args.command == "reset":
        reset(config, args.group)
    elif args.command == "stop":
        stop(config)
    elif args.command == "status":
        status()
    elif args.command == "dump":
        dump(args.chip, args.format)
    elif args.command == "set":
        set_level(args.pin, args.level)
    elif args.command == "get":
        get_level(args.target, args.format)


def main() -> int:
    args = build_parser().parse_args()
    try:
        run(args)
    except DevError as error:
        print(f"gpio_sim_dev: {error}", file=sys.stderr)
        return 1
    except OSError as error:
        detail = error.strerror or str(error)
        target = f": {error.filename}" if error.filename else ""
        print(f"gpio_sim_dev: {detail}{target}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
