/**
 * Client for the Kelvin API: full, pageable Rialo history, decoded transactions and live updates.
 * No dependencies; runs in browsers and in Node 18 or newer.
 *
 * Amounts are decimal strings in kelvin (1 RLO = 10^9 kelvin) because they exceed 2^53; use
 * `formatRlo` or `BigInt` for arithmetic.
 */

export type Network = "testnet" | "devnet";
export type Order = "asc" | "desc";

export interface Role {
  signer: boolean;
  writable: boolean;
}

export interface InstructionBrief {
  program: string;
  program_name: string | null;
  /** Decoded instruction type such as `transfer`; `null` when Kelvin has no decoder. */
  kind: string | null;
}

export interface Transfer {
  from: string;
  to: string;
  /** Kelvin for RLO, base units for tokens. */
  amount: string;
  /** `null` for native RLO. */
  mint: string | null;
  instruction?: number;
  inner_instruction?: number;
}

export interface TxSummary {
  signature: string;
  block: number;
  index: number;
  time: string;
  time_ms: number;
  status: "success" | "failed";
  error?: unknown;
  fee: string;
  /** How the queried address takes part (address lists only). */
  role?: Role;
  instructions: InstructionBrief[];
  transfers: Transfer[];
}

export interface TxDetail extends Omit<TxSummary, "instructions" | "role"> {
  compute_units: number | null;
  valid_from_ms: number | null;
  fee_payer: string;
  accounts: { address: string; signer: boolean; writable: boolean }[];
  instructions: (InstructionBrief & { index: number; inner_index?: number; data: string; accounts: string[] })[];
  logs: string[] | null;
}

export interface AddressTransfer extends Transfer {
  signature: string;
  block: number;
  index: number;
  time: string;
  time_ms: number;
  direction: "in" | "out";
}

export interface BlockBrief {
  height: number;
  time: string;
  time_ms: number;
  transaction_count: number;
  system_transaction_count: number;
}

export interface Block extends BlockBrief {
  transactions: TxSummary[];
}

export interface Page<T> {
  items: T[];
  /** Pass back as `cursor` for the next page; `null` on the last one. */
  next_cursor: string | null;
}

export interface PageOptions {
  /** 1 to 100, default 25. */
  limit?: number;
  order?: Order;
  cursor?: string;
}

export interface AddressSummary {
  address: string;
  transaction_count: number;
  first_seen?: { block: number; time: string; signature: string } | null;
  last_seen?: { block: number; time: string; signature: string } | null;
  rlo_transfers?: { received?: { count: number; total: string }; sent?: { count: number; total: string } };
  /** Live from the RPC, in kelvin; `null` if it did not answer in time. */
  balance: string | null;
}

export interface Status {
  network: Network;
  indexing: boolean;
  head: number;
  indexed_tip: number | null;
  lag_blocks: number | null;
  backfill: { from: number; next: number; until: number | null; complete: boolean; progress: number };
  oldest_block_served_by_rpc: number | null;
  missing_blocks: number;
  totals: { blocks: number | null; transactions: number | null; addresses: number | null; transfers: number | null };
}

export interface Subscription {
  /** Only this address's transactions; block events are not delivered. */
  address?: string;
  onBlock?: (block: BlockBrief) => void;
  onTx?: (tx: TxSummary) => void;
  onError?: (error: unknown) => void;
  /** Polling interval when server-sent events are unavailable or held back by a proxy. Default 4000 ms. */
  pollIntervalMs?: number;
}

export class KelvinError extends Error {
  constructor(
    message: string,
    readonly status: number,
  ) {
    super(message);
    this.name = "KelvinError";
  }
}

/** `"1500000000"` → `"1.5"`. Exact for any size. */
export function formatRlo(kelvin: string | bigint, maxDecimals = 9): string {
  const k = BigInt(kelvin);
  const neg = k < 0n;
  const a = neg ? -k : k;
  const whole = a / 1_000_000_000n;
  const frac = (a % 1_000_000_000n).toString().padStart(9, "0").slice(0, maxDecimals).replace(/0+$/, "");
  return (neg ? "-" : "") + whole.toString() + (frac ? "." + frac : "");
}

export class Kelvin {
  /** `https://host/v1/testnet` */
  readonly base: string;
  private readonly fetchFn: typeof fetch;

  constructor(options: { baseUrl: string; network?: Network; fetch?: typeof fetch }) {
    this.base = `${options.baseUrl.replace(/\/$/, "")}/v1/${options.network ?? "testnet"}`;
    this.fetchFn = options.fetch ?? globalThis.fetch.bind(globalThis);
  }

  private async get<T>(path: string, query: Record<string, string | number | undefined> = {}): Promise<T> {
    const params = new URLSearchParams();
    for (const [k, v] of Object.entries(query)) if (v !== undefined) params.set(k, String(v));
    const qs = params.toString();
    const r = await this.fetchFn(`${this.base}${path}${qs ? "?" + qs : ""}`);
    const body = (await r.json().catch(() => null)) as { error?: string } | null;
    if (!r.ok) throw new KelvinError(body?.error ?? `${r.status} ${r.statusText}`, r.status);
    return body as T;
  }

  /** Head, indexed tip, lag, backfill progress and totals. */
  status(): Promise<Status> {
    return this.get("/status");
  }

