import copy
import unittest

from check_native_rtp import placement_failures, validate, validate_loss

RECEIVER = {
    "units_received": 60, "lost_packets": 0, "dropped_units": 0,
    "withheld_awaiting_keyframe": 0, "malformed_packets": 0, "foreign_ssrc": 0,
    "first_unit_keyframe": True, "bitstream_dims": [640, 360], "packets": 134,
    "keyframes": 3, "interarrival_ms": {"p50": 50.0, "p95": 56.0, "max": 60.0},
    "os_decode": {"status": "decoded", "frames": 60, "fixture_matches": True,
                  "distinct_motion_bands": 60},
}
SENDER = {"units_sent": 60, "packets_sent": 134, "gop_accepted_by_encoder": True}


class LiveRtpValidationTests(unittest.TestCase):
    def test_clean_stream_passes(self):
        self.assertEqual(validate(RECEIVER, SENDER, 60), [])

    def test_detects_loss_static_video_and_missing_keyframes(self):
        rx = copy.deepcopy(RECEIVER)
        rx["lost_packets"] = 2
        rx["os_decode"]["distinct_motion_bands"] = 1
        rx["keyframes"] = 1
        failures = validate(rx, SENDER, 60)
        self.assertIn("no packet loss", failures)
        self.assertIn("motion visible", failures)
        self.assertIn("periodic keyframes", failures)

    def test_burst_delivery_fails_pacing(self):
        rx = copy.deepcopy(RECEIVER)
        rx["interarrival_ms"]["p50"] = 0.2
        self.assertIn("real-time pacing", validate(rx, SENDER, 60))

    def test_loss_recovery_accounting(self):
        rx = copy.deepcopy(RECEIVER)
        rx.update(units_received=40, lost_packets=1, dropped_units=1,
                  withheld_awaiting_keyframe=19, keyframes=2)
        rx["os_decode"]["frames"] = 40
        self.assertEqual(validate_loss(rx, 60), [])
        rx["withheld_awaiting_keyframe"] = 0
        self.assertIn("frames withheld until keyframe", validate_loss(rx, 60))


    def test_placement_must_match_request(self):
        tx = {"target": {"adapter_kind": "integrated", "delivery": "cross_adapter_copy",
                         "adapter_is_capture_gpu": False}}
        self.assertEqual(placement_failures(tx, "hardware", "minimum-power"), [])
        self.assertTrue(placement_failures(tx, "hardware", "high-performance"))
        cpu = {"target": {"adapter_kind": "cpu", "delivery": "system_memory",
                          "adapter_is_capture_gpu": False}}
        self.assertEqual(placement_failures(cpu, "software", "auto"), [])
        self.assertTrue(placement_failures(tx, "software", "auto"))
        self.assertEqual(placement_failures(tx, "auto", "auto"), [])


if __name__ == "__main__":
    unittest.main()
