#!/usr/bin/env python3

import json
import os
import socket
import threading
import time
import unittest
from argparse import Namespace
from unittest.mock import MagicMock, patch

import debug_client as dc


class RequestConstructionTests(unittest.TestCase):
    def test_init_request_uses_id_action_target_and_mode(self) -> None:
        target = {
            "GPIO4_B3": dc.build_target_config("input", "gpiochip0:2", bias="as_is"),
            "GPIO4_B5": dc.build_target_config("output", "gpiochip2:1", drive="push_pull"),
            "GPIO1_A2": dc.build_target_config("trigger", "gpiochip1:1", edge="rising"),
            "CombinedIN": dc.build_target_config(
                "input",
                ["gpiochip0:3", "gpiochip1:4"],
                bias="pull_up",
            ),
        }

        request = dc.build_init_request(target, "init-1")

        self.assertEqual(request["id"], "init-1")
        self.assertEqual(request["action"], "init")
        self.assertEqual(request["target"]["GPIO4_B3"]["mode"], "input")
        self.assertEqual(request["target"]["GPIO4_B3"]["pin"], "gpiochip0:2")
        self.assertEqual(request["target"]["GPIO4_B5"]["mode"], "output")
        self.assertEqual(request["target"]["GPIO1_A2"]["edge"], "rising")
        self.assertEqual(
            request["target"]["CombinedIN"]["pin"],
            ["gpiochip0:3", "gpiochip1:4"],
        )

    def test_get_single_target_stays_a_string(self) -> None:
        request = dc.build_get_request("GPIO4_B3", "2")
        self.assertEqual(
            request,
            {"id": "2", "action": "get", "target": "GPIO4_B3"},
        )

    def test_get_collapses_single_item_list(self) -> None:
        request = dc.build_get_request(["GPIO4_B3"], "3")
        self.assertEqual(request["target"], "GPIO4_B3")

    def test_get_multiple_targets_uses_array(self) -> None:
        request = dc.build_get_request(["GPIO4_B3", "CombinedIN2"], "get-1")
        self.assertEqual(
            request,
            {
                "id": "get-1",
                "action": "get",
                "target": ["GPIO4_B3", "CombinedIN2"],
            },
        )

    def test_set_immediate_object_shape(self) -> None:
        request = dc.build_set_request({"GPIO4_B5": 1, "GPIO4_B6": 2}, "set-2")
        self.assertEqual(request["action"], "set")
        self.assertEqual(request["target"], {"GPIO4_B5": 1, "GPIO4_B6": 2})
        self.assertIsInstance(request["target"], dict)

    def test_set_stepped_array_shape(self) -> None:
        steps = [
            {"GPIO4_B5": 1, "GPIO4_B6": 1},
            {"lag": 100, "GPIO4_B5": 0, "GPIO4_B6": 1},
            {"lag": 200, "GPIO4_B5": 1, "GPIO4_B6": 0},
        ]
        request = dc.build_set_request(steps, "set-1")
        self.assertEqual(request["id"], "set-1")
        self.assertEqual(request["action"], "set")
        self.assertEqual(request["target"], steps)
        self.assertIsInstance(request["target"], list)
        self.assertNotIn("lag", request["target"][0])
        self.assertEqual(request["target"][1]["lag"], 100)


class RequestIdAllocatorTests(unittest.TestCase):
    def test_canned_commands_get_monotonically_increasing_ids(self) -> None:
        allocator = dc.RequestIdAllocator()
        first = dc.build_init_request(
            {"LED": dc.build_target_config("output", "gpiochip0:1")},
            dc.canned_request_id(None, allocator),
        )
        second = dc.build_get_request("LED", dc.canned_request_id(None, allocator))
        third = dc.build_set_request({"LED": 1}, dc.canned_request_id(None, allocator))

        self.assertEqual(first["id"], "1")
        self.assertEqual(second["id"], "2")
        self.assertEqual(third["id"], "3")

    def test_explicit_id_is_not_consumed_from_allocator(self) -> None:
        allocator = dc.RequestIdAllocator()
        request = dc.build_get_request("LED", dc.canned_request_id("custom", allocator))
        self.assertEqual(request["id"], "custom")
        self.assertEqual(allocator.next_id(), "1")

    def test_raw_payload_is_not_rewritten(self) -> None:
        raw = '{"id":"user-9","action":"get","target":"GPIO4_B4"}'
        payload = dc.parse_json_object(raw, "raw request")
        self.assertEqual(payload["id"], "user-9")
        self.assertNotIn("type", payload)
        self.assertEqual(payload["action"], "get")


