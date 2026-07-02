import json
import unittest
from unittest.mock import patch

from models import TelemetryEvent, TelemetryEventType, TelemetrySseMessage
from telemetry import (
    RelayPusher,
    SSEBroadcaster,
    Telemetry,
    emit_chat,
    emit_status,
    emit_tool_result,
)


class Sink:
    def __init__(self):
        self.events = []

    def broadcast(self, event):
        self.events.append(event)

    def push(self, event):
        self.events.append(event)


class TelemetryTests(unittest.TestCase):
    def test_emit_normalizes_without_mutating_source_event(self):
        sse = Sink()
        relay = Sink()
        telemetry = Telemetry(sse=sse, relay=relay)
        source = {"type": "status", "data": {"ok": True}, "agent": "doug"}

        telemetry.emit(source)

        self.assertNotIn("timestamp", source)
        self.assertEqual(len(sse.events), 1)
        self.assertEqual(len(relay.events), 1)
        self.assertIs(sse.events[0], relay.events[0])
        event = sse.events[0]
        self.assertIsInstance(event, TelemetryEvent)
        self.assertEqual(event.type, TelemetryEventType.STATUS)
        self.assertEqual(event.data, {"ok": True})
        self.assertEqual(event.agent, "doug")
        self.assertTrue(event.timestamp)

    def test_emit_helpers_use_typed_event_shapes(self):
        sink = Sink()
        telemetry = Telemetry(sse=sink)

        emit_chat(
            telemetry,
            "agent",
            "ready",
            agent="doug",
            tick=5,
            sections={"STATUS": {"label": "STATUS"}},
        )
        emit_tool_result(telemetry, "mine_at", "x" * 250, agent="doug")
        emit_status(telemetry, {"working": True}, agent="doug")

        chat = sink.events[0]
        tool_result = sink.events[1]
        status = sink.events[2]
        self.assertIsInstance(chat, TelemetryEvent)
        self.assertEqual(chat.type, TelemetryEventType.CHAT)
        self.assertEqual(chat.data["role"], "agent")
        self.assertEqual(chat.data["sections"], {"STATUS": {"label": "STATUS"}})
        self.assertEqual(chat.tick, 5)
        self.assertEqual(tool_result.data["output"], "x" * 200)
        self.assertEqual(status.data, {"working": True})

    def test_emit_chat_normalizes_weird_section_payloads(self):
        class Weird:
            def __repr__(self):
                return "<Weird>"

        sink = Sink()
        telemetry = Telemetry(sse=sink)

        emit_chat(telemetry, "agent", "ready", sections={"bad": Weird()})

        payload = json.loads(sink.events[0].to_json_text())
        self.assertEqual(payload["data"]["sections"], {"bad": "<Weird>"})

    def test_sse_broadcaster_serializes_normalized_event(self):
        broadcaster = SSEBroadcaster()
        client = broadcaster.add_client()
        self.addCleanup(lambda: broadcaster.remove_client(client))

        broadcaster.broadcast({"type": "status", "data": {"ok": True}})

        message = client.get_nowait()
        self.assertIsInstance(message, TelemetrySseMessage)
        self.assertTrue(message.frame.startswith("data: "))
        payload = json.loads(message.data)
        self.assertEqual(payload["type"], "status")
        self.assertEqual(payload["data"], {"ok": True})
        self.assertIn("timestamp", payload)

    def test_sse_message_owns_wire_frame_encoding(self):
        message = TelemetrySseMessage.coerce({
            "type": "status",
            "data": {"ok": True},
        })

        self.assertEqual(message.to_bytes(), message.frame.encode())
        self.assertTrue(message.frame.startswith("data: {"))
        self.assertTrue(message.frame.endswith("\n\n"))

    def test_relay_pusher_queues_models_until_http_boundary(self):
        with patch("threading.Thread") as thread_class:
            relay = RelayPusher("https://relay.example", "token")
            thread_class.return_value.start.assert_called_once()

        relay.push({"type": "status", "data": {"ok": True}, "agent": "doug"})

        event = relay._queue.get_nowait()
        self.assertIsInstance(event, TelemetryEvent)
        self.assertEqual(event.type, TelemetryEventType.STATUS)
        self.assertEqual(event.to_dict()["data"], {"ok": True})


if __name__ == "__main__":
    unittest.main()
