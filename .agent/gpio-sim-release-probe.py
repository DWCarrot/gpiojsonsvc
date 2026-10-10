#!/usr/bin/env python3
"""Research probe: libgpiod 2.x C ABI on existing gpiojsonsvc gpio-sim only."""
import ctypes as C
import ctypes.util
import json
import os
from pathlib import Path
import platform
import stat
import time


lib = C.CDLL(ctypes.util.find_library("gpiod") or "libgpiod.so.3", use_errno=True)
P = C.c_void_p
U = C.c_uint
I = C.c_int
Z = C.c_size_t


def bind(name, result, *args):
    fn = getattr(lib, name)
    fn.restype = result
    fn.argtypes = args
    return fn


version = bind("gpiod_api_version", C.c_char_p)
chip_open = bind("gpiod_chip_open", P, C.c_char_p)
chip_close = bind("gpiod_chip_close", None, P)
line_info = bind("gpiod_chip_get_line_info", P, P, U)
info_free = bind("gpiod_line_info_free", None, P)
info_used = bind("gpiod_line_info_is_used", C.c_bool, P)
info_direction = bind("gpiod_line_info_get_direction", I, P)
info_consumer = bind("gpiod_line_info_get_consumer", C.c_char_p, P)
settings_new = bind("gpiod_line_settings_new", P)
settings_free = bind("gpiod_line_settings_free", None, P)
settings_direction = bind("gpiod_line_settings_set_direction", I, P, I)
settings_output = bind("gpiod_line_settings_set_output_value", I, P, I)
settings_bias = bind("gpiod_line_settings_set_bias", I, P, I)
config_new = bind("gpiod_line_config_new", P)
config_free = bind("gpiod_line_config_free", None, P)
config_add = bind("gpiod_line_config_add_line_settings", I, P, C.POINTER(U), Z, P)
req_config_new = bind("gpiod_request_config_new", P)
req_config_free = bind("gpiod_request_config_free", None, P)
req_consumer = bind("gpiod_request_config_set_consumer", None, P, C.c_char_p)
request_lines = bind("gpiod_chip_request_lines", P, P, P, P)
release = bind("gpiod_line_request_release", None, P)
set_value = bind("gpiod_line_request_set_value", I, P, U, I)
get_value = bind("gpiod_line_request_get_value", I, P, U)

# Constants verified against /usr/include/gpiod.h (libgpiod 2.1.3).
INPUT, OUTPUT = 2, 3
PULL_UP, PULL_DOWN = 4, 5


def pointer(value):
    if not value:
        err = C.get_errno()
        raise OSError(err, os.strerror(err))
    return value


def check(value):
    if value < 0:
        err = C.get_errno()
        raise OSError(err, os.strerror(err))
    return value


def open_chip(path):
    return pointer(chip_open(os.fsencode(path)))


def request(chip, offset, direction, initial=0, bias=None):
    settings = config = req_cfg = None
    try:
        settings = pointer(settings_new())
        config = pointer(config_new())
        req_cfg = pointer(req_config_new())
        req_consumer(req_cfg, b"gpiojsonsvc-release-research")
        check(settings_direction(settings, direction))
        if direction == OUTPUT:
            check(settings_output(settings, initial))
        if bias is not None:
            check(settings_bias(settings, bias))
        offsets = (U * 1)(offset)
        check(config_add(config, offsets, 1, settings))
        return pointer(request_lines(chip, req_cfg, config))
    finally:
        if req_cfg:
            req_config_free(req_cfg)
        if config:
            config_free(config)
        if settings:
            settings_free(settings)


def metadata(path, offset):
    chip = open_chip(path)
    info = None
    try:
        info = pointer(line_info(chip, offset))
        consumer = info_consumer(info)
        return {"used": bool(info_used(info)),
                "direction": {INPUT: "input", OUTPUT: "output"}[info_direction(info)],
                "consumer": consumer.decode() if consumer else None}
    finally:
        if info:
            info_free(info)
        chip_close(chip)


def snapshot(path, offset, sysline):
    return {"value": int((sysline / "value").read_text().strip()),
            "pull": (sysline / "pull").read_text().strip(),
            **metadata(path, offset)}


