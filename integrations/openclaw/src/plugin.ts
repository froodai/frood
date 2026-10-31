// Frood as OpenClaw's memory (docs/28 §3): what Frood knows about the message goes into the
// prompt before each turn, and the memory tools work over Frood. What the agent saves with
// memory_add reaches Frood as a note marked "from OpenClaw memory, not verified"; where it
// contradicts what is known, the person decides. Whole conversations are never written.

import { type TSchema, Type } from "typebox";

import { DEFAULT_URL, Frood, FroodError, type Hit, recallBlock } from "./frood.js";

/** The part of OpenClaw's plugin API the plugin uses. */
export interface PluginApi {
  pluginConfig?: Record<string, unknown>;
  logger: { info(message: string): void; warn(message: string): void };
  registerTool(tool: PluginTool, options?: { optional?: boolean }): void;
  on(
    hook: "before_prompt_build",
    handler: (event: { prompt?: unknown; currentUserMessage?: unknown }) => Promise<PromptAddition | undefined>,
  ): void;
}

export interface PluginTool {
  name: string;
  label: string;
  description: string;
  parameters: TSchema;
  execute(toolCallId: string, params: Record<string, unknown>): Promise<ToolResult>;
}

export interface ToolResult {
  content: { type: "text"; text: string }[];
  details: unknown;
}

export interface PromptAddition {
  prependContext: string;
}

export interface FroodConfig {
  url: string;
  token: string;
  recall: boolean;
  recallLimit: number;
}

/** Recall waits this long; a slow Frood must not hold the turn. */
export const RECALL_TIMEOUT_MS = 3_000;
/** After this many failures in a row Frood is left alone for a while. */
export const BREAKER_THRESHOLD = 3;
export const BREAKER_COOLDOWN_MS = 120_000;

export function readConfig(raw: Record<string, unknown> | undefined, env = process.env): FroodConfig {
  const config = raw ?? {};
  const limit = Number(config.recallLimit ?? 6);
  return {
    url: String(config.url ?? env.FROOD_URL ?? DEFAULT_URL),
    token: String(config.token ?? env.FROOD_TOKEN ?? ""),
    recall: config.recall !== false,
    recallLimit: Number.isInteger(limit) ? Math.min(Math.max(limit, 1), 20) : 6,
  };
}

const text = (value: string, details: unknown = null): ToolResult => ({
  content: [{ type: "text", text: value }],
  details,
});

export function registerFrood(api: PluginApi, now: () => number = Date.now): void {
  const config = readConfig(api.pluginConfig);
  if (!config.token) {
    api.logger.warn("frood: no token; set plugins.entries.frood.config.token (frood token create)");
  }
  const frood = new Frood(config.url, config.token);
  let failures = 0;
  let pausedUntil = 0;
  const ok = () => {
    failures = 0;
  };
  const failed = (err: unknown) => {
    failures += 1;
    api.logger.warn(`frood: ${err instanceof Error ? err.message : String(err)}`);
    if (failures >= BREAKER_THRESHOLD) {
      pausedUntil = now() + BREAKER_COOLDOWN_MS;
      failures = 0;
    }
  };
  const errorResult = (what: string, err: unknown) =>
    text(`${what} failed: ${err instanceof Error ? err.message : String(err)}`, { error: String(err) });

  api.registerTool(
    {
      name: "memory_search",
      label: "Frood: search",
      description:
        "Search Frood, the person's memory: decisions, commitments, rules, facts and statuses with their " +
        "sources. Items marked 'not verified' came from an assistant's memory or an agent and no person " +
        "has checked them.",
      parameters: Type.Object({
        query: Type.String({ description: "What to look for, in any language." }),
        limit: Type.Optional(Type.Integer({ minimum: 1, maximum: 20, description: "How many results (default 8)." })),
      }),
      async execute(_toolCallId, params) {
        const query = String(params.query ?? "").trim();
        if (!query) return text("Give a query.", { error: "query is required" });
        const limit = Math.min(Math.max(Number(params.limit ?? 8) || 8, 1), 20);
        try {
          const hits = await frood.search(query, limit);
          ok();
          return text(hits.length ? recallBlock(hits) : "Frood knows nothing about it yet.", {
            count: hits.length,
            results: hits.map(summary),
          });
        } catch (err) {
          failed(err);
          return errorResult("Memory search", err);
        }
      },
    },
    { optional: false },
  );

  api.registerTool(
    {
      name: "memory_get",
      label: "Frood: one piece of knowledge",
      description: "Read one piece of knowledge from Frood by its id, the [frood:…] mark next to it.",
      parameters: Type.Object({
        id: Type.String({ description: "The id from [frood:…]." }),
      }),
      async execute(_toolCallId, params) {
        const id = String(params.id ?? "")
          .replace(/^\[?frood:/, "")
          .replace(/\]$/, "")
          .trim();
        if (!id) return text("Give the id.", { error: "id is required" });
        try {
          const hit = await frood.get(id);
          ok();
          return text(recallBlock([hit]), summary(hit));
        } catch (err) {
          failed(err);
          return errorResult("Memory get", err);
        }
      },
    },
    { optional: false },
  );

  api.registerTool(
    {
      name: "memory_add",
      label: "Frood: remember",
      description:
        "Remember something in Frood: a short note in the person's words. Frood reads it into knowledge " +
        "marked 'from OpenClaw memory, not verified'; where it contradicts what is known, the person decides.",
      parameters: Type.Object({
        content: Type.String({ description: "What to remember, as a short note." }),
      }),
      async execute(_toolCallId, params) {
        const content = String(params.content ?? "").trim();
        if (!content) return text("Give what to remember.", { error: "content is required" });
        try {
          const saved = await frood.remember(content);
          ok();
          return text("Saved to Frood; it becomes knowledge marked 'not verified'.", {
            saved: true,
            source: saved.source?.id,
          });
        } catch (err) {
          failed(err);
          return errorResult("Memory add", err);
        }
      },
    },
    { optional: false },
  );

  if (!config.recall) return;
  api.on("before_prompt_build", async (event) => {
    // The person's request; older hosts give only the prompt, of which the end is the request.
    const asked = typeof event?.currentUserMessage === "string" ? event.currentUserMessage.trim() : "";
    const prompt = asked || (typeof event?.prompt === "string" ? event.prompt.trim().slice(-500) : "");
    if (prompt.length < 3 || !config.token || now() < pausedUntil) return undefined;
    try {
      const hits = await frood.search(prompt, config.recallLimit, RECALL_TIMEOUT_MS);
      ok();
      const block = recallBlock(hits);
      return block ? { prependContext: `<frood-memory>\n${block}\n</frood-memory>` } : undefined;
    } catch (err) {
      failed(err);
      return undefined;
    }
  });
}

function summary(hit: Hit) {
  return {
    id: hit.assertion_id,
    kind: hit.kind,
    title: hit.title ?? null,
    text: hit.text,
    notVerified: Boolean(hit.unverified),
    validFrom: hit.valid_from ?? null,
    sources: hit.evidence ?? [],
  };
}

export { FroodError };
