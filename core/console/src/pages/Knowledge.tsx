import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Search } from "lucide-react";
import { useEffect, useState } from "react";
import { useNavigate, useParams, useSearchParams } from "react-router";
import { toast } from "sonner";
import { Attachments } from "@/components/attachments";
import { KindBadge, SphereBadge, SphereFilter, SpherePicker } from "@/components/badges";
import { Sheet, SheetContent, SheetTitle } from "@/components/ui/sheet";
import { Skeleton } from "@/components/ui/skeleton";
import { useI18n } from "@/i18n";
import { api } from "@/lib/frood";
import { useMediaQuery } from "@/lib/hooks";
import { titled } from "@/lib/situations";
import { useSphereName } from "@/lib/spheres";
import type { Assertion, SearchHit } from "@/lib/types";
import { cn } from "@/lib/utils";

export function Knowledge() {
  const { t, date } = useI18n();
  const { id } = useParams();
  const navigate = useNavigate();
  const wide = useMediaQuery("(min-width: 1024px)");
  const [params, setParams] = useSearchParams();
  const [text, setText] = useState(params.get("q") ?? "");
  const q = params.get("q") ?? "";
  const sphere = params.get("sphere") ?? "all";
  const filter = (next: { q?: string; sphere?: string }) => {
    const merged = { q, sphere, ...next };
    setParams({ ...(merged.q ? { q: merged.q } : {}), ...(merged.sphere !== "all" ? { sphere: merged.sphere } : {}) }, { replace: true });
  };

  useEffect(() => {
    const timer = setTimeout(() => text.trim() !== q && filter({ q: text.trim() }), 300);
    return () => clearTimeout(timer);
  }, [text]);

  const list = useQuery({
    queryKey: ["knowledge", q, sphere],
    queryFn: async (): Promise<Assertion[]> => {
      if (q) {
        // Search ranks by meaning; the sphere narrows what it found.
        const hits = await api<SearchHit[]>(`/v1/search?limit=50&q=${encodeURIComponent(q)}`);
        return sphere === "all" ? hits : hits.filter((h) => (h.sphere ?? "none") === sphere);
      }
      const all = await api<Assertion[]>(`/v1/assertions?limit=200${sphere !== "all" ? `&sphere=${sphere}` : ""}`);
      return all.sort((a, b) => b.recorded_from.localeCompare(a.recorded_from));
    },
  });

  const open = (assertion: Assertion | undefined) =>
    navigate({ pathname: assertion ? `../knowledge/${assertion.assertion_id}` : "../knowledge", search: params.toString() }, { replace: true });
  const detail = id && <History id={id} />;

  return (
    <div className="flex min-h-0 flex-1">
      <section className="flex min-w-0 flex-1 flex-col gap-3.5 px-4 py-5 md:px-6">
        <div className="flex flex-wrap items-center gap-2">
          <label className="flex min-h-10 min-w-48 flex-1 items-center gap-2.5 rounded-lg border bg-card px-3 focus-within:ring-2 focus-within:ring-ring/40">
            <Search className="size-4 text-muted-foreground" aria-hidden />
            <input
              type="search"
              value={text}
              onChange={(e) => setText(e.target.value)}
              placeholder={t("knowledge.search")}
              aria-label={t("knowledge.search")}
              className="min-w-0 flex-1 bg-transparent text-sm outline-none"
            />
          </label>
          <SphereFilter value={sphere} onChange={(s) => filter({ sphere: s })} />
        </div>
        <span className="text-xs text-muted-foreground">
          {q ? t("knowledge.results", { n: list.data?.length ?? 0 }) : t("knowledge.recent")}
        </span>
        {list.isPending ? (
          <div className="flex flex-col gap-2">
            {[0, 1, 2, 3, 4].map((i) => (
              <Skeleton key={i} className="h-11" />
            ))}
          </div>
        ) : list.data && list.data.length === 0 ? (
          <p className="text-sm text-muted-foreground">{q ? t("knowledge.empty", { q }) : t("now.nothing")}</p>
        ) : (
          <div className="overflow-hidden rounded-xl border bg-card">
            <div className="hidden grid-cols-[minmax(0,1fr)_9rem_6rem] gap-3 border-b bg-muted/50 px-3.5 py-2 text-xs text-muted-foreground sm:grid">
              <span>{t("knowledge.col.what")}</span>
              <span>{t("knowledge.col.kind")}</span>
              <span>{t("knowledge.col.since")}</span>
            </div>
            {list.data?.map((a) => {
              const { title, description } = titled(a.title, a.text);
              return (
              <button
                key={a.assertion_id}
                type="button"
                onClick={() => open(a)}
                className={cn(
                  "grid w-full grid-cols-1 gap-1 border-b px-3.5 py-2.5 text-start text-sm last:border-b-0 hover:bg-accent/50 sm:grid-cols-[minmax(0,1fr)_9rem_6rem] sm:items-baseline sm:gap-3",
                  a.assertion_id === id && "bg-accent",
                )}
              >
                <span className="flex min-w-0 flex-col gap-0.5">
                  <span className="line-clamp-1">{title}</span>
                  {description && <span className="line-clamp-1 text-[13px] text-muted-foreground">{description}</span>}
                </span>
                <span className="flex flex-wrap items-center gap-1 sm:flex-col sm:items-start">
                  <KindBadge kind={a.kind} />
                  <SphereBadge sphere={a.sphere} />
                </span>
                <span className="font-mono text-xs text-muted-foreground">{date(a.valid_from)}</span>
              </button>
              );
            })}
          </div>
        )}
      </section>
      {wide ? (
        <aside className="sticky top-14 flex max-h-[calc(100dvh-3.5rem)] w-[clamp(320px,36%,480px)] shrink-0 flex-col overflow-y-auto border-s bg-card px-6 py-5">
          {detail || <p className="text-sm text-muted-foreground">{t("knowledge.pick")}</p>}
        </aside>
      ) : (
        <Sheet open={!!id} onOpenChange={(o) => !o && open(undefined)}>
          <SheetContent side="bottom" className="max-h-[85dvh] overflow-y-auto p-5">
            <SheetTitle className="sr-only">{t("knowledge.history")}</SheetTitle>
            {detail}
          </SheetContent>
        </Sheet>
      )}
    </div>
  );
}

