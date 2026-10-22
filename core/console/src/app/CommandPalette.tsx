import { useQuery } from "@tanstack/react-query";
import { DatabaseBackup, FileSearch, Languages, ShieldCheck, SunMoon } from "lucide-react";
import { useTheme } from "next-themes";
import { useEffect, useState } from "react";
import { useNavigate } from "react-router";
import { toast } from "sonner";
import { CommandDialog, CommandEmpty, CommandGroup, CommandInput, CommandItem, CommandList } from "@/components/ui/command";
import { LANGUAGES, useI18n } from "@/i18n";
import { api } from "@/lib/frood";
import { useStartJob } from "@/lib/queries";
import type { SearchHit } from "@/lib/types";
import { SECTIONS } from "./sections";

function useDebounced<T>(value: T, ms: number): T {
  const [debounced, setDebounced] = useState(value);
  useEffect(() => {
    const timer = setTimeout(() => setDebounced(value), ms);
    return () => clearTimeout(timer);
  }, [value, ms]);
  return debounced;
}

export function CommandPalette({ open, onOpenChange }: { open: boolean; onOpenChange: (open: boolean) => void }) {
  const { t, setLang } = useI18n();
  const { resolvedTheme, setTheme } = useTheme();
  const navigate = useNavigate();
  const start = useStartJob();
  const [query, setQuery] = useState("");
  const q = useDebounced(query.trim(), 250);
  const hits = useQuery({
    queryKey: ["search", q],
    queryFn: () => api<SearchHit[]>(`/v1/search?limit=6&q=${encodeURIComponent(q)}`),
    enabled: open && q.length >= 2,
  });

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key.toLowerCase() === "k" && (e.metaKey || e.ctrlKey)) {
        e.preventDefault();
        onOpenChange(!open);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [open, onOpenChange]);

  const run = (action: () => void) => {
    onOpenChange(false);
    setQuery("");
    action();
  };
  const job = (name: string) =>
    run(() =>
      start.mutate(name, {
        onSuccess: () => toast.success(t("system.started")),
        onError: (err) => toast.error(t("inbox.failed", { error: err.message })),
      }),
    );

  return (
    <CommandDialog open={open} onOpenChange={onOpenChange} title={t("app.search")} description={t("palette.placeholder")}>
      <CommandInput placeholder={t("palette.placeholder")} value={query} onValueChange={setQuery} />
      <CommandList>
        <CommandEmpty>{t("palette.empty")}</CommandEmpty>
        {hits.data && hits.data.length > 0 && (
          <CommandGroup heading={t("palette.memory")}>
            {hits.data.map((hit) => (
              <CommandItem
                key={hit.assertion_id}
                value={`${query} ${hit.text} ${hit.assertion_id}`}
                onSelect={() => run(() => navigate(`knowledge/${hit.assertion_id}`))}
              >
                <FileSearch />
                <span className="line-clamp-1">{hit.text}</span>
              </CommandItem>
            ))}
          </CommandGroup>
        )}
        <CommandGroup heading={t("palette.sections")}>
          {SECTIONS.map(({ path, icon: Icon, title }) => (
            <CommandItem key={path} value={`section ${t(title)} ${path}`} onSelect={() => run(() => navigate(path))}>
              <Icon />
              {t(title)}
            </CommandItem>
          ))}
        </CommandGroup>
        <CommandGroup heading={t("palette.actions")}>
          <CommandItem value={t("palette.backupNow")} onSelect={() => job("backup")}>
            <DatabaseBackup />
            {t("palette.backupNow")}
          </CommandItem>
          <CommandItem value={t("palette.verifyBackup")} onSelect={() => job("backup-verify")}>
            <ShieldCheck />
            {t("palette.verifyBackup")}
          </CommandItem>
          <CommandItem value={t("palette.toggleTheme")} onSelect={() => run(() => setTheme(resolvedTheme === "dark" ? "light" : "dark"))}>
            <SunMoon />
            {t("palette.toggleTheme")}
          </CommandItem>
          {LANGUAGES.map((l) => (
            <CommandItem key={l.code} value={`${t("app.language")} ${l.name} ${l.code}`} onSelect={() => run(() => setLang(l.code))}>
              <Languages />
              <span lang={l.code}>{l.name}</span>
            </CommandItem>
          ))}
        </CommandGroup>
      </CommandList>
    </CommandDialog>
  );
}
