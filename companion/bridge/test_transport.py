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


class FakeRcon:
    def __init__(self, responses=None):
        self.commands = []
        self.responses = list(responses or [])

    def execute(self, command):
        self.commands.append(command)
        if self.responses:
            return self.responses.pop(0)
        return ""


class TransportTests(unittest.TestCase):
    def test_send_response_uses_validated_side_effect_remote_call(self):
        rcon = FakeRcon()

        transport.send_response(
            rcon,
            3,
            'doug") game.print("oops',
            'hello ]=] world',
        )

        self.assertEqual(len(rcon.commands), 1)
        command = rcon.commands[0]
        self.assertEqual(
            command,
            '/silent-command remote.call("claude_interface", '
            '"receive_response", 3, [[doug") game.print("oops]], '
            '[[hello ]=] world]])',
        )
        self.assertNotIn("rcon.print", command)

    def test_status_helpers_use_validated_side_effect_remote_calls(self):
        rcon = FakeRcon()

        transport.send_tool_status(rcon, 2, "doug", "walk_to")
        transport.set_status(rcon, 2, "running")
        transport.register_agent(rcon, "doug", label="Nauvis")
        transport.unregister_agent(rcon, "doug")
        transport.set_spectator_mode(rcon, enabled=False)

        self.assertEqual(len(rcon.commands), 5)
        for command in rcon.commands:
            self.assertIn('remote.call("claude_interface"', command)
            self.assertNotIn("rcon.print", command)
        self.assertIn('"tool_status"', rcon.commands[0])
        self.assertIn('"set_status"', rcon.commands[1])
        self.assertIn('"register_agent"', rcon.commands[2])
        self.assertIn('"unregister_agent"', rcon.commands[3])
        self.assertIn('"set_spectator_mode", false)', rcon.commands[4])

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

    def test_input_watcher_legacy_poll_returns_dicts(self):
        with tempfile.TemporaryDirectory() as tmp:
            input_file = Path(tmp) / "input.jsonl"
            watcher = transport.InputWatcher(input_file)
            input_file.write_text(json.dumps({"message": "hi"}) + "\n")

            messages = watcher.poll()

        self.assertEqual(messages, [{
            "message": "hi",
            "player_index": 1,
            "player_name": "Player",
            "target_agent": "default",
        }])

    def test_setup_surfaces_uses_mod_remote(self):
        rcon = FakeRcon([
            '{"planet":"vulcanus","status":"created"}\n',
            '{"planet":"nauvis","status":"exists"}\n',
        ])

        result = transport.setup_surfaces(rcon, ["vulcanus", "nauvis"])

        self.assertEqual(result, {"vulcanus": "created", "nauvis": "exists"})
        self.assertEqual(len(rcon.commands), 2)
        self.assertIn('remote.call("claude_interface", "ensure_surface_result"', rcon.commands[0])
        for command in rcon.commands:
            self.assertNotIn("game.planets", command)
            self.assertNotIn("game.surfaces", command)
            self.assertNotIn("create_surface", command)

    def test_setup_surfaces_model_returns_typed_results(self):
        rcon = FakeRcon([
            '{"planet":"vulcanus","status":"created"}\n',
            '{"planet":"nauvis","status":"exists"}\n',
        ])

        result = transport.setup_surfaces_model(rcon, ["vulcanus", "nauvis"])

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

    def test_pre_place_character_uses_mod_remote(self):
        rcon = FakeRcon(['{"agent_name":"doug-nauvis","planet":"nauvis","status":"created"}\n'])

        result = transport.pre_place_character(rcon, "doug-nauvis", "nauvis", spawn_offset=2)

        self.assertEqual(result, "created")
        self.assertEqual(len(rcon.commands), 1)
        command = rcon.commands[0]
        self.assertIn('remote.call("claude_interface", "pre_place_character_result"', command)
        self.assertIn("doug-nauvis", command)
        self.assertIn("nauvis", command)
        self.assertIn(", 15))", command)
        for forbidden in [
            "request_to_generate_chunks",
            "force_generate_chunk_requests",
            "create_entity",
            "storage.factorioctl_characters",
            "storage.factorioctl_entities",
        ]:
            self.assertNotIn(forbidden, command)

    def test_pre_place_character_model_returns_typed_result(self):
        rcon = FakeRcon([
            '{"agent_name":"doug-nauvis","planet":"nauvis","status":"teleported"}\n',
        ])

        result = transport.pre_place_character_model(
            rcon,
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

    def test_check_mod_loaded_uses_typed_json_probe(self):
        rcon = FakeRcon(['{"loaded":true}\n'])

        self.assertTrue(transport.check_mod_loaded(rcon))

        self.assertEqual(len(rcon.commands), 1)
        command = rcon.commands[0]
        self.assertIn('"loaded"', command)
        self.assertIn('remote.interfaces["claude_interface"] ~= nil', command)
        self.assertNotIn('"yes"', command)
        self.assertNotIn('"no"', command)

    def test_check_mod_loaded_false_and_malformed_are_false(self):
        self.assertFalse(transport.check_mod_loaded(FakeRcon(['{"loaded":false}\n'])))
        self.assertFalse(transport.check_mod_loaded(FakeRcon(["not json\n"])))

    def test_mod_interface_status_validates_rcon_json_payload(self):
        status = ModInterfaceStatus.from_rcon_response("noise\n{\"loaded\":true}\n")

        self.assertTrue(status.loaded)


if __name__ == "__main__":
    unittest.main()
