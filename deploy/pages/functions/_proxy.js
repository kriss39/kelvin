// Forwards a request to the Kelvin server in env.BACKEND and streams the answer back.
export async function proxy({ request, env }) {
  if (!env.BACKEND) return new Response(JSON.stringify({ error: "BACKEND is not configured" }), { status: 503 });
  const url = new URL(request.url);
  const target = env.BACKEND.replace(/\/$/, "") + url.pathname + url.search;
  const headers = new Headers(request.headers);
  headers.delete("host");
  // Plain bodies from the server; Cloudflare compresses for each client as that client accepts.
  headers.set("accept-encoding", "identity");
  const upstream = await fetch(target, { method: request.method, headers, redirect: "manual" });
  return new Response(upstream.body, upstream);
}
