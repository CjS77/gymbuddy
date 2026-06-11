# Piper TTS Server

HTTP server wrapping [Piper](https://github.com/rhasspy/piper) text-to-speech. Ships the
**Semaine** multi-speaker British English model with four distinct voices.

## Speakers

| Name       | ID | Personality               |
|------------|----|---------------------------|
| `prudence` | 0  | Calm, measured            |
| `spike`    | 1  | Energetic, direct         |
| `obadiah`  | 2  | Slow, thoughtful          |
| `poppy`    | 3  | Bright, cheerful          |

Speaker names are mapped to numeric IDs automatically from the model's config file.

## Configuration

Environment variables (all optional):

| Variable           | Default                               | Description                          |
|--------------------|---------------------------------------|--------------------------------------|
| `PIPER_MODEL`      | `/models/en_GB-semaine-medium.onnx`   | Path to the ONNX voice model         |
| `PIPER_PORT`       | `5000`                                | HTTP listen port                     |
| `PIPER_SAMPLE_RATE`| `22050`                               | Audio sample rate (must match model) |
| `PIPER_SPEAKER`    | *(empty)*                             | Default speaker for all requests     |
| `PIPER_SPEED`      | `1.0`                                 | Default speaking speed multiplier    |

Set `PIPER_SPEAKER` to lock the server to a single voice:

```sh
docker run -e PIPER_SPEAKER=spike -p 5000:5000 corre-piper
```

## API

### `POST /synthesize`

Synthesize text to OGG/Opus audio.

**Request body** (JSON):

```json
{
  "text": "Hello, how are you?",
  "speaker": "prudence",
  "speed": 1.2
}
```

| Field     | Type     | Default          | Description                                          |
|-----------|----------|------------------|------------------------------------------------------|
| `text`    | string   | *(required)*     | Text to synthesize                                   |
| `speaker` | string   | `PIPER_SPEAKER`  | Speaker name or numeric ID                           |
| `speed`   | number   | `PIPER_SPEED`    | Speed multiplier: 1.5 = 50% faster, 0.75 = slower   |

Speed range is 0.25 to 4.0. The `speaker` field accepts either a name (`"spike"`) or
numeric ID (`"1"`).

**Response**: `audio/ogg` bytes (OGG/Opus, mono, 24000 Hz input sample rate).

### `GET /health`

Returns JSON with the loaded model, default speaker, and available speaker names.

## Examples

```sh
# Build
docker compose --profile voice build piper

# Run standalone
docker run --rm -d --name piper -p 5000:5000 corre-piper

# Health check
curl http://localhost:5000/health

# Synthesize, piping output to mplayer
curl -X POST -H "Content-Type: application/json" \
  -d '{"text":"Hello! Are you ready? to get that heart rate pumping?", "speaker":"prudence", "speed": 0.75}' \
  http://localhost:5000/synthesize | mplayer -

# Save to disk
curl -X POST -H "Content-Type: application/json" \
  -d '{"text":"Hey, I am Spike!", "speaker":"spike"}' \
  http://localhost:5000/synthesize --output spike.ogg

curl -X POST -H "Content-Type: application/json" \
  -d '{"text":"Good day, I am Obadiah.", "speaker":"obadiah"}' \
  http://localhost:5000/synthesize --output obadiah.ogg

curl -X POST -H "Content-Type: application/json" \
  -d '{"text":"Hi there, I am Poppy!", "speaker":"poppy"}' \
  http://localhost:5000/synthesize --output poppy.ogg

# Speed variations
curl -s -X POST -H "Content-Type: application/json" \
  -d '{"text":"This is normal speed.", "speaker":"prudence", "speed":1.0}' \
  http://localhost:5000/synthesize | mplayer -cache 256 -

curl -s -X POST -H "Content-Type: application/json" \
  -d '{"text":"This is faster.", "speaker":"spike", "speed":1.5}' \
  http://localhost:5000/synthesize | mplayer -cache 256 -

curl -s -X POST -H "Content-Type: application/json" \
  -d '{"text":"This is slower.", "speaker":"obadiah", "speed":0.75}' \
  http://localhost:5000/synthesize | mplayer -cache 256 -
```
