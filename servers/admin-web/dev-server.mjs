import { createReadStream, statSync } from "node:fs";
import { createServer, request as proxyRequest } from "node:http";
import { extname, join, normalize } from "node:path";
import { fileURLToPath } from "node:url";

const root = fileURLToPath(new URL(".", import.meta.url));
const host = process.env.BOKHEIM_ADMIN_DEV_HOST ?? "127.0.0.1";
const port = Number(process.env.BOKHEIM_ADMIN_DEV_PORT ?? 4174);
const api = new URL(process.env.BOKHEIM_ADMIN_API ?? "http://127.0.0.1:8080");
const types = { ".css": "text/css; charset=utf-8", ".html": "text/html; charset=utf-8", ".js": "text/javascript; charset=utf-8" };

function proxy(request, response) {
  const headers = { ...request.headers, "x-forwarded-for": request.socket.remoteAddress ?? "127.0.0.1", "x-forwarded-proto": "http" };
  const upstream = proxyRequest(new URL(request.url, api), { method: request.method, headers }, (upstreamResponse) => {
    response.writeHead(upstreamResponse.statusCode ?? 502, upstreamResponse.headers);
    upstreamResponse.pipe(response);
  });
  upstream.on("error", () => {
    if (!response.headersSent) response.writeHead(502, { "Content-Type": "text/plain; charset=utf-8" });
    response.end("The local sync server is unavailable.");
  });
  request.pipe(upstream);
}

function serve(request, response) {
  const pathname = decodeURIComponent(new URL(request.url, `http://${host}:${port}`).pathname);
  const relative = pathname === "/" ? "index.html" : normalize(pathname).replace(/^[/\\]+/, "");
  const path = join(root, relative);
  try {
    if (!path.startsWith(root) || !statSync(path).isFile()) throw new Error("not found");
    response.writeHead(200, {
      "Cache-Control": "no-store",
      "Content-Security-Policy": "default-src 'self'; connect-src 'self'; img-src 'self'; style-src 'self'; script-src 'self'; object-src 'none'; base-uri 'none'; form-action 'self'; frame-ancestors 'none'",
      "Content-Type": types[extname(path)] ?? "application/octet-stream",
      "X-Content-Type-Options": "nosniff",
    });
    createReadStream(path).pipe(response);
  } catch {
    response.writeHead(404, { "Content-Type": "text/plain; charset=utf-8" });
    response.end("Not found");
  }
}

createServer((request, response) => request.url?.startsWith("/api/") ? proxy(request, response) : serve(request, response)).listen(port, host, () => {
  console.log(`Bokheim administrator dashboard: http://${host}:${port}/`);
  console.log(`Proxying API requests to ${api.origin}`);
});
