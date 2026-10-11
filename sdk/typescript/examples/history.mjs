// node examples/history.mjs <address> — counts an address's whole history and prints the oldest transaction.
import { Kelvin, formatRlo } from "../dist/index.js";

const kelvin = new Kelvin({ baseUrl: process.env.KELVIN_API ?? "http://127.0.0.1:8080" });
const address = process.argv[2] ?? "t2J87f5HDtd4UkpszzBCm6nfbRgkJHRrbJtxuWpwVAG";

const summary = await kelvin.address(address).summary();
console.log(`${address}: ${summary.transaction_count} transactions, balance ${formatRlo(summary.balance ?? "0")} RLO`);

let count = 0;
let oldest;
for await (const tx of kelvin.address(address).history({ order: "asc", limit: 100 })) {
  oldest ??= tx;
  count++;
}
console.log(`paged through ${count}; oldest ${oldest?.signature} at ${oldest?.time}`);
