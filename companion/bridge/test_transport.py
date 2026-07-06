import json
import tempfile
import unittest
from pathlib import Path

from models import (
    BridgeInputFileDelta,
    BridgeInputMessage,
    BridgeValidationError,
    CharacterPlacementResult,
    ModInterfaceStatus,
    SurfaceSetupResult,
    SurfaceSetupResults,
)
import transport


class FakeLifecycle:
    def __init__(self, responses=None):
        self.calls = []
        self.responses = list(responses or [])

    def _record(self, tool, **arguments):
        self.calls.append((tool, arguments))
        if self.responses:
            return self.responses.pop(0)
        return ""

    def send_chat_response(self, player_index, agent_name, text):
        return self._record(
            "send_chat_response",
            player_index=player_index,
            agent_name=agent_name,
            text=text,
        )

    def tool_status(self, player_index, agent_name, tool_name):
        return self._record(
            "tool_status",
            player_index=player_index,
            agent_name=agent_name,
            tool_name=tool_name,
        )

    def set_status(self, player_index, status):
        return self._record("set_status", player_index=player_index, status=status)

    def register_agent(self, agent_name, label=None):
        arguments = {"agent_name": agent_name}
        if label is not None:
            arguments["label"] = label
        return self._record("register_agent", **arguments)

    def unregister_agent(self, agent_name):
        return self._record("unregister_agent", agent_name=agent_name)

    def ensure_surface(self, planet):
        return self._record("ensure_surface", planet=planet)

    def place_character(self, agent_name, planet, spawn_x):
        return self._record(
            "place_character",
            agent_name=agent_name,
            planet=planet,
            spawn_x=spawn_x,
        )

    def set_spectator_mode(self, enabled=True):
        return self._record("set_spectator_mode", enabled=enabled)

    def ping(self):
        return self._record("ping")


