#!/usr/bin/env python3
"""Unit tests for piper_server.resolve_speaker_id (stdlib unittest only).

Importing piper_server is side-effect-safe: the server only binds a socket
under `if __name__ == "__main__"`, and `_load_speaker_map()` returns `{}`
when the model JSON is absent. Tests pass explicit speaker maps rather than
relying on module globals.
"""
import unittest

from piper_server import SpeakerError, resolve_speaker_id

SPEAKER_ID_MAP = {"prudence": 0, "spike": 1}
SPEAKER_IDS = {0, 1}


class ResolveSpeakerIdTests(unittest.TestCase):
    def resolve(self, speaker):
        return resolve_speaker_id(speaker, SPEAKER_ID_MAP, SPEAKER_IDS)

    def test_name_resolves_to_id(self):
        self.assertEqual(self.resolve("spike"), 1)
        self.assertEqual(self.resolve("prudence"), 0)

    def test_numeric_id_string_resolves(self):
        # README states numeric IDs are accepted; this is the path the
        # original code left un-sanitised.
        self.assertEqual(self.resolve("0"), 0)
        self.assertEqual(self.resolve("1"), 1)

    def test_empty_speaker_returns_none(self):
        # Empty/whitespace must fall through to the model default, not error.
        self.assertIsNone(self.resolve(""))
        self.assertIsNone(self.resolve("   "))

    def test_unknown_name_raises(self):
        with self.assertRaises(SpeakerError):
            self.resolve("nobody")

    def test_out_of_range_numeric_id_raises(self):
        # A numeric string that parses but is not a real speaker.
        with self.assertRaises(SpeakerError):
            self.resolve("99")

    def test_injection_payloads_raise(self):
        # Regression guard for code-scanning alert #31: these must never
        # reach the command line.
        for payload in ("0; rm -rf /", "1 --model /evil.onnx", "prudence\n--output-raw"):
            with self.subTest(payload=payload):
                with self.assertRaises(SpeakerError):
                    self.resolve(payload)

    def test_resolved_value_is_int(self):
        # The int() conversion is what breaks the taint flow.
        self.assertIsInstance(self.resolve("spike"), int)
        self.assertIsInstance(self.resolve("0"), int)


if __name__ == "__main__":
    unittest.main()