  /** The last ~25,000 blocks (about an hour): volumes, success rate, signers, programs called. */
  activity(): Promise<Record<string, unknown>> {
    return this.get("/activity");
  }

  /** Epoch, validators, DKG committee and RPC version. */
  network(): Promise<Record<string, unknown>> {
    return this.get("/network");
  }

  /** The Rialo RPC's known shortcomings, re-checked hourly. */
  conformance(): Promise<{ network: Network; checks: Record<string, unknown>[] }> {
    return this.get("/rpc-conformance");
  }

  tx(signature: string): Promise<TxDetail> {
    return this.get(`/tx/${signature}`);
  }

  /** The workflow lineage of a transaction (`getWorkflowLineage`). */
  workflow(signature: string): Promise<Record<string, unknown>> {
    return this.get(`/tx/${signature}/workflow`);
  }

  block(height: number): Promise<Block> {
    return this.get(`/blocks/${height}`);
  }

  blocks(options: PageOptions = {}): Promise<Page<BlockBrief>> {
    return this.get("/blocks", { ...options });
  }

  programTxs(program: string, options: PageOptions = {}): Promise<Page<TxSummary>> {
    return this.get(`/programs/${program}/txs`, { ...options });
  }

  address(address: string) {
    const path = `/address/${address}`;
    const pages = <T>(sub: string, options: Omit<PageOptions, "cursor"> = {}) => this.iterate<T>(`${path}/${sub}`, options);
    return {
      summary: (): Promise<AddressSummary> => this.get(path),
      txs: (options: PageOptions = {}): Promise<Page<TxSummary>> => this.get(`${path}/txs`, { ...options }),
      transfers: (options: PageOptions = {}): Promise<Page<AddressTransfer>> => this.get(`${path}/transfers`, { ...options }),
      /** Every transaction of the address, fetched page by page as you iterate. */
      history: (options: Omit<PageOptions, "cursor"> = {}) => pages<TxSummary>("txs", options),
      /** Every RLO and token transfer of the address. */
      allTransfers: (options: Omit<PageOptions, "cursor"> = {}) => pages<AddressTransfer>("transfers", options),
      /** Transactions per UTC day and top RLO counterparties. */
      activity: (): Promise<Record<string, unknown>> => this.get(`${path}/activity`),
      /** REX requests the address created. */
      rex: (): Promise<Record<string, unknown>> => this.get(`${path}/rex`),
      /** URL of the full history as CSV. */
      csvUrl: (order: Order = "desc") => `${this.base}${path}/export.csv?order=${order}`,
    };
  }

  private async *iterate<T>(path: string, options: Omit<PageOptions, "cursor">): AsyncGenerator<T> {
    let cursor: string | undefined;
    do {
      const page = await this.get<Page<T>>(path, { limit: options.limit ?? 100, order: options.order, cursor });
      yield* page.items;
      cursor = page.next_cursor ?? undefined;
    } while (cursor);
  }

  /**
   * New blocks and transactions as Kelvin indexes them. Uses server-sent events where the
   * runtime has `EventSource` and the stream arrives (it opens with a `hello` event); otherwise
   * polls. Returns a function that stops the subscription.
   */
  subscribe(s: Subscription): () => void {
    let stopped = false;
    const cleanups: (() => void)[] = [];
    const stop = () => {
      stopped = true;
      cleanups.forEach((f) => f());
    };
    const report = (e: unknown) => s.onError?.(e);
    const poll = () => {
      if (stopped) return;
      const seen = new Set<string>();
      let lastHeight = 0;
      const tick = async () => {
        try {
          if (s.address) {
            const page = await this.address(s.address).txs({ limit: 20 });
            const fresh = page.items.filter((t) => !seen.has(t.signature)).reverse();
            fresh.forEach((t) => seen.add(t.signature));
            if (lastHeight) fresh.forEach((t) => s.onTx?.(t));
            lastHeight = 1;
          } else {
            const page = await this.blocks({ limit: 30 });
            const fresh = page.items.filter((b) => b.height > lastHeight).reverse();
            const first = lastHeight === 0;
            if (fresh.length) lastHeight = fresh[fresh.length - 1].height;
            if (first) return;
            for (const b of fresh) {
              s.onBlock?.(b);
              if (s.onTx && b.transaction_count > b.system_transaction_count) {
                const full = await this.block(b.height);
                full.transactions.forEach((t) => s.onTx?.(t));
              }
            }
          }
        } catch (e) {
          report(e);
        }
      };
      void tick();
      const t = setInterval(tick, s.pollIntervalMs ?? 4000);
      cleanups.push(() => clearInterval(t));
    };

    const ES = (globalThis as { EventSource?: typeof EventSource }).EventSource;
    if (!ES) {
      poll();
      return stop;
    }
    const es = new ES(`${this.base}/stream${s.address ? `?address=${encodeURIComponent(s.address)}` : ""}`);
    let hello = false;
    es.addEventListener("hello", () => (hello = true));
    es.addEventListener("block", (e) => s.onBlock?.(JSON.parse((e as MessageEvent).data)));
    es.addEventListener("tx", (e) => s.onTx?.(JSON.parse((e as MessageEvent).data)));
    es.onerror = (e) => {
      if (hello) report(e);
    };
    cleanups.push(() => es.close());
    const check = setTimeout(() => {
      if (!hello) {
        es.close();
        poll();
      }
    }, 6000);
    cleanups.push(() => clearTimeout(check));
    return stop;
  }
}
