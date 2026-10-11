#!/bin/sh
# Ships the kelvin image and deploy/compose.yaml to an Ubuntu server over SSH and starts the stack.
#
#   deploy/deploy.sh ubuntu@1.2.3.4                    # build, upload, (re)start
#   deploy/deploy.sh ubuntu@1.2.3.4 --restore          # also copy this Mac's database first
#
# The image is built here for linux/arm64 (Oracle Ampere A1). --restore stops the local indexer,
# dumps the local Postgres and loads it into the server's empty database, so the server resumes
# from the same cursors. SSH_KEY defaults to ~/.ssh/kelvin_oracle_ed25519.
set -eu
cd "$(dirname "$0")/.."

TARGET=${1:?usage: deploy/deploy.sh user@host [--restore]}
RESTORE=${2:-}
KEY=${SSH_KEY:-$HOME/.ssh/kelvin_oracle_ed25519}
SSH_OPTS="-i $KEY -o StrictHostKeyChecking=accept-new"
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT

echo "== building image"
docker build -q --platform linux/arm64 -t kelvin:latest . >/dev/null
docker save kelvin:latest | gzip -1 > "$WORK/kelvin-image.tar.gz"

if [ "$RESTORE" = "--restore" ]; then
    echo "== stopping the local indexer and dumping its database"
    scripts/indexer.sh stop
    docker exec kelvin-db-1 pg_dump -U kelvin -Fc kelvin > "$WORK/kelvin.dump"
fi

echo "== uploading"
# shellcheck disable=SC2086
scp $SSH_OPTS deploy/compose.yaml "$WORK"/* "$TARGET:/tmp/"

echo "== installing and starting on $TARGET"
# shellcheck disable=SC2086
ssh $SSH_OPTS "$TARGET" "RESTORE='$RESTORE' sh -s" <<'REMOTE'
set -eu
if ! command -v docker >/dev/null; then
    sudo apt-get update -q
    sudo DEBIAN_FRONTEND=noninteractive apt-get install -yq docker.io docker-compose-v2
fi
sudo mkdir -p /opt/kelvin /srv/kelvin/pg
sudo mv /tmp/compose.yaml /opt/kelvin/compose.yaml
gunzip -c /tmp/kelvin-image.tar.gz | sudo docker load -q
rm /tmp/kelvin-image.tar.gz
cd /opt/kelvin
if [ "$RESTORE" = "--restore" ]; then
    sudo docker compose up -d --wait db
    if [ "$(sudo docker compose exec -T db psql -U kelvin -Atc "SELECT to_regclass('public.blocks') IS NULL")" = "t" ]; then
        sudo docker compose exec -T db pg_restore -U kelvin -d kelvin --no-owner < /tmp/kelvin.dump
        echo "restored the database"
    else
        echo "the server already has data; skipped the restore"
    fi
    rm /tmp/kelvin.dump
fi
sudo docker compose up -d
sleep 15
sudo docker compose ps --format '{{.Service}}: {{.Status}}'
echo "public URL: $(sudo docker compose logs tunnel 2>&1 | grep -o 'https://[a-z0-9-]*\.trycloudflare\.com' | tail -1)"
REMOTE