class CliBuilderTests(unittest.TestCase):
    def _namespace(self, **overrides: object) -> Namespace:
        values = {
            "request_id": None,
            "id_allocator": dc.RequestIdAllocator(),
            "target_json": None,
            "name": None,
            "mode": None,
            "pin": None,
            "pins": None,
            "bias": None,
            "drive": None,
            "edge": None,
            "target": None,
            "value": None,
            "steps_json": None,
        }
        values.update(overrides)
        return Namespace(**values)

    def test_init_cli_single_target(self) -> None:
        args = self._namespace(name="GPIO4_B5", mode="output", pin="gpiochip2:1", drive="push_pull")
        request = dc.build_init_from_args(args)
        self.assertEqual(request["id"], "1")
        self.assertEqual(request["action"], "init")
        self.assertEqual(request["target"]["GPIO4_B5"]["mode"], "output")

    def test_set_cli_steps_json(self) -> None:
        args = self._namespace(
            steps_json='[{"GPIO4_B5":1},{"lag":100,"GPIO4_B5":0}]',
        )
        request = dc.build_set_from_args(args)
        self.assertEqual(request["id"], "1")
        self.assertEqual(request["target"][0], {"GPIO4_B5": 1})
        self.assertEqual(request["target"][1]["lag"], 100)


class CorrelationTests(unittest.TestCase):
    def test_skips_events_until_matching_non_event_id(self) -> None:
        incoming = [
            {
                "id": "init-1",
                "status": "event",
                "event": {"target": "GPIO4_B5", "type": "rising"},
            },
            {"id": "2", "status": "ok"},
        ]
        seen: list[str] = []

        response = dc.wait_for_matching_response(
            incoming,
            "2",
            on_unsolicited=lambda kind, payload: seen.append(kind),
        )

        self.assertEqual(response, {"id": "2", "status": "ok"})
        self.assertEqual(seen, ["event"])

    def test_event_with_same_id_is_not_treated_as_request_response(self) -> None:
        incoming = [
            {
                "id": "init-1",
                "status": "event",
                "event": {"target": "TRIG", "type": "falling"},
            },
            {"id": "init-1", "status": "ok"},
        ]
        seen: list[tuple[str, str]] = []

        response = dc.wait_for_matching_response(
            incoming,
            "init-1",
            on_unsolicited=lambda kind, payload: seen.append((kind, payload["status"])),
        )

        self.assertEqual(response["status"], "ok")
        self.assertEqual(seen, [("event", "event")])

    def test_interleaved_unsolicited_and_pin_value(self) -> None:
        incoming = [
            {
                "id": "init-1",
                "status": "event",
                "event": {"target": "TRIG", "type": "rising"},
            },
            {"id": "other", "status": "ok"},
            {"id": "get-1", "status": "pin_value", "value": 1},
        ]
        seen: list[str] = []

        response = dc.wait_for_matching_response(
            incoming,
            "get-1",
            on_unsolicited=lambda kind, _payload: seen.append(kind),
        )

        self.assertEqual(response["status"], "pin_value")
        self.assertEqual(response["value"], 1)
        self.assertEqual(seen, ["event", "unsolicited"])

    def test_raw_payload_without_id_takes_first_non_event(self) -> None:
        incoming = [
            {
                "id": "init-1",
                "status": "event",
                "event": {"target": "TRIG", "type": "rising"},
            },
            {"id": "whatever", "status": "ok"},
        ]

        response = dc.wait_for_matching_response(incoming, None)
        self.assertEqual(response["status"], "ok")

    def test_missing_matching_response_raises(self) -> None:
        incoming = [
            {
                "id": "init-1",
                "status": "event",
                "event": {"target": "TRIG", "type": "rising"},
            }
        ]
        with self.assertRaises(RuntimeError):
            dc.wait_for_matching_response(incoming, "2")

    def test_classify_event_even_when_ids_match(self) -> None:
        payload = {
            "id": "1",
            "status": "event",
            "event": {"target": "GPIO4_B5", "type": "rising"},
        }
        self.assertEqual(dc.classify_incoming(payload, "1"), "event")
        self.assertTrue(dc.is_event_message(payload))


