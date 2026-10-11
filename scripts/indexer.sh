#!/bin/sh
# Testnet indexer on this Mac (head follower + backfill): start (also after a reboot), stop, or
# show progress. It resumes from its saved cursors, so stopping or shutting down loses nothing.
set -e
cd "$(dirname "$0")/.."

case "${1:-start}" in
start)
    if pgrep -f "kelvin run" >/dev/null; then
        echo "indexer is already running"
        exit 0
    fi
    if ! docker info >/dev/null 2>&1; then
        echo "starting Docker Desktop..."
        open -a Docker
        until docker info >/dev/null 2>&1; do sleep 2; done
    fi
    docker compose up -d db
    until docker compose exec -T db pg_isready -U kelvin >/dev/null 2>&1; do sleep 1; done
    mkdir -p logs
    # caffeinate keeps the Mac from idle-sleeping while the indexer runs (closing the lid still sleeps it).
    nohup caffeinate -is ./target/release/kelvin run --network testnet --api 127.0.0.1:8080 >> logs/indexer-testnet.log 2>&1 &
    echo "indexer started; API: http://127.0.0.1:8080/docs; log: logs/indexer-testnet.log"
    ;;
stop)
    if pkill -INT -f "kelvin run"; then
        while pgrep -f "kelvin run" >/dev/null; do sleep 1; done
        echo "indexer stopped; progress is saved"
    else
        echo "indexer is not running"
    fi
    ;;
status)
    ./target/release/kelvin status 2>/dev/null
    if pgrep -f "kelvin run" >/dev/null; then echo "  process    running"; else echo "  process    not running"; fi
    ;;
*)
    echo "usage: $0 [start|stop|status]" >&2
    exit 1
    ;;
esac