def prepare_input(path, offset, pull):
    chip = open_chip(path)
    req = None
    try:
        req = request(chip, offset, INPUT,
                      bias=PULL_UP if pull == "pull-up" else PULL_DOWN)
    finally:
        if req:
            release(req)
        chip_close(chip)


def emit(kind, **data):
    print(json.dumps({"kind": kind, **data}, sort_keys=True), flush=True)


def run_case(path, offset, sysline, name, baseline, output, bias=None, chip_first=False):
    prepare_input(path, offset, "pull-up" if baseline else "pull-down")
    before = snapshot(path, offset, sysline)
    chip = req = None
    try:
        chip = open_chip(path)
        req = request(chip, offset, OUTPUT, initial=baseline, bias=bias)
        check(set_value(req, offset, output))
        logical = check(get_value(req, offset))
        held = snapshot(path, offset, sysline)
        emit("observation", case=name, stage="before_request", state=before)
        emit("observation", case=name, stage="after_set_held", state=held, logical=logical)
        assert held["value"] == output and logical == output and held["used"]
        if chip_first:
            chip_close(chip)
            chip = None
            closed_chip = snapshot(path, offset, sysline)
            assert check(get_value(req, offset)) == output
            emit("observation", case=name, stage="chip_closed_request_held", state=closed_chip)
            assert closed_chip["value"] == output and closed_chip["used"]
        release(req)
        req = None
        after_release = snapshot(path, offset, sysline)
        emit("observation", case=name, stage="request_released", state=after_release)
        if chip:
            chip_close(chip)
            chip = None
        both_closed = snapshot(path, offset, sysline)
        emit("observation", case=name, stage="both_closed", state=both_closed)
        time.sleep(0.1)
        delayed = snapshot(path, offset, sysline)
        emit("observation", case=name, stage="both_closed_after_100ms", state=delayed)
        expected = 1 if held["pull"] == "pull-up" else 0
        for state in (after_release, both_closed, delayed):
            assert state["value"] == expected and not state["used"]
    finally:
        if req:
            release(req)
        if chip:
            chip_close(chip)


def main():
    state = json.loads(Path("/run/gpiojsonsvc-gpio-sim/state.json").read_text())
    assert state["simulator"] == "gpiojsonsvc-dev"
    platform_dir = Path("/sys/devices/platform") / state["device_name"]
    assert (platform_dir / "driver").resolve().name == "gpio-sim"
    for device in Path("/sys/bus/gpio/devices").glob("gpiochip*"):
        assert "/gpio-sim." in str(device.resolve()), f"Non-simulator GPIO: {device}"
    emit("environment", kernel=platform.release(), libgpiod=version().decode(),
         effective_uid=os.geteuid(), observer="read-only gpio-sim sysfs value/pull")
    for logical_name in ("gpiochip0", "gpiochip1"):
        entry = next(c for c in state["chips"] if c["name"] == logical_name)
        path = Path(entry["link"])
        assert stat.S_ISCHR(path.stat().st_mode)
        device_sys = Path(f"/sys/dev/char/{os.major(path.stat().st_rdev)}:{os.minor(path.stat().st_rdev)}").resolve()
        assert platform_dir.resolve() in device_sys.parents
        # Restrict the probe to an unowned input so cleanup can restore its state.
        offset = 7
        sysline = platform_dir / entry["kernel_chip"] / f"sim_gpio{offset}"
        original = snapshot(path, offset, sysline)
        assert not original["used"] and original["direction"] == "input", original
        assert original["value"] == (1 if original["pull"] == "pull-up" else 0)
        emit("original", chip=logical_name, offset=offset, state=original)
        try:
            cases = [
                ("pull_low_set_high", 0, 1, None, False),
                ("pull_high_set_low", 1, 0, None, False),
                ("before_low_output_bias_high_set_low", 0, 0, PULL_UP, False),
                ("close_chip_first", 0, 1, None, True),
            ]
            for name, baseline, value, bias, chip_first in cases:
                run_case(path, offset, sysline, f"{logical_name}:{offset}/{name}",
                         baseline, value, bias, chip_first)
        finally:
            prepare_input(path, offset, original["pull"])
            restored = snapshot(path, offset, sysline)
            emit("restored", chip=logical_name, offset=offset, state=restored)
            assert restored == original, (original, restored)
    emit("result", status="PASS", cases=8)


if __name__ == "__main__":
    main()