class DebugClientSocketTests(unittest.TestCase):
    def setUp(self) -> None:
        self.client_sock, self.server_sock = socket.socketpair()
        self.client_sock.settimeout(0.5)
        self.client = dc.DebugClient("unused", timeout=0.5)
        self.client.sock = self.client_sock

    def tearDown(self) -> None:
        self.client.__exit__(None, None, None)
        self.server_sock.close()

    def test_read_still_works_after_drain_timeout(self) -> None:
        self.client.drain_pending(timeout=0.01)
        self.server_sock.sendall(b'{"id":"1","status":"ok"}\n')

        response = self.client.send({"id": "1", "action": "init", "target": {}})

        self.assertEqual(response, {"id": "1", "status": "ok"})

    def test_partial_line_is_retained_across_drain_timeout(self) -> None:
        self.server_sock.sendall(b'{"id":"1"')
        self.client.drain_pending(timeout=0.01)
        self.server_sock.sendall(b',"status":"ok"}\n')

        response = self.client.send({"id": "1", "action": "init", "target": {}})

        self.assertEqual(response, {"id": "1", "status": "ok"})


class PromptLineTests(unittest.TestCase):
    def setUp(self) -> None:
        self.client_sock, self.server_sock = socket.socketpair()
        self.stdin_r, self.stdin_w = os.pipe()
        self.stdin_file = os.fdopen(self.stdin_r)
        self.stdin_out = os.fdopen(self.stdin_w, "w")
        self.client = dc.DebugClient("unused", timeout=2.0)
        self.client.sock = self.client_sock

    def tearDown(self) -> None:
        self.client.__exit__(None, None, None)
        self.server_sock.close()
        self.stdin_file.close()
        self.stdin_out.close()

    def test_flushes_queued_event_before_reading_stdin(self) -> None:
        self.server_sock.sendall(
            b'{"id":"1","status":"event","event":{"target":"IRQ","type":"rising"}}\n'
        )
        self.stdin_out.write("init\n")
        self.stdin_out.flush()
        seen: list[tuple[str, dict]] = []

        with patch.object(dc, "print_unsolicited", lambda kind, payload: seen.append((kind, payload))):
            line = self.client.prompt_line("action: ", stdin=self.stdin_file)

        self.assertEqual(line, "init")
        self.assertEqual(seen[0][0], "event")
        self.assertEqual(seen[0][1]["event"]["target"], "IRQ")

    def test_prints_event_arriving_while_waiting_for_stdin(self) -> None:
        seen: list[tuple[str, dict]] = []
        result: dict[str, str] = {}

        def worker() -> None:
            with patch.object(
                dc,
                "print_unsolicited",
                lambda kind, payload: seen.append((kind, payload)),
            ):
                result["line"] = self.client.prompt_line("action: ", stdin=self.stdin_file)

        thread = threading.Thread(target=worker)
        thread.start()
        time.sleep(0.05)
        self.server_sock.sendall(
            b'{"id":"1","status":"event","event":{"target":"IRQ","type":"falling"}}\n'
        )
        time.sleep(0.05)
        self.stdin_out.write("get\n")
        self.stdin_out.flush()
        thread.join(timeout=2)
        self.assertFalse(thread.is_alive())
        self.assertEqual(result["line"], "get")
        self.assertEqual(seen[0][0], "event")
        self.assertEqual(seen[0][1]["event"]["type"], "falling")


