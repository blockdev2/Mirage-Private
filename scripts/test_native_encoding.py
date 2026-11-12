"""Regression checks for independent native-video verification."""

import unittest

from check_native_encoding import validate_stream, validate_pixel


class EncodingValidationTests(unittest.TestCase):
    def test_rejects_wrong_codec_dimensions_count_and_timing(self):
        stream = dict(codec_name="h264", width=640, height=360,
                      nb_read_frames="20", avg_frame_rate="20/1")
        self.assertTrue(validate_stream(stream))
        for key, value in [("codec_name", "hevc"), ("width", 602),
                           ("height", 377), ("nb_read_frames", "0"),
                           ("nb_read_frames", "N/A"), ("avg_frame_rate", "30/1")]:
            with self.subTest(key=key):
                self.assertFalse(validate_stream({**stream, key: value}))

    def test_decoded_fixture_is_not_black_or_channel_swapped(self):
        self.assertTrue(validate_pixel(bytes([80, 180, 20, 255])))
        self.assertTrue(validate_pixel(bytes([83, 177, 24, 255])))
        self.assertFalse(validate_pixel(bytes([0, 0, 0, 255])))
        self.assertFalse(validate_pixel(bytes([20, 180, 80, 255])))
        # Observed before propagating BT.709 metadata into Media Foundation.
        self.assertFalse(validate_pixel(bytes([81, 201, 32, 255])))
        self.assertFalse(validate_pixel(b""))


if __name__ == "__main__":
    unittest.main()
