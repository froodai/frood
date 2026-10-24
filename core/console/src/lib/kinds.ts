// How a kind of knowledge looks: one colour and sign per kind, the same in every section.

import { Activity, CalendarCheck, CalendarDays, CircleHelp, Gavel, Info, type LucideIcon, Scale, UserCheck } from "lucide-react";
import type { Key } from "@/i18n/en";

export const KINDS = ["decision", "commitment", "rule", "responsibility", "fact", "status", "event", "open_question"] as const;

export const KIND_TONE: Record<string, { badge: string; dot: string; icon: LucideIcon }> = {
  decision: { badge: "bg-violet-500/15 text-violet-700 dark:text-violet-300", dot: "bg-violet-500", icon: Gavel },
  commitment: { badge: "bg-blue-500/15 text-blue-700 dark:text-blue-300", dot: "bg-blue-500", icon: CalendarCheck },
  rule: { badge: "bg-amber-500/20 text-amber-800 dark:text-amber-300", dot: "bg-amber-500", icon: Scale },
  responsibility: { badge: "bg-teal-500/15 text-teal-700 dark:text-teal-300", dot: "bg-teal-500", icon: UserCheck },
  fact: { badge: "bg-emerald-500/15 text-emerald-700 dark:text-emerald-300", dot: "bg-emerald-500", icon: Info },
  status: { badge: "bg-cyan-500/15 text-cyan-700 dark:text-cyan-300", dot: "bg-cyan-500", icon: Activity },
  event: { badge: "bg-pink-500/15 text-pink-700 dark:text-pink-300", dot: "bg-pink-500", icon: CalendarDays },
  open_question: { badge: "bg-yellow-400/25 text-yellow-800 dark:text-yellow-200", dot: "bg-yellow-500", icon: CircleHelp },
};

const OTHER = { badge: "bg-secondary text-secondary-foreground", dot: "bg-muted-foreground/40", icon: Info };

export const kindTone = (kind: string) => KIND_TONE[kind] ?? OTHER;
export const kindKey = (kind: string) => `kind.${kind}` as Key;