class EndToEndPayloadTests(unittest.TestCase):
    def test_smoke_flow_payloads_use_live_protocol_shape(self) -> None:
        allocator = dc.RequestIdAllocator()
        init = dc.build_init_request(
            {
                "IN": dc.build_target_config("input", "gpiochip0:0"),
                "OUT": dc.build_target_config("output", "gpiochip0:7"),
                "LED": dc.build_target_config("output", "GPIO1_B5"),
            },
            dc.canned_request_id(None, allocator),
        )
        get_in = dc.build_get_request("IN", dc.canned_request_id(None, allocator))
        set_out = dc.build_set_request({"OUT": 1}, dc.canned_request_id(None, allocator))
        stepped = dc.build_set_request(
            [{"LED": 1}, {"lag": 150, "LED": 0}],
            dc.canned_request_id(None, allocator),
        )

        self.assertEqual([init["id"], get_in["id"], set_out["id"], stepped["id"]], ["1", "2", "3", "4"])
        self.assertEqual(init["action"], "init")
        self.assertEqual(get_in["action"], "get")
        self.assertEqual(set_out["action"], "set")
        self.assertEqual(stepped["target"][0], {"LED": 1})
        self.assertEqual(stepped["target"][1]["lag"], 150)

        correlated = dc.wait_for_matching_response(
            [
                {"id": "1", "status": "event", "event": {"target": "LED", "type": "rising"}},
                {"id": "4", "status": "ok"},
            ],
            "4",
        )
        self.assertEqual(correlated, {"id": "4", "status": "ok"})


