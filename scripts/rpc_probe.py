#!/usr/bin/env python3
"""Rialo RPC probe: head, retention window, throughput, sample mix.
Usage: python3 -I rpc_probe.py [testnet|devnet]
"""
import json, http.client, ssl, sys, time, random, collections, concurrent.futures as cf

NET = sys.argv[1] if len(sys.argv) > 1 else "testnet"
HOST = f"{NET}.rialo.io"
CTX = ssl.create_default_context(cafile="/etc/ssl/cert.pem")


def conn():
    return http.client.HTTPSConnection(HOST, timeout=30, context=CTX)


def call(c, method, params):
    body = json.dumps({"jsonrpc": "2.0", "id": 1, "method": method, "params": params})
    c.request("POST", "/", body, {"content-type": "application/json"})
    return json.loads(c.getresponse().read())


def main():
    c = conn()
    head = call(c, "getBlockHeight", [])["result"]
    lo, hi = 1, head
    while lo < hi:
        mid = (lo + hi) // 2
        if "result" in call(c, "getBlock", [{"blockHeight": mid}]):
            hi = mid
        else:
            lo = mid + 1
    print(f"{HOST} head={head} earliest={lo} retained={head - lo}")

    heights = [head - random.randint(10, 2_000_000) for _ in range(120)]

    def worker(chunk):
        cc, out = conn(), []
        for h in chunk:
            d = call(cc, "getBlock", [{"blockHeight": h}])
            if "result" in d:
                out.append(d["result"]["value"])
        return out

    t = time.time()
    blocks = []
    with cf.ThreadPoolExecutor(12) as ex:
        for r in ex.map(worker, [heights[i::12] for i in range(12)]):
            blocks += r
    dt = time.time() - t
    progs, ntx = collections.Counter(), 0
    for b in blocks:
        for tx in b["transactions"]:
            ntx += 1
            m = tx["transaction"]["message"]
            for ins in m["instructions"]:
                progs[m["accountKeys"][ins["programIdIndex"]]] += 1
    print(f"sampled {len(blocks)} blocks in {dt:.1f}s ({len(blocks)/dt:.1f} blk/s, 12 workers), tx/block {ntx/len(blocks):.2f}")
    for p, n in progs.most_common(8):
        print(f"  {n:4d} {p}")


if __name__ == "__main__":
    main()
