#!/usr/bin/env python3

"""Unit tests only; never run GPIO-dependent tests on hardware GPIO systems."""

import unittest

from gpio_sim_dev import DevError, parse_gpioinfo


class ParseGpioinfoTests(unittest.TestCase):
    def test_parses_requested_input(self) -> None:
        name, info = parse_gpioinfo(
            '"gpiochip0:1" input bias=disabled edges=both consumer="gpiomon"'
        )

        self.assertEqual(name, "gpiochip0:1")
        self.assertEqual(
            info,
            {
                "direction": "input",
                "bias": "disabled",
                "edges": "both",
                "consumer": "gpiomon",
            },
        )

    def test_normalizes_flags_keys_and_quoted_values(self) -> None:
        name, info = parse_gpioinfo(
            '"pin 1" output active-low drive=open-drain '
            'event-clock=realtime debounce-period=10ms consumer="tool name"'
        )

        self.assertEqual(name, "pin 1")
        self.assertEqual(
            info,
            {
                "direction": "output",
                "active_low": True,
                "drive": "open-drain",
                "event_clock": "realtime",
                "debounce_period": "10ms",
                "consumer": "tool name",
            },
        )

    def test_rejects_missing_direction(self) -> None:
        with self.assertRaisesRegex(DevError, "cannot parse"):
            parse_gpioinfo('"gpiochip0:1"')

    def test_rejects_duplicate_attributes(self) -> None:
        with self.assertRaisesRegex(DevError, "duplicate"):
            parse_gpioinfo('"gpiochip0:1" input bias=disabled bias=pull-up')


if __name__ == "__main__":
    unittest.main()