class InteractiveBuilderTests(unittest.TestCase):
    def _feed(self, lines: list[str]):
        iterator = iter(lines)
        return lambda _prompt: next(iterator)

    def test_init_multi_target_with_optional_fields(self) -> None:
        request = dc.build_request_interactively(
            "1",
            self._feed(
                [
                    "init",
                    "IN",
                    "input",
                    "gpiochip0:7",
                    "pull_up",
                    "y",
                    "BUS",
                    "output",
                    "GPIO1_B5 GPIO1_B6",
                    "",
                    "y",
                    "IRQ",
                    "trigger",
                    "GPIO1_A2",
                    "rising",
                    "n",
                ]
            ),
        )

        self.assertEqual(request["id"], "1")
        self.assertEqual(request["action"], "init")
        self.assertEqual(
            request["target"]["IN"],
            {"mode": "input", "pin": "gpiochip0:7", "bias": "pull_up"},
        )
        self.assertEqual(
            request["target"]["BUS"],
            {"mode": "output", "pin": ["GPIO1_B5", "GPIO1_B6"]},
        )
        self.assertNotIn("drive", request["target"]["BUS"])
        self.assertEqual(
            request["target"]["IRQ"],
            {"mode": "trigger", "pin": "GPIO1_A2", "edge": "rising"},
        )

    def test_get_one_name_stays_a_string(self) -> None:
        request = dc.build_request_interactively(
            "2",
            self._feed(["get", "IN"]),
        )
        self.assertEqual(request, {"id": "2", "action": "get", "target": "IN"})

    def test_get_several_names_uses_array(self) -> None:
        request = dc.build_request_interactively(
            "3",
            self._feed(["get", "IN IRQ"]),
        )
        self.assertEqual(
            request,
            {"id": "3", "action": "get", "target": ["IN", "IRQ"]},
        )

    def test_set_immediate_map(self) -> None:
        request = dc.build_request_interactively(
            "4",
            self._feed(["set", "immediate", "LED", "1", "BUS", "5", ""]),
        )
        self.assertEqual(
            request,
            {"id": "4", "action": "set", "target": {"LED": 1, "BUS": 5}},
        )

    def test_set_stepped_omits_lag_on_first_step(self) -> None:
        request = dc.build_request_interactively(
            "5",
            self._feed(
                [
                    "set",
                    "stepped",
                    "LED",
                    "1",
                    "",
                    "y",
                    "150",
                    "LED",
                    "0",
                    "",
                    "n",
                ]
            ),
        )
        self.assertEqual(request["id"], "5")
        self.assertEqual(request["action"], "set")
        self.assertEqual(request["target"][0], {"LED": 1})
        self.assertNotIn("lag", request["target"][0])
        self.assertEqual(request["target"][1], {"lag": 150, "LED": 0})

    def test_allocator_id_is_written_onto_the_built_object(self) -> None:
        allocator = dc.RequestIdAllocator()
        request = dc.build_request_interactively(
            dc.canned_request_id(None, allocator),
            self._feed(["get", "IN"]),
        )
        self.assertEqual(request["id"], "1")
        self.assertEqual(allocator.next_id(), "2")

    def test_invalid_action_retries_then_builds(self) -> None:
        request = dc.build_request_interactively(
            "6",
            self._feed(["bogus", "get", "IN"]),
        )
        self.assertEqual(request["action"], "get")
        self.assertEqual(request["target"], "IN")

    def test_empty_required_field_retries(self) -> None:
        request = dc.build_request_interactively(
            "7",
            self._feed(["get", "", "IN"]),
        )
        self.assertEqual(request["target"], "IN")

    def test_trigger_with_multiple_pins_returns_to_action_prompt(self) -> None:
        request = dc.build_request_interactively(
            "8",
            self._feed(
                [
                    "init",
                    "IRQ",
                    "trigger",
                    "GPIO1_A2 GPIO1_A3",
                    "get",
                    "IN",
                ]
            ),
        )
        self.assertEqual(request, {"id": "8", "action": "get", "target": "IN"})

    def test_quit_raises_interactive_quit(self) -> None:
        with self.assertRaises(dc.InteractiveQuit):
            dc.build_request_interactively("9", self._feed(["quit"]))

    def test_raw_init_request_multiline_fills_missing_id(self) -> None:
        request = dc.build_request_interactively(
            "10",
            self._feed(
                [
                    "raw",
                    "{",
                    '  "action": "init",',
                    '  "target": {',
                    '    "LED": {',
                    '      "mode": "output",',
                    '      "pin": "GPIO1_B5"',
                    "    }",
                    "  }",
                    "}",
                    "",
                ]
            ),
        )
        self.assertEqual(request["id"], "10")
        self.assertEqual(request["action"], "init")
        self.assertEqual(
            request["target"]["LED"],
            {"mode": "output", "pin": "GPIO1_B5"},
        )

    def test_raw_get_request_keeps_id_and_action(self) -> None:
        request = dc.build_request_interactively(
            "11",
            self._feed(
                [
                    "raw",
                    '{"id":"user-9","action":"get","target":["IN","IRQ"]}',
                    "",
                ]
            ),
        )
        self.assertEqual(
            request,
            {"id": "user-9", "action": "get", "target": ["IN", "IRQ"]},
        )

    def test_raw_set_request_accepts_steps(self) -> None:
        request = dc.build_request_interactively(
            "12",
            self._feed(
                [
                    "raw",
                    '{"action":"set","target":[{"LED":1},{"lag":100,"LED":0}]}',
                    "",
                ]
            ),
        )
        self.assertEqual(request["id"], "12")
        self.assertEqual(request["action"], "set")
        self.assertEqual(request["target"][0], {"LED": 1})
        self.assertEqual(request["target"][1]["lag"], 100)

    def test_raw_invalid_json_returns_to_action_prompt(self) -> None:
        request = dc.build_request_interactively(
            "13",
            self._feed(
                [
                    "raw",
                    "{not json",
                    "",
                    "get",
                    "IN",
                ]
            ),
        )
        self.assertEqual(request, {"id": "13", "action": "get", "target": "IN"})

    def test_raw_requires_action_in_payload(self) -> None:
        request = dc.build_request_interactively(
            "14",
            self._feed(
                [
                    "raw",
                    '{"target":{"LED":{"mode":"output","pin":"GPIO1_B5"}}}',
                    "",
                    "get",
                    "IN",
                ]
            ),
        )
        self.assertEqual(request, {"id": "14", "action": "get", "target": "IN"})


