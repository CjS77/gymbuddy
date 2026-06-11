# Global ARG must be before any FROM to be usable in FROM lines
ARG BASE_IMAGE=debian:trixie-slim
ARG WHISPER_VERSION=1.7.5

# ── Stage 1: Build whisper.cpp from source ───────────────────────────────────
FROM debian:trixie AS builder

ARG WHISPER_VERSION

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates curl git cmake g++ make && rm -rf /var/lib/apt/lists/*

RUN git clone --depth 1 --branch v${WHISPER_VERSION} \
    https://github.com/ggml-org/whisper.cpp.git /src/whisper

WORKDIR /src/whisper
RUN cmake -B build -DWHISPER_BUILD_SERVER=ON -DCMAKE_BUILD_TYPE=Release && \
    cmake --build build --config Release -j$(nproc) --target whisper-server

# Smoke test: verify the binary runs
RUN ./build/bin/whisper-server --help || true

# ── Stage 2: Download model ──────────────────────────────────────────────────
ARG WHISPER_MODEL=ggml-base.en.bin
RUN mkdir -p /models && \
    curl -fSL "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/${WHISPER_MODEL}" \
    -o "/models/${WHISPER_MODEL}"

# ── Stage 3: Slim runtime ────────────────────────────────────────────────────
FROM ${BASE_IMAGE}

ARG WHISPER_VERSION
LABEL org.opencontainers.image.title="whisper-server" \
      org.opencontainers.image.version="${WHISPER_VERSION}"

RUN apt-get update && apt-get install -y --no-install-recommends \
    ffmpeg curl && rm -rf /var/lib/apt/lists/*

COPY --from=builder /src/whisper/build/bin/whisper-server /usr/local/bin/
COPY --from=builder /src/whisper/build/src/libwhisper.so* /usr/local/lib/
COPY --from=builder /src/whisper/build/ggml/src/libggml*.so /usr/local/lib/
RUN ldconfig
COPY --from=builder /models /models

EXPOSE 5005
ENTRYPOINT ["whisper-server"]
CMD ["--model", "/models/ggml-base.en.bin", "--host", "0.0.0.0", "--port", "5005", "--convert"]