class TransportTests(unittest.TestCase):
    def test_send_response_uses_lifecycle_mcp_tool(self):
        lifecycle = FakeLifecycle()

        transport.send_response(
            lifecycle,
            3,
            'doug") game.print("oops',
            'hello ]=] world',
        )

        self.assertEqual(lifecycle.calls, [(
            "send_chat_response",
            {
                "player_index": 3,
                "agent_name": 'doug") game.print("oops',
                "text": "hello ]=] world",
            },
        )])

    def test_status_helpers_use_lifecycle_mcp_tools(self):
        lifecycle = FakeLifecycle()

        transport.send_tool_status(lifecycle, 2, "doug", "walk_to")
        transport.set_status(lifecycle, 2, "running")
        transport.register_agent(lifecycle, "doug", label="Nauvis")
        transport.unregister_agent(lifecycle, "doug")
        transport.set_spectator_mode(lifecycle, enabled=False)

        self.assertEqual(lifecycle.calls, [
            ("tool_status", {
                "player_index": 2,
                "agent_name": "doug",
                "tool_name": "walk_to",
            }),
            ("set_status", {"player_index": 2, "status": "running"}),
            ("register_agent", {"agent_name": "doug", "label": "Nauvis"}),
            ("unregister_agent", {"agent_name": "doug"}),
            ("set_spectator_mode", {"enabled": False}),
        ])

    def test_input_watcher_coerces_and_filters_inbound_messages(self):
        with tempfile.TemporaryDirectory() as tmp:
            input_file = Path(tmp) / "input.jsonl"
            watcher = transport.InputWatcher(input_file)
            input_file.write_text("\n".join([
                "{not-json",
                json.dumps(["not", "a", "mapping"]),
                json.dumps({"message": ""}),
                json.dumps({
                    "message": 42,
                    "player_index": "3",
                    "player_name": "",
                    "target_agent": "",
                    "read_only_tools": "true",
                    "future_field": "kept",
                }),
            ]) + "\n")

            messages = watcher.poll_model()

        self.assertEqual(len(messages), 1)
        self.assertIsInstance(messages[0], BridgeInputMessage)
        self.assertEqual(messages[0].to_dict(), {
            "future_field": "kept",
            "message": "42",
            "player_index": 3,
            "player_name": "Player",
            "target_agent": "default",
            "read_only_tools": True,
        })

    def test_input_watcher_returns_typed_delta_and_advances_cursor(self):
        with tempfile.TemporaryDirectory() as tmp:
            input_file = Path(tmp) / "input.jsonl"
            input_file.write_text(json.dumps({"message": "old"}) + "\n")
            watcher = transport.InputWatcher(input_file)
            input_file.write_text(
                input_file.read_text()
                + json.dumps({"message": "new", "target_agent": "doug"}) + "\n"
            )

            delta = watcher.poll_delta_model()
            second = watcher.poll_delta_model()

        self.assertIsInstance(delta, BridgeInputFileDelta)
        self.assertTrue(delta.advanced)
        self.assertEqual(len(delta.messages), 1)
        self.assertEqual(delta.messages[0].message, "new")
        self.assertEqual(delta.messages[0].target_agent, "doug")
        self.assertEqual(second.messages, [])

    def test_input_watcher_poll_model_returns_typed_messages(self):
        with tempfile.TemporaryDirectory() as tmp:
            input_file = Path(tmp) / "input.jsonl"
            watcher = transport.InputWatcher(input_file)
            input_file.write_text(json.dumps({"message": "hi"}) + "\n")

            messages = watcher.poll_model()

        self.assertEqual([message.to_dict() for message in messages], [{
            "message": "hi",
            "player_index": 1,
            "player_name": "Player",
            "target_agent": "default",
        }])

    def test_setup_surfaces_uses_lifecycle_mcp_tool(self):
        lifecycle = FakeLifecycle([
            '{"planet":"vulcanus","status":"created"}\n',
            '{"planet":"nauvis","status":"exists"}\n',
        ])

        result = transport.setup_surfaces_model(lifecycle, ["vulcanus", "nauvis"])

        self.assertEqual(result.to_dict(), {"vulcanus": "created", "nauvis": "exists"})
        self.assertEqual(lifecycle.calls, [
            ("ensure_surface", {"planet": "vulcanus"}),
            ("ensure_surface", {"planet": "nauvis"}),
        ])

    def test_setup_surfaces_model_returns_typed_results(self):
        lifecycle = FakeLifecycle([
            '{"planet":"vulcanus","status":"created"}\n',
            '{"planet":"nauvis","status":"exists"}\n',
        ])

        result = transport.setup_surfaces_model(lifecycle, ["vulcanus", "nauvis"])

        self.assertIsInstance(result, SurfaceSetupResults)
        self.assertEqual(result.items(), (
            ("vulcanus", "created"),
            ("nauvis", "exists"),
        ))
        self.assertEqual(result.to_dict(), {
            "vulcanus": "created",
            "nauvis": "exists",
        })
        self.assertIs(SurfaceSetupResults.from_mapping(result), result)

    def test_surface_setup_result_validates_rcon_json_payload(self):
        result = SurfaceSetupResult.from_rcon_response(
            'noise\n{"planet":"gleba","status":"created"}\n',
            planet="gleba",
        )

        self.assertEqual(result.planet, "gleba")
        self.assertEqual(result.status, "created")
        self.assertIs(
            SurfaceSetupResult.from_rcon_response(result, planet="gleba"),
            result,
        )

    def test_surface_setup_result_uses_requested_planet_as_authority(self):
        inferred = SurfaceSetupResult.from_rcon_response(
            '{"status":"created"}\n',
            planet="vulcanus",
        )

        self.assertEqual(inferred.planet, "vulcanus")
        self.assertEqual(inferred.status, "created")
        with self.assertRaisesRegex(
            BridgeValidationError,
            "surface_setup_result.planet: expected 'nauvis', got 'gleba'",
        ):
            SurfaceSetupResult.from_rcon_response(
                '{"planet":"gleba","status":"created"}\n',
                planet="nauvis",
            )

    def test_transport_payload_models_preserve_typed_instances(self):
        status = ModInterfaceStatus(loaded=True)
        placement = CharacterPlacementResult(
            agent_name="doug",
            planet="nauvis",
            status="created",
        )

        self.assertIs(ModInterfaceStatus.from_rcon_response(status), status)
        self.assertIs(
            CharacterPlacementResult.from_rcon_response(
                placement,
                agent_name="doug",
                planet="nauvis",
            ),
            placement,
        )

    def test_character_placement_result_uses_requested_identity_as_authority(self):
        inferred = CharacterPlacementResult.from_rcon_response(
            '{"status":"created"}\n',
            agent_name="doug-nauvis",
            planet="nauvis",
        )

        self.assertEqual(inferred.agent_name, "doug-nauvis")
        self.assertEqual(inferred.planet, "nauvis")
        self.assertEqual(inferred.status, "created")
        with self.assertRaisesRegex(
            BridgeValidationError,
            "character_placement_result.agent_name: expected 'doug', got 'ada'",
        ):
            CharacterPlacementResult.from_rcon_response(
                '{"agent_name":"ada","planet":"nauvis","status":"created"}\n',
                agent_name="doug",
                planet="nauvis",
            )
        with self.assertRaisesRegex(
            BridgeValidationError,
            "character_placement_result.planet: expected 'nauvis', got 'gleba'",
        ):
            CharacterPlacementResult.from_rcon_response(
                CharacterPlacementResult(
                    agent_name="doug",
                    planet="gleba",
                    status="created",
                ),
                agent_name="doug",
                planet="nauvis",
            )

    def test_pre_place_character_uses_lifecycle_mcp_tool(self):
        lifecycle = FakeLifecycle([
            '{"agent_name":"doug-nauvis","planet":"nauvis","status":"created"}\n'
        ])

        result = transport.pre_place_character_model(
            lifecycle,
            "doug-nauvis",
            "nauvis",
            spawn_offset=2,
        )

        self.assertEqual(result.status, "created")
        self.assertEqual(lifecycle.calls, [(
            "place_character",
            {"agent_name": "doug-nauvis", "planet": "nauvis", "spawn_x": 15},
        )])

    def test_pre_place_character_model_returns_typed_result(self):
        lifecycle = FakeLifecycle([
            '{"agent_name":"doug-nauvis","planet":"nauvis","status":"teleported"}\n',
        ])

        result = transport.pre_place_character_model(
            lifecycle,
            "doug-nauvis",
            "nauvis",
            spawn_offset=1,
        )

        self.assertIsInstance(result, CharacterPlacementResult)
        self.assertEqual(result.agent_name, "doug-nauvis")
        self.assertEqual(result.planet, "nauvis")
        self.assertEqual(result.status, "teleported")

    def test_character_placement_result_validates_rcon_json_payload(self):
        result = CharacterPlacementResult.from_rcon_response(
            'noise\n{"agent_name":"doug","planet":"fulgora","status":"created"}\n',
            agent_name="doug",
            planet="fulgora",
        )

        self.assertEqual(result.agent_name, "doug")
        self.assertEqual(result.planet, "fulgora")
        self.assertEqual(result.status, "created")

    def test_check_mod_loaded_uses_lifecycle_mcp_tool(self):
        lifecycle = FakeLifecycle(["pong\n"])

        self.assertTrue(transport.check_mod_loaded(lifecycle))

        self.assertEqual(lifecycle.calls, [("ping", {})])

    def test_check_mod_loaded_false_and_malformed_are_false(self):
        self.assertFalse(transport.check_mod_loaded(FakeLifecycle(["not pong\n"])))
        self.assertFalse(transport.check_mod_loaded(FakeLifecycle(["not json\n"])))

    def test_mod_interface_status_validates_rcon_json_payload(self):
        status = ModInterfaceStatus.from_rcon_response("noise\n{\"loaded\":true}\n")

        self.assertTrue(status.loaded)


if __name__ == "__main__":
    unittest.main()