class InteractiveLoopTests(unittest.TestCase):
    def test_parser_default_and_repl_use_wizard(self) -> None:
        parser = dc.build_parser()
        default = parser.parse_args([])
        self.assertIsNone(default.command)
        self.assertIs(default.handler, dc.run_repl)

        repl = parser.parse_args(["repl"])
        self.assertEqual(repl.command, "repl")
        self.assertIs(repl.handler, dc.run_repl)

    def test_oneshot_and_script_commands_remain(self) -> None:
        parser = dc.build_parser()
        self.assertEqual(parser.parse_args(["script", "session.jsonl"]).command, "script")
        self.assertIs(parser.parse_args(["script", "session.jsonl"]).handler, dc.run_script)
        self.assertEqual(parser.parse_args(["raw", "{}"]).command, "raw")
        self.assertEqual(
            parser.parse_args(
                ["init", "--name", "LED", "--mode", "output", "--pin", "GPIO1_B5"]
            ).command,
            "init",
        )
        self.assertEqual(parser.parse_args(["get", "--target", "LED"]).command, "get")
        self.assertEqual(
            parser.parse_args(["set", "--target", "LED", "--value", "1"]).command,
            "set",
        )

    def test_repl_connects_once_sends_wizard_request_then_quits(self) -> None:
        lines = iter(
            [
                "init",
                "LED",
                "output",
                "GPIO1_B5",
                "push_pull",
                "n",
                "quit",
            ]
        )
        client = MagicMock()
        client.send.return_value = {"id": "1", "status": "ok"}
        client.__enter__.return_value = client
        client.__exit__.return_value = None
        args = Namespace(
            socket="/tmp/gpiojsonsvc.sock",
            timeout=5.0,
            id_allocator=dc.RequestIdAllocator(),
        )

        with patch.object(dc, "DebugClient", return_value=client) as factory:
            status = dc.run_repl(args, readline=lambda _prompt: next(lines))

        self.assertEqual(status, 0)
        factory.assert_called_once_with("/tmp/gpiojsonsvc.sock", 5.0)
        client.send.assert_called_once()
        payload = client.send.call_args[0][0]
        self.assertEqual(payload["id"], "1")
        self.assertEqual(payload["action"], "init")
        self.assertEqual(payload["target"]["LED"]["pin"], "GPIO1_B5")
        client.drain_pending.assert_not_called()

    def test_repl_help_and_quit_do_not_send(self) -> None:
        lines = iter(["help", "quit"])
        client = MagicMock()
        client.__enter__.return_value = client
        client.__exit__.return_value = None
        args = Namespace(
            socket="/tmp/gpiojsonsvc.sock",
            timeout=5.0,
            id_allocator=dc.RequestIdAllocator(),
        )

        with patch.object(dc, "DebugClient", return_value=client):
            status = dc.run_repl(args, readline=lambda _prompt: next(lines))

        self.assertEqual(status, 0)
        client.send.assert_not_called()


class FramingTests(unittest.TestCase):
    def test_request_serializes_as_single_json_object(self) -> None:
        request = dc.build_get_request(["A", "B"], "4")
        encoded = json.dumps(request, separators=(",", ":"))
        decoded = json.loads(encoded)
        self.assertEqual(decoded["action"], "get")
        self.assertEqual(decoded["target"], ["A", "B"])


if __name__ == "__main__":
    unittest.main()
