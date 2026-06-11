#!/usr/bin/env python3
"""Minimal HTTP server wrapping Piper TTS. Zero pip dependencies.

Supports multi-speaker models (e.g. Semaine: prudence, spike, obadiah, poppy).
Speaker can be set via:
  - PIPER_SPEAKER env var (default for all requests)
  - "speaker" field in the JSON request body (per-request override)
"""
import http.server
import json
import os
import subprocess

MODEL = os.environ.get("PIPER_MODEL", "/models/en_GB-semaine-medium.onnx")
PORT = int(os.environ.get("PIPER_PORT", "5000"))
SAMPLE_RATE = os.environ.get("PIPER_SAMPLE_RATE", "22050")
DEFAULT_SPEAKER = os.environ.get("PIPER_SPEAKER", "")
DEFAULT_SPEED = float(os.environ.get("PIPER_SPEED", "1.0"))
MIN_SPEED = 0.25
MAX_SPEED = 4.0
MAX_BODY_BYTES = 10 * 1024  # 10KB -- generous for any TTS input
SUBPROCESS_TIMEOUT = 30     # seconds

# Speaker name -> numeric ID mapping. Piper CLI only accepts numeric IDs.
# Loaded from the model's .onnx.json config if available, otherwise falls back
# to the Semaine defaults.
def _load_speaker_map():
    """Read speaker_id_map from the model's JSON config."""
    json_path = MODEL + ".json"
    try:
        with open(json_path) as f:
            cfg = json.load(f)
        return cfg.get("speaker_id_map", {})
    except (FileNotFoundError, json.JSONDecodeError):
        return {}

SPEAKER_ID_MAP = _load_speaker_map()  # e.g. {"prudence": 0, "spike": 1, ...}
VALID_SPEAKERS = set(SPEAKER_ID_MAP.keys()) | {str(v) for v in SPEAKER_ID_MAP.values()}
SPEAKER_IDS = {int(v) for v in SPEAKER_ID_MAP.values()}  # numeric IDs only


class SpeakerError(ValueError):
    """Raised when a requested speaker cannot be resolved to a valid ID."""


def resolve_speaker_id(speaker, speaker_id_map, speaker_ids):
    """Resolve a speaker name or numeric-ID string to an int speaker ID.

    Returns None for an empty speaker (use the model default). Raises
    SpeakerError for any value that is not a known speaker. The int()
    conversion guarantees the result cannot carry argv/shell injection.
    """
    speaker = str(speaker).strip()
    if not speaker:
        return None
    if speaker in speaker_id_map:
        speaker_id = int(speaker_id_map[speaker])
    else:
        try:
            speaker_id = int(speaker)
        except ValueError:
            raise SpeakerError(speaker)
    if speaker_id not in speaker_ids:
        raise SpeakerError(speaker)
    return speaker_id


class Handler(http.server.BaseHTTPRequestHandler):
    def do_POST(self):
        if self.path != "/synthesize":
            self.send_error(404)
            return

        # Validate Content-Type
        content_type = self.headers.get("Content-Type", "")
        if "application/json" not in content_type:
            self.send_error(415, "expected Content-Type: application/json")
            return

        # Enforce body size limit
        length = int(self.headers.get("Content-Length", 0))
        if length > MAX_BODY_BYTES:
            self.send_error(413, f"body exceeds {MAX_BODY_BYTES} byte limit")
            return

        body = json.loads(self.rfile.read(length))
        text = body.get("text", "").strip()
        if not text:
            self.send_error(400, "empty text")
            return

        # Speaker: per-request override > env default > omit (model default).
        # Resolve to an integer ID; piper CLI only accepts numeric IDs.
        try:
            speaker_id = resolve_speaker_id(
                body.get("speaker", DEFAULT_SPEAKER), SPEAKER_ID_MAP, SPEAKER_IDS
            )
        except SpeakerError as e:
            self.send_error(
                400,
                f"unknown speaker '{e}', valid: "
                f"{', '.join(sorted(SPEAKER_ID_MAP.keys()))}",
            )
            return

        # Speed: per-request override > env default > 1.0
        # speed > 1.0 = faster speech, < 1.0 = slower.
        # Piper uses --length-scale which is the inverse (lower = faster).
        speed = body.get("speed", DEFAULT_SPEED)
        try:
            speed = float(speed)
        except (TypeError, ValueError):
            self.send_error(400, f"invalid speed: {speed}")
            return
        if not (MIN_SPEED <= speed <= MAX_SPEED):
            self.send_error(400, f"speed must be between {MIN_SPEED} and {MAX_SPEED}, got {speed}")
            return
        length_scale = 1.0 / speed

        piper_cmd = ["/usr/local/piper/piper", "--model", MODEL, "--output-raw",
                     "--length-scale", f"{length_scale:.3f}"]
        if speaker_id is not None:
            piper_cmd += ["--speaker", str(speaker_id)]

        piper = None
        ffmpeg = None
        try:
            # Piper --output-raw emits s16le PCM at the model's sample rate.
            # Pipe directly into ffmpeg to produce OGG/Opus.
            piper = subprocess.Popen(
                piper_cmd,
                stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
            )
            ffmpeg = subprocess.Popen(
                ["ffmpeg", "-f", "s16le", "-ar", SAMPLE_RATE, "-ac", "1", "-i", "pipe:",
                 "-c:a", "libopus", "-b:a", "64k", "-f", "ogg", "pipe:1",
                 "-loglevel", "error"],
                stdin=piper.stdout, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
            )
            piper.stdout.close()
            piper.stdin.write(text.encode())
            piper.stdin.close()
            ogg_bytes, ffmpeg_err = ffmpeg.communicate(timeout=SUBPROCESS_TIMEOUT)
            piper.wait(timeout=5)
        except subprocess.TimeoutExpired:
            # Kill both processes on timeout
            for proc in (piper, ffmpeg):
                if proc and proc.poll() is None:
                    proc.kill()
            self.send_error(500, "synthesis timed out")
            return

        if piper.returncode != 0 or ffmpeg.returncode != 0:
            self.send_error(500, f"piper rc={piper.returncode} ffmpeg rc={ffmpeg.returncode}")
            return

        self.send_response(200)
        self.send_header("Content-Type", "audio/ogg")
        self.send_header("Content-Length", str(len(ogg_bytes)))
        self.end_headers()
        self.wfile.write(ogg_bytes)

    def do_GET(self):
        if self.path == "/health":
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.end_headers()
            self.wfile.write(json.dumps({
                "status": "ok",
                "model": MODEL,
                "default_speaker": DEFAULT_SPEAKER or None,
                "speakers": sorted(VALID_SPEAKERS - {"0", "1", "2", "3"}),
            }).encode())
        else:
            self.send_error(404)


if __name__ == "__main__":
    speaker_info = f", default_speaker={DEFAULT_SPEAKER}" if DEFAULT_SPEAKER else ""
    print(f"Piper server listening on :{PORT}, model={MODEL}{speaker_info}")
    http.server.ThreadingHTTPServer(("0.0.0.0", PORT), Handler).serve_forever()
