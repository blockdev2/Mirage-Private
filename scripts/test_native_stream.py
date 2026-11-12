import unittest

from check_native_stream import annexb_nals, av1_obu_types, summarize


class NativeStreamParserTests(unittest.TestCase):
    def test_annexb_split_and_h264_summary(self):
        data = bytes([0, 0, 0, 1, 0x67, 1, 0, 0, 1, 0x68, 2, 0, 0, 1, 0x65, 3, 0, 0, 1, 0x41, 4])
        self.assertEqual(len(annexb_nals(data)), 4)
        self.assertEqual(summarize("h264", data), (1, True, 4))

    def test_hevc_requires_vps_sps_pps(self):
        nal = lambda kind: bytes([0, 0, 1, kind << 1, 1, 0xAA])
        full = nal(32) + nal(33) + nal(34) + nal(19)
        self.assertEqual(summarize("hevc", full), (1, True, 4))
        self.assertFalse(summarize("hevc", nal(33) + nal(34) + nal(19))[1])

    def test_av1_rejects_unsized_and_truncated_obus(self):
        stream = bytes([0x12, 0x00, 0x0A, 0x01, 0x00, 0x32, 0x01, 0x10])
        self.assertEqual(av1_obu_types(stream), [(2, False), (1, False), (6, True)])
        self.assertEqual(summarize("av1", stream), (1, True, 3))
        for bad in (bytes([0x30, 0x10]), bytes([0x32, 0x05, 0x00]), bytes([0x92, 0x00])):
            with self.assertRaises(ValueError):
                av1_obu_types(bad)


if __name__ == "__main__":
    unittest.main()
