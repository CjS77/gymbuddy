#!/usr/bin/env bash
#
# Start the gymbuddy voice pipeline: whisper (STT) + piper (TTS) containers,
# then run the gymbuddy bot locally via cargo.
#
# Usage:
#   ./start.sh                              # production-style run, uses corre.toml voice URLs as-is
#   ./start.sh --dev                        # dev mode: override voice URLs to point at localhost
#   ./start.sh -c /path/to/gymbuddy.toml       # any extra args are forwarded to gymbuddy
#   ./start.sh --dev -c /path/to/gymbuddy.toml
#
# In --dev mode the script exports CORRE_GYM_STT_URL/CORRE_GYM_TTS_URL so voice
# requests hit the host-published container ports rather than Docker network
# hostnames. Use this when running gymbuddy from your local cargo build while
# the voice services run in Docker.
#
# On exit (Ctrl-C or process end), both containers are stopped automatically.
set -euo pipefail

export RUST_LOG=warn,gymbuddy=debug,gymbuddy:*=debug
DEV_MODE=0
PASSTHROUGH_ARGS=()
while [[ $# -gt 0 ]]; do
    case "$1" in
        --dev)
            DEV_MODE=1
            shift
            ;;
        *)
            PASSTHROUGH_ARGS+=("$1")
            shift
            ;;
    esac
done

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR" && pwd)"

WHISPER_PORT=5005
PIPER_PORT=5000
WHISPER_CONTAINER=corre-whisper-dev
PIPER_CONTAINER=corre-piper-dev

# ── Helpers ──────────────────────────────────────────────────────────────────

cleanup() {
    echo ""
    echo "Stopping containers..."
    docker stop "$WHISPER_CONTAINER" "$PIPER_CONTAINER" 2>/dev/null || true
    echo "Done."
}
trap cleanup EXIT

wait_healthy() {
    local name="$1" url="$2" timeout="${3:-30}"
    echo -n "  Waiting for $name"
    for i in $(seq 1 "$timeout"); do
        if curl -sf "$url" >/dev/null 2>&1; then
            echo " ready (${i}s)"
            return 0
        fi
        echo -n "."
        sleep 1
    done
    echo " FAILED after ${timeout}s"
    echo "  Logs:"
    docker logs --tail 20 "$name" 2>&1 | sed 's/^/    /'
    return 1
}

# ── Build containers if needed ───────────────────────────────────────────────

echo "Building voice containers (if needed)..."
(cd "$REPO_ROOT" && docker compose --profile voice build whisper piper)

# ── Start containers ─────────────────────────────────────────────────────────

# Stop any previous dev instances
docker stop "$WHISPER_CONTAINER" "$PIPER_CONTAINER" 2>/dev/null || true
docker rm "$WHISPER_CONTAINER" "$PIPER_CONTAINER" 2>/dev/null || true

echo ""
echo "Starting whisper on localhost:${WHISPER_PORT}..."
docker run --rm -d \
    --name "$WHISPER_CONTAINER" \
    -p "${WHISPER_PORT}:${WHISPER_PORT}" \
    corre-whisper

echo "Starting piper on localhost:${PIPER_PORT}..."
docker run --rm -d \
    --name "$PIPER_CONTAINER" \
    -p "${PIPER_PORT}:${PIPER_PORT}" \
    corre-piper

# ── Wait for healthy ─────────────────────────────────────────────────────────

echo ""
wait_healthy "$WHISPER_CONTAINER" "http://localhost:${WHISPER_PORT}/health" 30
wait_healthy "$PIPER_CONTAINER" "http://localhost:${PIPER_PORT}/health" 15

# ── Run gymbuddy ────────────────────────────────────────────────────────────

echo ""
echo "Voice pipeline ready:"
echo "  STT: http://localhost:${WHISPER_PORT}"
echo "  TTS: http://localhost:${PIPER_PORT}"
echo ""

if [[ "$DEV_MODE" -eq 1 ]]; then
    echo "Dev mode: overriding voice URLs to localhost"
    export CORRE_GYM_STT_URL="http://localhost:${WHISPER_PORT}"
    export CORRE_GYM_TTS_URL="http://localhost:${PIPER_PORT}"
fi

cargo run -p gymbuddy-backend --release -- "${PASSTHROUGH_ARGS[@]}"
