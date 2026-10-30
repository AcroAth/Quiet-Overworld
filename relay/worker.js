// Quiet Overworld's audio relay, for Cloudflare Workers (free plan is plenty).
//
// Mojang's asset server labels the music as application/octet-stream, which
// Safari's engine refuses to play, and every browser on an iPhone uses that
// engine. This passes a file through with an audio type. It streams the bytes
// as they arrive and stores nothing.
//
// GET /<sha1>  ->  https://resources.download.minecraft.net/<sh>/<sha1>

const ORIGIN = "https://resources.download.minecraft.net";
const PASSED_ON = ["Content-Length", "Content-Range", "Accept-Ranges", "ETag", "Last-Modified"];

export default {
  async fetch(request) {
    if (request.method !== "GET" && request.method !== "HEAD") {
      return new Response(null, { status: 405, headers: { Allow: "GET, HEAD" } });
    }
    const hash = new URL(request.url).pathname.slice(1);
    if (!/^[0-9a-f]{40}$/.test(hash)) {
      return new Response("Not found", { status: 404 });
    }
    // Safari asks for byte ranges when it plays media; pass them through.
    const headers = new Headers();
    const range = request.headers.get("Range");
    if (range) headers.set("Range", range);
    const upstream = await fetch(`${ORIGIN}/${hash.slice(0, 2)}/${hash}`, { method: request.method, headers });

    const out = new Headers({
      "Content-Type": upstream.ok ? "audio/ogg" : "text/plain",
      "Access-Control-Allow-Origin": "*",
      "Cache-Control": "no-store",
    });
    for (const name of PASSED_ON) {
      const value = upstream.headers.get(name);
      if (value) out.set(name, value);
    }
    return new Response(upstream.body, { status: upstream.status, headers: out });
  },
};
