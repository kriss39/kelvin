// node examples/live.mjs [address] — prints new transactions as Kelvin indexes them (Ctrl-C to stop).
import { Kelvin, formatRlo } from "../dist/index.js";

const kelvin = new Kelvin({ baseUrl: process.env.KELVIN_API ?? "http://127.0.0.1:8080" });
kelvin.subscribe({
  address: process.argv[2],
  onTx: (tx) => {
    const moved = tx.transfers.map((t) => `${formatRlo(t.amount)} ${t.mint ? "tokens" : "RLO"}`).join(", ");
    console.log(tx.time, tx.status.padEnd(7), tx.signature.slice(0, 12), tx.instructions.map((i) => i.kind ?? i.program).join("+"), moved);
  },
  onError: (e) => console.error("error:", e.message ?? e),
});
