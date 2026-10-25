// A tiny MCP server (Streamable HTTP) for the console's tests, FAKE_MCP_PORT (default 18099):
// /mcp wants `Bearer e2e-token`; /signed/mcp signs people in with OAuth (MCP authorization:
// resource metadata, registration, PKCE) and takes the token it gave.
import { createHash } from "node:crypto";
import http from "node:http";

const port = Number(process.env.FAKE_MCP_PORT ?? 18099);
const base = `http://127.0.0.1:${port}`;
const tools = [
  { name: "search_issues", description: "Finds issues by a query.", annotations: { readOnlyHint: true } },
  { name: "update_issue", description: "Changes an issue.", annotations: { readOnlyHint: false } },
];
const challenges = new Map();

const json = (res, value, status = 200, headers = {}) => {
  res.writeHead(status, { "content-type": "application/json", ...headers });
  res.end(JSON.stringify(value));
};

function mcp(res, message) {
  const answer = (result) => json(res, { jsonrpc: "2.0", id: message.id, result }, 200, { "mcp-session-id": "e2e" });
  if (message.method === "initialize") answer({ protocolVersion: "2025-06-18", capabilities: { tools: {} }, serverInfo: { name: "e2e-tracker", version: "1" } });
  else if (message.method === "tools/list") answer({ tools });
  else res.writeHead(202).end();
}

http
  .createServer((req, res) => {
    let body = "";
    req.on("data", (chunk) => (body += chunk));
    req.on("end", () => {
      const url = new URL(req.url, base);
      const auth = req.headers.authorization;
      if (url.pathname === "/mcp") {
        if (auth !== "Bearer e2e-token") return res.writeHead(401).end();
        return mcp(res, JSON.parse(body || "{}"));
      }
      if (url.pathname === "/signed/mcp") {
        if (auth !== "Bearer signed-token") {
          res.writeHead(401, { "www-authenticate": `Bearer resource_metadata="${base}/.well-known/oauth-protected-resource/signed/mcp"` });
          return res.end();
        }
        return mcp(res, JSON.parse(body || "{}"));
      }
      if (url.pathname === "/.well-known/oauth-protected-resource/signed/mcp") return json(res, { resource: `${base}/signed/mcp`, authorization_servers: [base] });
      if (url.pathname === "/.well-known/oauth-authorization-server")
        return json(res, { issuer: base, authorization_endpoint: `${base}/authorize`, token_endpoint: `${base}/token`, registration_endpoint: `${base}/register` });
      if (url.pathname === "/register") return json(res, { client_id: "e2e-client" }, 201);
      if (url.pathname === "/authorize") {
        // Signs the person in at once and sends them back.
        challenges.set("e2e-code", url.searchParams.get("code_challenge"));
        const back = new URL(url.searchParams.get("redirect_uri"));
        back.searchParams.set("code", "e2e-code");
        back.searchParams.set("state", url.searchParams.get("state"));
        res.writeHead(302, { location: back.toString() });
        return res.end();
      }
      if (url.pathname === "/token") {
        const form = new URLSearchParams(body);
        const challenge = createHash("sha256").update(form.get("code_verifier") ?? "").digest("base64url");
        if (form.get("code") !== "e2e-code" || challenges.get("e2e-code") !== challenge) return json(res, { error: "invalid_grant" }, 400);
        return json(res, { access_token: "signed-token", token_type: "Bearer", expires_in: 3600 });
      }
      res.writeHead(404).end();
    });
  })
  .listen(port, "127.0.0.1");
