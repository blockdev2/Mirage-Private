"""Offline wire regressions for live channel-chat diagnostics."""
import json
import socket
import struct
import unittest

from check_channel_chat import Peer


class ConnectionOutcomeTests(unittest.TestCase):
    def peer_with_events(self, events):
        connection, server = socket.socketpair()
        self.addCleanup(server.close)
        self.addCleanup(connection.close)
        peer = Peer.__new__(Peer)
        peer.connection = connection
        peer.stream = connection.makefile("rb")
        self.addCleanup(peer.stream.close)
        peer.history = []
        for event in events:
            payload = json.dumps(event).encode()
            header = (bytes([0x81, len(payload)]) if len(payload) < 126
                      else bytes([0x81, 126]) + struct.pack("!H", len(payload)))
            server.sendall(header + payload)
        return peer

    def test_refusal_stops_before_a_later_state_and_withholds_raw_error(self):
        peer = self.peer_with_events([
            {"ev": "status", "connected": False,
             "error": "handshake failed: connection failed, you are banned; private-fixture"},
            {"ev": "state", "snapshot": {}},
        ])
        with self.assertRaises(RuntimeError) as failure:
            peer.until(lambda event: event.get("ev") == "state")
        self.assertIn("handshake_banned", str(failure.exception))
        self.assertNotIn("private-fixture", str(failure.exception))
        self.assertEqual(len(peer.history), 1)

    def test_unknown_failure_remains_sanitized(self):
        peer = self.peer_with_events([
            {"ev": "status", "connected": False, "error": "private-fixture"},
            {"ev": "state", "snapshot": {}},
        ])
        with self.assertRaises(RuntimeError) as failure:
            peer.until(lambda event: event.get("ev") == "state")
        self.assertIn("connection_failed", str(failure.exception))
        self.assertNotIn("private-fixture", str(failure.exception))

    def test_initial_disconnected_status_does_not_abort_connection(self):
        peer = self.peer_with_events([
            {"ev": "status", "connected": False, "error": None},
            {"ev": "state", "snapshot": {}},
        ])
        result = peer.until(lambda event: event.get("ev") == "state")
        self.assertEqual(result["ev"], "state")


if __name__ == "__main__":
    unittest.main()
