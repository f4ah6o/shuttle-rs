// Local smoke-test server: serves the Worker's `handle` over Node HTTP with
// the node:sqlite-backed Database used by the test suite. Not for production.
import { createServer } from "node:http";

const { handle } = await import("../dist-dev/src/index.js");
const { NodeSqliteDatabase } = await import("../dist-dev/test/helpers.js");

const env = {
  ADMIN_BOOTSTRAP_TOKEN: process.env.ADMIN_BOOTSTRAP_TOKEN ?? "bootstrap-secret",
  ADMIN_OWNER_ID: "owner-dev",
};
const db = new NodeSqliteDatabase();
const port = Number(process.env.PORT ?? 8788);

createServer(async (req, res) => {
  const chunks = [];
  for await (const chunk of req) chunks.push(chunk);
  const body = Buffer.concat(chunks);
  const request = new Request(`http://127.0.0.1:${port}${req.url}`, {
    method: req.method,
    headers: req.headers,
    body: body.length > 0 ? body : undefined,
  });
  const response = await handle(request, env, db);
  res.writeHead(response.status, Object.fromEntries(response.headers));
  res.end(Buffer.from(await response.arrayBuffer()));
}).listen(port, () => console.log(`gateway dev server on :${port}`));
