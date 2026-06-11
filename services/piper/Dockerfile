# Stage 1: download
FROM debian:trixie-slim AS builder

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates curl && rm -rf /var/lib/apt/lists/*

ARG PIPER_VERSION=2023.11.14-2
RUN curl -fSL \
    "https://github.com/rhasspy/piper/releases/download/${PIPER_VERSION}/piper_linux_x86_64.tar.gz" \
    | tar xz -C /usr/local/

# Semaine is a multi-speaker British English model with 4 voices:
#   prudence (0), spike (1), obadiah (2), poppy (3)
# Speaker is selected at runtime via the PIPER_SPEAKER env var or per-request.
ARG PIPER_VOICE=en_GB-semaine-medium
RUN mkdir -p /models && \
    curl -fSL "https://huggingface.co/rhasspy/piper-voices/resolve/main/en/en_GB/semaine/medium/${PIPER_VOICE}.onnx" \
    -o "/models/${PIPER_VOICE}.onnx" && \
    curl -fSL "https://huggingface.co/rhasspy/piper-voices/resolve/main/en/en_GB/semaine/medium/${PIPER_VOICE}.onnx.json" \
    -o "/models/${PIPER_VOICE}.onnx.json"

# Stage 2: slim runtime
FROM debian:trixie-slim

RUN apt-get update && apt-get install -y --no-install-recommends \
    python3 ffmpeg curl && rm -rf /var/lib/apt/lists/*

COPY --from=builder /usr/local/piper /usr/local/piper
COPY --from=builder /models /models

# Piper needs its bundled libs and espeak-ng-data as siblings
ENV LD_LIBRARY_PATH=/usr/local/piper/lib
WORKDIR /usr/local/piper

COPY piper_server.py /app/piper_server.py

EXPOSE 5000
CMD ["python3", "/app/piper_server.py"]
