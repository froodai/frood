// How an Inbox item reads at a glance: its type with a colour, where it comes from, what kind of
// knowledge it touches. One place, so the list, the filters and the details agree.

import type { Key } from "@/i18n/en";
import type { InboxEntry, ProposalContent } from "./types";

export type Type = "new" | "change" | "conflict" | "stale" | "merge" | "question" | "reminder" | "alert";
export type Origin = "agent" | "extract" | "person" | "other";

export const TYPES: Type[] = ["new", "change", "conflict", "stale", "merge", "question", "reminder", "alert"];

/** Text and background of a type's badge; each readable in both themes. */
export const TONE: Record<Type, { badge: string; dot: string }> = {
  new: { badge: "bg-emerald-500/12 text-emerald-700 dark:text-emerald-300", dot: "bg-emerald-500" },
  change: { badge: "bg-sky-500/12 text-sky-700 dark:text-sky-300", dot: "bg-sky-500" },
  conflict: { badge: "bg-amber-500/15 text-amber-800 dark:text-amber-300", dot: "bg-amber-500" },
  stale: { badge: "bg-zinc-500/15 text-zinc-700 dark:text-zinc-300", dot: "bg-zinc-400" },
  merge: { badge: "bg-violet-500/12 text-violet-700 dark:text-violet-300", dot: "bg-violet-500" },
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

/** The line a person reads first. */
export function headline(entry: InboxEntry): string {
  const change = proposal(entry)?.alternatives[0]?.changes[0];
  if (change) return change.after ?? change.before ?? "";
  const content = entry.content as Record<string, unknown>;
  const data = entry.item.data;
  for (const value of [content.text, data.text, data.title, data.message, data.error]) {
    if (typeof value === "string" && value) return value;
  }
  return entry.item.subject;
}
