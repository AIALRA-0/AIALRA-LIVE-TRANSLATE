// Serve one ignored synthetic audio fixture over HTTPS with Basic authentication.
import { createReadStream, readFileSync, statSync } from "node:fs";
import { createServer } from "node:https";
import { basename } from "node:path";

const [certPath, keyPath, fixturePath, password, requestedPort = "0"] = process.argv.slice(2);
if (!certPath || !keyPath || !fixturePath || !password) {
  throw new Error("usage: node tools/https_fixture_server.mjs <cert> <key> <fixture> <password> [port]");
}
if (!/^\S{8,128}$/.test(password)) throw new Error("fixture password must be 8-128 non-space characters");
const fixtureSize = statSync(fixturePath).size;
const expected = `Basic ${Buffer.from(`soak:${password}`).toString("base64")}`;
const server = createServer({ cert: readFileSync(certPath), key: readFileSync(keyPath) }, (request, response) => {
  if (request.url !== "/fixture.wav") {
    response.writeHead(404).end();
    return;
  }
  if (request.headers.authorization !== expected) {
    response.writeHead(401, { "www-authenticate": 'Basic realm="aialra-test-fixture"' }).end();
    return;
  }
  response.writeHead(200, {
    "content-type": "audio/wav",
    "content-length": fixtureSize,
    "cache-control": "no-store",
    "content-disposition": `inline; filename="${basename(fixturePath)}"`,
  });
  createReadStream(fixturePath).pipe(response);
});

server.listen(Number(requestedPort), "127.0.0.1", () => {
  const address = server.address();
  if (!address || typeof address === "string") throw new Error("fixture server did not bind a TCP port");
  process.stdout.write(`${JSON.stringify({ url: `https://127.0.0.1:${address.port}/fixture.wav`, fixture_size: fixtureSize })}\n`);
});

const shutdown = () => server.close(() => process.exit(0));
process.once("SIGINT", shutdown);
process.once("SIGTERM", shutdown);
