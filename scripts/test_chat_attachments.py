import unittest

from check_chat_attachments import KINDS, REFUSALS, gif_two_frames, png_1x1, verdict

PASSED = {"emoji_received": True, **{key: True for key in REFUSALS},
          **{kind: {"send_error_code": None, "bytes_identical": True} for kind in KINDS}}


class FixtureTests(unittest.TestCase):
    def test_png_is_a_valid_signature_and_ends_with_iend(self):
        data = png_1x1((1, 2, 3))
        self.assertTrue(data.startswith(b"\x89PNG\r\n\x1a\n"))
        self.assertTrue(data.endswith(b"IEND\xaeB`\x82"))

    def test_gif_is_animated_and_loops(self):
        data = gif_two_frames()
        self.assertTrue(data.startswith(b"GIF89a"))
        self.assertIn(b"NETSCAPE2.0", data)
        self.assertEqual(data.count(b"\x21\xF9\x04"), 2)  # two frames with delays
        self.assertTrue(data.endswith(b"\x3B"))


class VerdictTests(unittest.TestCase):
    def test_full_pass(self):
        self.assertEqual(verdict(PASSED), 0)

    def test_corrupted_download_fails(self):
        results = {**PASSED, "gif": {"send_error_code": None, "bytes_identical": False}}
        self.assertEqual(verdict(results), 1)

    def test_permission_denied_server_is_unverified_not_passed(self):
        results = {**PASSED, **{kind: {"send_error_code": "upload_permission"} for kind in KINDS}}
        self.assertEqual(verdict(results), 2)

    def test_missing_refusal_fails_even_when_uploads_are_denied(self):
        results = {**PASSED, "refused_oversized": False,
                   **{kind: {"send_error_code": "upload_permission"} for kind in KINDS}}
        self.assertEqual(verdict(results), 1)

    def test_other_upload_errors_fail(self):
        results = {**PASSED, **{kind: {"send_error_code": "upload_refused"} for kind in KINDS}}
        self.assertEqual(verdict(results), 1)


if __name__ == "__main__":
    unittest.main()
