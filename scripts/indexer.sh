#!/bin/sh
# Kelvin on this Mac: start (also after a reboot), stop, or show progress. Everything resumes from
# its saved cursors, so stopping or shutting down loses nothing.
#
#   testnet: head follower + backfill, and the API and explorer on 127.0.0.1:8080
#   devnet:  head follower only (its backfill is left for a server)
set -e
cd "$(dirname "$0")/.."

running() { pgrep -f "kelvin run --network $1" >/dev/null; }

start_one() {
    net=$1
    shift
    if running "$net"; then
        echo "$net indexer is already running"
        return
    fi
    # caffeinate keeps the Mac from idle-sleeping while indexing (closing the lid still sleeps it).
    nohup caffeinate -is ./target/release/kelvin run --network "$net" "$@" >> "logs/indexer-$net.log" 2>&1 &
    echo "$net indexer started; log: logs/indexer-$net.log"
}

case "${1:-start}" in
start)
    if ! docker info >/dev/null 2>&1; then
        echo "starting Docker Desktop..."
        open -a Docker
        until docker info >/dev/null 2>&1; do sleep 2; done
    fi
    docker compose up -d db
    until docker compose exec -T db pg_isready -U kelvin >/dev/null 2>&1; do sleep 1; done
    mkdir -p logs
    start_one testnet --api 127.0.0.1:8080
    start_one devnet --no-backfill
    echo "explorer: http://127.0.0.1:8080/"
    ;;
stop)
    if pkill -INT -f "kelvin run"; then
        while pgrep -f "kelvin run" >/dev/null; do sleep 1; done
        echo "indexers stopped; progress is saved"
    else
        echo "no indexer is running"
    fi
    ;;
status)
    for net in testnet devnet; do
        ./target/release/kelvin status --network "$net" 2>/dev/null
        if running "$net"; then echo "  process    running"; else echo "  process    not running"; fi
    done
    ;;
*)
    echo "usage: $0 [start|stop|status]" >&2
    exit 1
    ;;
esac