/** What held when (current knowledge by valid time), then what was corrected in the record. */
function History({ id }: { id: string }) {
  const { t, date, ago } = useI18n();
  const history = useQuery({ queryKey: ["history", id], queryFn: () => api<Assertion[]>(`/v1/assertions/${id}/history`) });
  const move = useMove(id);
  if (history.isPending) return <Skeleton className="h-40" />;
  if (history.isError) return <p className="text-sm text-destructive">{history.error.message}</p>;
  const held = history.data.filter((v) => !v.recorded_to).sort((a, b) => b.valid_from.localeCompare(a.valid_from));
  const corrected = history.data
    .filter((v) => v.recorded_to && !held.some((h) => h.text === v.text))
    .sort((a, b) => b.recorded_to!.localeCompare(a.recorded_to!));
  const head = held[0] ?? history.data[history.data.length - 1];
  const entries = [
    ...held.map((v) => ({ v, live: !v.valid_to && v.status === "active", corrected: false })),
    ...corrected.map((v) => ({ v, live: false, corrected: true })),
  ];
  const { title } = titled(head.title, head.text);
  const full = head.text.trim();
  const span = (v: Assertion) => (v.valid_to ? `${date(v.valid_from, "long")} – ${date(v.valid_to, "long")}` : date(v.valid_from, "long"));
  return (
    <div className="flex flex-col gap-4">
      <div className="flex flex-col gap-1.5">
        <div className="flex flex-wrap items-center gap-1.5">
          <KindBadge kind={head.kind} />
          {head.recorded_to ? (
            <SphereBadge sphere={head.sphere} />
          ) : (
            <SpherePicker value={move.isPending ? (move.variables ?? null) : (head.sphere ?? null)} onChange={(s) => move.mutate(s)} disabled={move.isPending} />
          )}
        </div>
        <h2 className="text-[15px] leading-snug font-semibold">{title}</h2>
        {full !== title && <p className="text-sm leading-relaxed text-foreground/85">{full}</p>}
      </div>
      <Attachments target={`assertions/${id}`} />
      <span className="text-xs text-muted-foreground">{t("knowledge.history")}</span>
      <ol className="flex flex-col">
        {entries.map(({ v, live, corrected }, i) => (
          <li key={v.version} className="flex gap-3">
            <span className="flex w-2.5 flex-col items-center">
              <span className={cn("mt-1.5 size-2.5 rounded-full", live ? "bg-primary" : "bg-border")} />
              {i < entries.length - 1 && <span className="w-px flex-1 bg-border" />}
            </span>
            <span className="flex flex-col gap-0.5 pb-4">
              <span className={cn("text-sm", live && "font-medium", corrected && "text-muted-foreground line-through")}>{v.text}</span>
              <span className="text-xs text-muted-foreground">
                {corrected
                  ? t("knowledge.correctedOn", { when: ago(v.recorded_to) })
                  : `${live ? t("knowledge.current") : v.status === "active" || v.status === "superseded" ? t("knowledge.was") : t("knowledge.closed")} · ${span(v)} · ${t("knowledge.recorded", { when: ago(v.recorded_from) })}`}
              </span>
            </span>
          </li>
        ))}
      </ol>
    </div>
  );
}

/** Moves knowledge to another sphere: a person's own move is accepted at once. */
function useMove(id: string) {
  const { t } = useI18n();
  const name = useSphereName();
  const client = useQueryClient();
  return useMutation({
    mutationFn: (sphere: string | null) => api(`/v1/assertions/${id}/sphere`, { method: "PUT", json: { sphere } }),
    onSuccess: (_proposal, sphere) => {
      toast(t("sphere.moved", { sphere: sphere ? name(sphere) : t("sphere.none") }));
      for (const key of [["history", id], ["knowledge"], ["now"]]) client.invalidateQueries({ queryKey: key });
    },
    onError: (err: Error) => toast.error(t("inbox.failed", { error: err.message })),
  });
}
