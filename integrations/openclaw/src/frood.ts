// The few Frood calls the plugin makes, and how knowledge reads in a prompt.

export const DEFAULT_URL = "http://127.0.0.1:8080";

/** A piece of knowledge as Frood's search returns it (the fields the plugin uses). */
export interface Hit {
  assertion_id: string;
  kind?: string;
  title?: string | null;
  text?: string;
  status?: string;
  valid_from?: string;
  evidence?: string[];
  unverified?: boolean;
}

export class FroodError extends Error {}

export class Frood {
  readonly url: string;

  constructor(
    url: string,
    private readonly token: string,
    private readonly timeoutMs = 10_000,
  ) {
    this.url = url.replace(/\/+$/, "");
  }

  private async call(method: string, path: string, body?: unknown, timeoutMs?: number): Promise<unknown> {
    let response: Response;
    try {
      response = await fetch(this.url + path, {
        method,
        headers: {
          Authorization: `Bearer ${this.token}`,
          "Content-Type": "application/json",
          Accept: "application/json",
        },
        body: body === undefined ? undefined : JSON.stringify(body),
        signal: AbortSignal.timeout(timeoutMs ?? this.timeoutMs),
      });
    } catch (err) {
      throw new FroodError(`Frood at ${this.url} is not reachable: ${String(err)}`);
    }
    const text = await response.text();
    let parsed: unknown = null;
    try {
      parsed = text ? JSON.parse(text) : null;
    } catch {
      parsed = null;
    }
    if (!response.ok) {
      const detail = (parsed as { detail?: string } | null)?.detail;
      throw new FroodError(detail ?? `Frood answered ${response.status}`);
    }
    return parsed;
  }

  async search(query: string, limit: number, timeoutMs?: number): Promise<Hit[]> {
    const params = new URLSearchParams({ q: query, limit: String(limit), status: "active" });
    const hits = await this.call("GET", `/v1/search?${params}`, undefined, timeoutMs);
    return Array.isArray(hits) ? (hits as Hit[]) : [];
  }

  async get(id: string): Promise<Hit & { flags?: unknown[] }> {
    return (await this.call("GET", `/v1/assertions/${encodeURIComponent(id)}`)) as Hit;
  }

  /** A note marked as coming from the assistant's memory: Frood takes it as not verified. */
  async remember(content: string, origin = "OpenClaw"): Promise<{ source?: { id?: string } }> {
    return (await this.call("POST", "/v1/sources", {
      kind: "note",
      origin: "capture",
      content,
      metadata: { imported_from: origin },
    })) as { source?: { id?: string } };
  }
}

/** One piece of knowledge as the model reads it: the text, its kind, the mark, the id. */
export function line(hit: Hit): string {
  const marks = [hit.kind ?? "knowledge"];
  if (hit.unverified) marks.push("not verified");
  const text = hit.text ?? "";
  const shown = hit.title && !text.includes(hit.title) ? `${hit.title}: ${text}` : text;
  return `- ${shown} (${marks.join(", ")}) [frood:${hit.assertion_id}]`;
}

export function recallBlock(hits: Hit[]): string {
  return hits.length ? `What Frood knows:\n${hits.map(line).join("\n")}` : "";
}
