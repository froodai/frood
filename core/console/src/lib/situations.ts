// How an Inbox item reads at a glance: its type with a colour, where it comes from, what kind of
// knowledge it touches. One place, so the list, the filters and the details agree.

import type { Key } from "@/i18n/en";
import type { InboxEntry, ProposalContent } from "./types";

export type Type = "new" | "change" | "conflict" | "stale" | "merge" | "question" | "reminder" | "alert";
export type Origin = "agent" | "extract" | "person" | "other";

export const TYPES: Type[] = ["new", "change", "conflict", "stale", "merge", "question", "reminder", "alert"];

/**
 * A type's badge: what happened to the knowledge. New knowledge is the common case and stays
 * quiet; what needs a closer look stands out. Each readable in both themes.
 */
export const TONE: Record<Type, { badge: string; dot: string }> = {
  new: { badge: "border text-muted-foreground", dot: "bg-muted-foreground/40" },
  change: { badge: "border border-sky-500/50 text-sky-700 dark:text-sky-300", dot: "bg-sky-500" },
  conflict: { badge: "bg-rose-500/15 text-rose-700 dark:text-rose-300", dot: "bg-rose-500" },
  stale: { badge: "border border-zinc-400/60 text-zinc-600 dark:text-zinc-300", dot: "bg-zinc-400" },
  merge: { badge: "border border-violet-500/50 text-violet-700 dark:text-violet-300", dot: "bg-violet-500" },
  question: { badge: "bg-fuchsia-500/12 text-fuchsia-700 dark:text-fuchsia-300", dot: "bg-fuchsia-500" },
  reminder: { badge: "bg-orange-500/12 text-orange-700 dark:text-orange-300", dot: "bg-orange-500" },
  alert: { badge: "bg-red-500/12 text-red-700 dark:text-red-300", dot: "bg-red-500" },
};

export const typeKey = (type: Type) => `type.${type}` as Key;

export function proposal(entry: InboxEntry): ProposalContent | null {
  return entry.item.kind === "proposal" ? (entry.content as ProposalContent) : null;
}

export function typeOf(entry: InboxEntry): Type {
  const p = proposal(entry);
  if (!p) {
    const kind = entry.item.kind;
    return kind === "question" || kind === "reminder" ? kind : "alert";
  }
  switch (p.risk) {
    case "conflict":
      return "conflict";
    case "change":
      return "change";
    case "stale_knowledge":
      return "stale";
    case "entity_merge":
      return "merge";
    default:
      return "new";
  }
}

/** Who brought it: an agent's pass over a source, extraction from a note, a person. */
export function originOf(entry: InboxEntry): { origin: Origin; job: string | null } {
  const author = proposal(entry)?.author ?? (entry.item.data.author as string | undefined) ?? "";
  if (author === "agent:extract" || author.startsWith("agent:extract/")) return { origin: "extract", job: null };
  if (author.startsWith("agent:")) return { origin: "agent", job: author.slice("agent:".length).split("/")[0] || null };
  if (author.startsWith("user:")) return { origin: "person", job: null };
  return { origin: "other", job: null };
}

/** The kind of knowledge a proposal touches (decision, commitment, …), from its first change. */
export function kindOf(entry: InboxEntry): string | null {
  return proposal(entry)?.alternatives[0]?.changes[0]?.kind || null;
}

/** The line a person reads first, and the text under it. */
export function summary(entry: InboxEntry): Titled {
  const change = proposal(entry)?.alternatives[0]?.changes[0];
  if (change) return titled(change.title, change.after ?? change.before ?? "");
  const content = entry.content as Record<string, unknown>;
  const data = entry.item.data;
  for (const value of [content.text, data.text, data.title, data.message, data.error]) {
    if (typeof value === "string" && value) return titled(null, value);
  }
  return { title: entry.item.subject, description: null };
}

export const headline = (entry: InboxEntry) => summary(entry).title;

export type Titled = { title: string; description: string | null };

const TITLE_CHARS = 80;

/**
 * A title and the text under it. Knowledge recorded before titles existed gets one from the
 * start of its text — its first sentence or clause, cut at a word when it runs long — and the
 * rest of the text goes under it, so nothing is said twice.
 */
export function titled(title: string | null | undefined, text: string): Titled {
  const body = text.trim();
  if (title?.trim()) return { title: title.trim(), description: body && body !== title.trim() ? body : null };
  const clause = body.match(/^(.{12,}?)(?:[.!?;](?=\s|$)|\s[—–]\s|\n)/su)?.[1] ?? body;
  const cut = clause.length > TITLE_CHARS;
  const head = cut ? clause.slice(0, TITLE_CHARS).replace(/[\s,;:(—–-]+\S*$/u, "") : clause;
  const rest = body.slice(head.length).replace(/^[\s.!?;:,—–-]+/u, "");
  if (!rest) return { title: body, description: null };
  return cut ? { title: `${head}…`, description: `…${rest}` } : { title: head, description: rest };
}
