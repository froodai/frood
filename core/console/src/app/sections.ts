import { BookOpen, Inbox, LayoutDashboard, Plug, Server, Zap, type LucideIcon } from "lucide-react";
import type { Key } from "@/i18n/en";

export type Section = { path: string; icon: LucideIcon; title: Key; sub: Key };

export const SECTIONS: Section[] = [
  { path: "now", icon: LayoutDashboard, title: "nav.now", sub: "nav.now.sub" },
  { path: "inbox", icon: Inbox, title: "nav.inbox", sub: "nav.inbox.sub" },
  { path: "knowledge", icon: BookOpen, title: "nav.knowledge", sub: "nav.knowledge.sub" },
  { path: "sources", icon: Plug, title: "nav.sources", sub: "nav.sources.sub" },
  { path: "automations", icon: Zap, title: "nav.automations", sub: "nav.automations.sub" },
  { path: "system", icon: Server, title: "nav.system", sub: "nav.system.sub" },
];
