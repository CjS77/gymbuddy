# Whisper STT Server

HTTP server for speech-to-text using [whisper.cpp](https://github.com/ggml-org/whisper.cpp).
Built from source in a multi-stage Docker build — no compilers or build artefacts in the
final image.

The `--convert` flag enables ffmpeg input conversion, so the server accepts OGG/Opus directly
from Telegram (or any other audio format ffmpeg supports) without requiring WAV input.

## Models

The default model is `ggml-base.en.bin`. Set the `WHISPER_MODEL` build arg to change it.

| Model                | Size  | CPU speed | Accuracy                |
|----------------------|-------|-----------|-------------------------|
| `ggml-tiny.en.bin`   | 75MB  | ~0.5s     | Good for clear speech   |
| `ggml-base.en.bin`   | 142MB | 1-3s      | Good default            |
| `ggml-small.en.bin`  | 466MB | 5-10s     | Better for noisy audio  |

Build with a different model:

```sh
docker compose --profile voice build --build-arg WHISPER_MODEL=ggml-small.en.bin whisper
```

## GPU support

GPU acceleration is a deploy-time concern. The Rust code and HTTP API are identical.

1. Build with a CUDA base image:
   ```sh
   docker build \
     --build-arg BASE_IMAGE=nvidia/cuda:12.8.0-runtime-ubuntu24.04 \
     -t corre-whisper:cuda services/whisper/
   ```

2. Uncomment the `deploy` section in `docker-compose.yml`:
   ```yaml
   deploy:
     resources:
       reservations:
         devices:
           - driver: nvidia
             count: 1
             capabilities: [gpu]
   ```

Requires `nvidia-container-toolkit` on the host.

## API

### `POST /inference`

Transcribe audio to text. Accepts any audio format (OGG, WAV, MP3, etc.) via multipart form.

**Request**: multipart form with a `file` field containing the audio bytes.

**Response** (JSON):

```json
{
  "text": " And so my fellow Americans, ask not what your country can do for you..."
}
```

Some whisper.cpp versions return segmented output instead:

```json
{
  "segments": [
    {"text": " And so my fellow Americans,"},
    {"text": " ask not what your country can do for you..."}
  ]
}
```

The corre-gym client handles both formats.

### `GET /health`

Returns 200 when the server is ready.

## Examples

```sh
# Build
docker compose --profile voice build whisper

# Run standalone
docker run --rm -d --name whisper -p 5005:5005 corre-whisper

# Health check
curl http://localhost:5005/health

# Transcribe a WAV file
curl -F "file=@samples_jfk.wav" http://localhost:5005/inference

# Transcribe an OGG file (from Telegram voice message)
curl -F "file=@voice.ogg" http://localhost:5005/inference

# Pipe output through jq
curl -s -F "file=@samples_jfk.wav" http://localhost:5005/inference | jq -r .text
```
