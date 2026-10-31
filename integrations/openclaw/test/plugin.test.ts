// Frood as OpenClaw's memory, against a fake Frood: recall before a turn, the memory tools,
// a wrong token, a stopped Frood.

import { createServer, type Server } from "node:http";
import type { AddressInfo } from "node:net";

import { afterEach, beforeEach, describe, expect, it } from "vitest";

import { type PluginApi, type PluginTool, type PromptAddition, readConfig, registerFrood } from "../src/plugin.js";

interface Fake {
  url: string;
  token: string;
  down: boolean;
  knowledge: Record<string, unknown>[];
  notes: Record<string, unknown>[];
  searches: Record<string, string>[];
  server: Server;
}

async function fakeFrood(): Promise<Fake> {
  const fake = { token: "frood_pat_test", down: false, knowledge: [], notes: [], searches: [] } as unknown as Fake;
  fake.server = createServer((req, res) => {
    const answer = (status: number, body: unknown) => {
      res.writeHead(status, { "Content-Type": "application/json" });
      res.end(JSON.stringify(body));
    };
    if (fake.down) return answer(503, { detail: "down" });
    if (req.headers.authorization !== `Bearer ${fake.token}`) return answer(401, { detail: "the token is not known" });
    const url = new URL(req.url ?? "/", "http://x");
    if (req.method === "GET" && url.pathname === "/v1/search") {
      const query = Object.fromEntries(url.searchParams);
      fake.searches.push(query);
      const words = (query.q ?? "").toLowerCase().split(/[^\p{L}\p{N}]+/u).filter((w) => w.length > 2);
      const hits = fake.knowledge.filter((k) => words.some((w) => String(k.text).toLowerCase().includes(w)));
      return answer(200, hits.slice(0, Number(query.limit ?? 20)));
    }
    if (req.method === "GET" && url.pathname.startsWith("/v1/assertions/")) {
      const id = decodeURIComponent(url.pathname.split("/").pop() ?? "");
      const found = fake.knowledge.find((k) => k.assertion_id === id);
      return found ? answer(200, { ...found, flags: [], links: [] }) : answer(404, { detail: `assertion ${id} not found` });
    }
    if (req.method === "POST" && url.pathname === "/v1/sources") {
      let body = "";
      req.on("data", (chunk) => (body += chunk));
      req.on("end", () => {
        fake.notes.push(JSON.parse(body));
        answer(201, { source: { id: `src-${fake.notes.length}`, evidence: [{ id: "ev" }] } });
      });
      return;
    }
    answer(404, { detail: "not found" });
  });
  await new Promise<void>((resolve) => fake.server.listen(0, "127.0.0.1", resolve));
  fake.url = `http://127.0.0.1:${(fake.server.address() as AddressInfo).port}`;
  return fake;
}

interface Loaded {
  tools: Map<string, PluginTool>;
  recall?: (event: { prompt?: unknown; currentUserMessage?: unknown }) => Promise<PromptAddition | undefined>;
  warnings: string[];
}

function load(config: Record<string, unknown>, now: () => number = Date.now): Loaded {
  const loaded: Loaded = { tools: new Map(), warnings: [] };
  const api: PluginApi = {
    pluginConfig: config,
    logger: { info: () => {}, warn: (m) => loaded.warnings.push(m) },
    registerTool: (tool, options) => {
      expect(options?.optional).toBe(false);
      loaded.tools.set(tool.name, tool);
    },
    on: (hook, handler) => {
      expect(hook).toBe("before_prompt_build");
      loaded.recall = handler;
    },
  };
  registerFrood(api, now);
  return loaded;
}

const run = (loaded: Loaded, tool: string, params: Record<string, unknown>) =>
  loaded.tools.get(tool)!.execute("call-1", params);

