// A tiny MCP server (Streamable HTTP) for the console's tests: it wants `Bearer e2e-token`
// and offers a reading tool and a changing one. FAKE_MCP_PORT, default 18099.
import http from "node:http";

const port = Number(process.env.FAKE_MCP_PORT ?? 18099);
const tools = [
  { name: "search_issues", description: "Finds issues by a query.", annotations: { readOnlyHint: true } },
  { name: "update_issue", description: "Changes an issue.", annotations: { readOnlyHint: false } },
];

http
  .createServer((req, res) => {
    let body = "";
    req.on("data", (chunk) => (body += chunk));
    req.on("end", () => {
      if (req.headers.authorization !== "Bearer e2e-token") {
        res.writeHead(401).end();
        return;
      }
      const message = JSON.parse(body || "{}");
      const answer = (result) => {
        res.writeHead(200, { "content-type": "application/json", "mcp-session-id": "e2e" });
        res.end(JSON.stringify({ jsonrpc: "2.0", id: message.id, result }));
      };
      if (message.method === "initialize") answer({ protocolVersion: "2025-06-18", capabilities: { tools: {} }, serverInfo: { name: "e2e-tracker", version: "1" } });
      else if (message.method === "tools/list") answer({ tools });
      else res.writeHead(202).end();
    });
  })
  .listen(port, "127.0.0.1");
