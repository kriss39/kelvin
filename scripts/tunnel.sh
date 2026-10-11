#!/bin/sh
# Public HTTPS address for the local explorer and API through a Cloudflare quick tunnel.
# No account and no open ports; the https://<random>.trycloudflare.com address changes on every start.
set -e
cd "$(dirname "$0")/.."
case "${1:-start}" in
start)
    if pgrep -f "cloudflared tunnel.*127.0.0.1:8080" >/dev/null; then
        echo "tunnel already running"
    else
        mkdir -p logs
        : > logs/tunnel.log
        nohup cloudflared tunnel --no-autoupdate --protocol http2 --url http://127.0.0.1:8080 >> logs/tunnel.log 2>&1 &
        until grep -qo 'https://[a-z0-9-]*\.trycloudflare\.com' logs/tunnel.log; do sleep 1; done
    fi
    url=$(grep -o 'https://[a-z0-9-]*\.trycloudflare\.com' logs/tunnel.log | tail -1)
    echo "public URL: $url"
    # Keep kelvinscan.pages.dev pointing at this tunnel when its address changes.
    if [ "$url" != "$(cat logs/pages-backend.txt 2>/dev/null)" ] && [ -x deploy/pages/deploy.sh ]; then
        if deploy/pages/deploy.sh "$url" > logs/pages-deploy.log 2>&1; then
            echo "$url" > logs/pages-backend.txt
            echo "https://kelvinscan.pages.dev now forwards to it"
        else
            echo "could not update kelvinscan.pages.dev; see logs/pages-deploy.log"
        fi
    fi
    ;;
stop)
    if pkill -f "cloudflared tunnel.*127.0.0.1:8080"; then
        while pgrep -f "cloudflared tunnel.*127.0.0.1:8080" >/dev/null; do sleep 0.5; done
        echo "tunnel stopped"
    else
        echo "tunnel is not running"
    fi
    ;;
url)
    grep -o 'https://[a-z0-9-]*\.trycloudflare\.com' logs/tunnel.log 2>/dev/null | tail -1
    ;;
*)
    echo "usage: $0 [start|stop|url]" >&2
    exit 1
    ;;
esac