describe("Frood as OpenClaw's memory", () => {
  let frood: Fake;
  beforeEach(async () => {
    frood = await fakeFrood();
  });
  afterEach(() => {
    frood.server.close();
  });

  it("registers the three memory tools the manifest lists and a recall hook", async () => {
    const manifest = JSON.parse(await import("node:fs").then((fs) => fs.readFileSync("openclaw.plugin.json", "utf8")));
    const loaded = load({ url: frood.url, token: frood.token });
    expect([...loaded.tools.keys()]).toEqual(manifest.contracts.tools);
    expect(manifest.kind).toBe("memory");
    expect(loaded.recall).toBeTypeOf("function");
    for (const tool of loaded.tools.values()) {
      expect((tool.parameters as { type: string }).type).toBe("object");
    }
  });

  it("puts what Frood knows into the prompt, marking what no person checked", async () => {
    frood.knowledge = [
      { assertion_id: "a1", kind: "commitment", title: null, text: "Release 2.4 on Nov 14" },
      { assertion_id: "a2", kind: "fact", title: "Hosting", text: "The site runs on Hetzner", unverified: true },
    ];
    const loaded = load({ url: frood.url, token: frood.token, recallLimit: 4 });
    const added = await loaded.recall!({ prompt: "when is the release, and is it on Hetzner?" });
    expect(added?.prependContext).toBe(
      "<frood-memory>\nWhat Frood knows:\n" +
        "- Release 2.4 on Nov 14 (commitment) [frood:a1]\n" +
        "- Hosting: The site runs on Hetzner (fact, not verified) [frood:a2]\n" +
        "</frood-memory>",
    );
    expect(frood.searches[0]).toMatchObject({ limit: "4", status: "active" });
    // The person's own request is what Frood is asked about, not the whole prompt.
    await loaded.recall!({ prompt: "a long system prompt about releases", currentUserMessage: "where is it hosted? Hetzner?" });
    expect(frood.searches[1].q).toBe("where is it hosted? Hetzner?");
    frood.searches.pop();
    expect(await loaded.recall!({ prompt: "nothing matches this" })).toBeUndefined();
    expect(await loaded.recall!({ prompt: "hi" })).toBeUndefined();
    expect(frood.searches).toHaveLength(2);
  });

  it("searches, reads one piece by its mark and remembers as OpenClaw memory", async () => {
    frood.knowledge = [{ assertion_id: "a1", kind: "decision", text: "Search on OpenSearch", evidence: ["e1"] }];
    const loaded = load({ url: frood.url, token: frood.token });
    const found = await run(loaded, "memory_search", { query: "opensearch", limit: 99 });
    expect(found.content[0].text).toContain("- Search on OpenSearch (decision) [frood:a1]");
    expect(found.details).toMatchObject({ count: 1, results: [{ id: "a1", sources: ["e1"], notVerified: false }] });
    expect(frood.searches[0].limit).toBe("20");

    const one = await run(loaded, "memory_get", { id: "[frood:a1]" });
    expect(one.content[0].text).toContain("[frood:a1]");
    const missing = await run(loaded, "memory_get", { id: "zz" });
    expect(missing.content[0].text).toBe("Memory get failed: assertion zz not found");

    const saved = await run(loaded, "memory_add", { content: "Olya leads the release" });
    expect(saved.details).toEqual({ saved: true, source: "src-1" });
    expect(frood.notes).toEqual([
      { kind: "note", origin: "capture", content: "Olya leads the release", metadata: { imported_from: "OpenClaw" } },
    ]);
    expect((await run(loaded, "memory_add", { content: "  " })).details).toEqual({ error: "content is required" });
  });

  it("says a wrong token plainly and turns go on without Frood when it is down", async () => {
    let clock = 1_000;
    const loaded = load({ url: frood.url, token: "another" }, () => clock);
    const refused = await run(loaded, "memory_search", { query: "x" });
    expect(refused.content[0].text).toBe("Memory search failed: the token is not known");

    frood.down = true;
    for (const prompt of ["question one", "question two"]) {
      expect(await loaded.recall!({ prompt })).toBeUndefined();
    }
    const asked = frood.searches.length;
    expect(await loaded.recall!({ prompt: "question three" })).toBeUndefined();
    expect(frood.searches.length).toBe(asked);
    clock += 121_000;
    frood.down = false;
    frood.token = "another";
    frood.knowledge = [{ assertion_id: "a1", kind: "fact", text: "question answered" }];
    expect((await loaded.recall!({ prompt: "question four" }))?.prependContext).toContain("[frood:a1]");
  });

  it("reads its config, with Frood on this computer by default", () => {
    expect(readConfig({ token: "t" }, {})).toEqual({
      url: "http://127.0.0.1:8080",
      token: "t",
      recall: true,
      recallLimit: 6,
    });
    expect(readConfig({ recall: false, recallLimit: 50 }, { FROOD_TOKEN: "e", FROOD_URL: "https://m" })).toEqual({
      url: "https://m",
      token: "e",
      recall: false,
      recallLimit: 20,
    });
    const warned = load({ url: frood.url });
    expect(warned.warnings[0]).toContain("no token");
  });
});
