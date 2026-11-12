"""Offline framing/privacy tests; these never start a browser or network peer."""
import io
import json
from pathlib import Path
import subprocess
import sys
import unittest

from stream_webrtc_controller import MAX_LINE, read_command


class ControllerFrameTests(unittest.TestCase):
    def test_valid_command_and_clean_eof(self):
        self.assertEqual(read_command(io.BytesIO(b'{"op":"stats"}\n')), {"op": "stats"})
        self.assertIsNone(read_command(io.BytesIO()))

    def test_invalid_and_oversized_frames_are_rejected(self):
        for frame in [b'[]\n', b'{"op":7}\n', b'{"op":"stats"}', b'\xff\n',
                      b'x' * (MAX_LINE + 1) + b'\n']:
            with self.assertRaises((ValueError, UnicodeDecodeError)):
                read_command(io.BytesIO(frame))

    def test_controller_errors_do_not_echo_private_input(self):
        result = subprocess.run(
            [sys.executable, str(Path(__file__).with_name("stream_webrtc_controller.py"))],
            input=b'{"op":"private-fixture"}\n', capture_output=True, timeout=5)
        self.assertEqual(json.loads(result.stdout),
                         {"ok": False, "error": "controller_operation_failed"})
        self.assertNotIn(b'private-fixture', result.stdout + result.stderr)


if __name__ == "__main__":
    unittest.main()
