// Forwards a request to the Kelvin server in env.BACKEND and streams the answer back.
export async function proxy({ request, env }) {
  if (!env.BACKEND) return new Response(JSON.stringify({ error: "BACKEND is not configured" }), { status: 503 });
  const url = new URL(request.url);
  const target = env.BACKEND.replace(/\/$/, "") + url.pathname + url.search;
  const headers = new Headers(request.headers);
  headers.delete("host");
  // Plain bodies from the server; Cloudflare compresses for each client as that client accepts.
  headers.set("accept-encoding", "identity");
  // The server rate-limits per visitor; every request reaches it from Cloudflare's addresses.
  headers.set("x-kelvin-client-ip", request.headers.get("cf-connecting-ip") ?? "unknown");
  let upstream;
  try {
    upstream = await fetch(target, { method: request.method, headers, redirect: "manual" });
  } catch (e) {
    return Response.json({ error: "The Kelvin server is unreachable right now; retrying shortly usually works." }, { status: 502 });
  }
  if (upstream.status === 530 || upstream.status === 1033) {
    return Response.json({ error: "The Kelvin server is offline right now." }, { status: 502 });
  }
  return new Response(upstream.body, upstream);
}
