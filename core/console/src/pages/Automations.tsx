import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Play, Settings2 } from "lucide-react";
import { useState } from "react";
import { toast } from "sonner";
import { Button } from "@/components/ui/button";
import { Sheet, SheetContent, SheetTitle } from "@/components/ui/sheet";
import { Skeleton } from "@/components/ui/skeleton";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { useI18n } from "@/i18n";
import type { Key } from "@/i18n/en";
import { en } from "@/i18n/en";
import { api } from "@/lib/frood";
import { useStartJob } from "@/lib/queries";
import type { Run } from "@/lib/types";
import { cn } from "@/lib/utils";
import { JobForm, type JobSetting } from "./JobSettings";
import { Notifications } from "./Notifications";

type Trigger = { event?: string; schedule?: string; timezone?: string; for_each?: string };
type Job = { name: string; uses: string; triggers: Trigger[] };

const TONE: Record<string, string> = {
  succeeded: "bg-emerald-500",
  failed: "bg-red-500",
  dead: "bg-red-500",
  cancelled: "bg-zinc-400",
  running: "bg-sky-500",
  queued: "bg-amber-400",
  waiting_for_input: "bg-fuchsia-500",
};
const PILL: Record<string, string> = {
  succeeded: "bg-emerald-500/12 text-emerald-700 dark:text-emerald-300",
  failed: "bg-red-500/12 text-red-700 dark:text-red-300",
  dead: "bg-red-500/12 text-red-700 dark:text-red-300",
  cancelled: "bg-zinc-500/15 text-zinc-700 dark:text-zinc-300",
  running: "bg-sky-500/12 text-sky-700 dark:text-sky-300",
  queued: "bg-amber-500/15 text-amber-800 dark:text-amber-300",
  waiting_for_input: "bg-fuchsia-500/12 text-fuchsia-700 dark:text-fuchsia-300",
};

/** A job's name for people, when it is one Frood knows; else its own name. */
export function useJobTitle() {
  const { t } = useI18n();
  return (name: string) => (`job.${name}` in en ? t(`job.${name}` as Key) : name);
}

/** The common cron shapes in words ("weekdays at 08:30"); anything else as it is. */
function useCron() {
  const { t } = useI18n();
  return (cron: string) => {
    const [minute, hour, day, month, weekday] = cron.trim().split(/\s+/);
    const at = /^\d+$/.test(minute) && /^\d+$/.test(hour) ? `${hour.padStart(2, "0")}:${minute.padStart(2, "0")}` : null;
    if (day === "*" && month === "*") {
      if (at && weekday === "*") return t("cron.daily", { time: at });
      if (at && weekday === "1-5") return t("cron.weekdays", { time: at });
      const every = /^\*\/(\d+)$/.exec(hour ?? "");
      if (every && /^\d+$/.test(minute) && weekday === "*") return t("cron.everyHours", { n: Number(every[1]) });
    }
    return cron;
  };
}

function useWhen() {
  const { t } = useI18n();
  const cron = useCron();
  return (job: Job) =>
    job.triggers
      .map((tr) => {
        if (tr.schedule) return t("trigger.schedule", { cron: cron(tr.schedule), tz: tr.timezone ?? "UTC" });
        if (tr.event === "source.captured") return t("trigger.onNote");
        if (tr.event?.startsWith("assertion.")) return t("trigger.onKnowledge");
        return tr.event ?? "";
      })
      .filter(Boolean)
      .filter((v, i, all) => all.indexOf(v) === i)
      .join(" · ") || t("trigger.byHand");
}

export function Automations() {
  const { t, ago, lang } = useI18n();
  const title = useJobTitle();
  const when = useWhen();
  const [job, setJob] = useState<string | null>(null);
  const [open, setOpen] = useState<string | null>(null);
  const [editing, setEditing] = useState<string | null>(null);
  const running = useQuery({ queryKey: ["jobs"], queryFn: () => api<Job[]>("/v1/jobs") });
  // Every job, with what the console may change; without admin, only the running ones.
  const settings = useQuery({
    queryKey: ["job-settings"],
    queryFn: () => api<{ jobs: JobSetting[]; accounts: string[] }>("/v1/jobs/settings"),
    retry: false,
  });
  const jobs = {
    isPending: running.isPending,
    data: settings.data
      ? settings.data.jobs.map((s) => ({ name: s.name, uses: s.uses, triggers: running.data?.find((r) => r.name === s.name)?.triggers ?? s.schedules, setting: s }))
      : (running.data ?? []).map((r) => ({ ...r, setting: undefined as JobSetting | undefined })),
  };
  const edited = settings.data?.jobs.find((s) => s.name === editing);
  const runs = useQuery({ queryKey: ["runs", "all"], queryFn: () => api<Run[]>("/v1/runs?limit=200"), refetchInterval: 15_000 });
  const start = useStartJob();
  const client = useQueryClient();
  const control = useMutation({
    mutationFn: ({ run, what }: { run: Run; what: "retry" | "cancel" }) => api(`/v1/runs/${run.id}/${what}`, { method: "POST" }),
    onSuccess: () => client.invalidateQueries({ queryKey: ["runs"] }),
    onError: (err) => toast.error(t("inbox.failed", { error: err.message })),
  });

  const all = runs.data ?? [];
  const shown = job ? all.filter((r) => r.job === job) : all;
  const took = (run: Run) => {
    if (!run.started_at || !run.finished_at) return "";
    const seconds = (new Date(run.finished_at).getTime() - new Date(run.started_at).getTime()) / 1000;
    const [value, unit] = seconds < 90 ? [Math.round(seconds), "second"] : seconds < 5400 ? [Math.round(seconds / 60), "minute"] : [Math.round(seconds / 3600), "hour"];
    return new Intl.NumberFormat(lang, { style: "unit", unit, unitDisplay: "short" }).format(value);
  };
  const cost = (usd: number) => (usd > 0 ? new Intl.NumberFormat("en", { style: "currency", currency: "USD", maximumFractionDigits: 4 }).format(usd) : "");

  return (
    <Tabs defaultValue={new URLSearchParams(location.search).get("tab") === "notifications" ? "notifications" : "jobs"} className="flex flex-col gap-4 px-4 py-5 md:px-7">
      <TabsList>
        <TabsTrigger value="jobs">{t("automations.jobs")}</TabsTrigger>
        <TabsTrigger value="notifications">{t("automations.notifications")}</TabsTrigger>
      </TabsList>
      <TabsContent value="notifications">
        <Notifications />
      </TabsContent>
      <TabsContent value="jobs" className="flex flex-col gap-5">
        {jobs.isPending ? (
          <Skeleton className="h-32" />
        ) : (
          <div className="grid grid-cols-1 gap-3 sm:grid-cols-2 xl:grid-cols-3">
            {(jobs.data ?? []).map((j) => {
              const mine = all.filter((r) => r.job === j.name);
              const last = mine[0];
              const scheduled = j.triggers.some((tr) => tr.schedule) || j.triggers.length === 0;
              const off = j.setting && !j.setting.running;
              return (
                <div
                  key={j.name}
                  className={cn(
                    "flex flex-col gap-2 rounded-xl border bg-card p-4 transition-colors",
                    job === j.name ? "border-primary" : "hover:border-foreground/20",
                    off && "border-dashed",
                  )}
                >
                  <div className="flex items-start gap-2">
                    <button type="button" onClick={() => setJob(job === j.name ? null : j.name)} className="flex min-w-0 flex-1 flex-col gap-1 text-start">
                      <span className={cn("font-medium", off && "text-muted-foreground")}>{title(j.name)}</span>
                      <span className="text-xs text-muted-foreground">{off ? t("jobs.notRunning") : when(j)}</span>
                    </button>
                    {j.setting?.from === "config" && <span className="rounded-md border px-1.5 text-[11px] text-muted-foreground">{t("connect.fromConfig")}</span>}
                    {j.setting?.from === "console" && <span className="rounded-md border px-1.5 text-[11px] text-muted-foreground">{t("jobs.fromConsole")}</span>}
                    {j.setting && (
                      <button type="button" onClick={() => setEditing(j.name)} aria-label={`${t("jobs.configure")}: ${title(j.name)}`} className="text-muted-foreground hover:text-foreground">
                        <Settings2 className="size-4" />
                      </button>
                    )}
                  </div>
                  <div className="flex items-center gap-2 text-xs text-muted-foreground">
                    {last ? (
                      <>
                        <span className={cn("size-2 rounded-full", TONE[last.status] ?? "bg-zinc-400")} />
                        <span>{t(`run.${last.status}` as Key)}</span>
                        <span aria-hidden>·</span>
                        <span>{ago(last.started_at ?? last.created_at)}</span>
                      </>
                    ) : (
                      <span>{t("automations.neverRan")}</span>
                    )}
                  </div>
                  <div className="flex items-center justify-between gap-2">
                    {/* The last runs, oldest first: a row of colours reads at a glance. */}
                    <span className="flex gap-0.5" aria-hidden>
                      {mine
                        .slice(0, 16)
                        .reverse()
                        .map((r) => (
                          <span key={r.id} className={cn("h-4 w-1.5 rounded-sm", TONE[r.status] ?? "bg-zinc-300")} />
                        ))}
                    </span>
                    {scheduled && !off && (
                      <Button
                        size="sm"
                        variant="ghost"
                        className="h-7 px-2 text-xs"
                        disabled={start.isPending}
                        onClick={() =>
                          start.mutate(j.name, {
                            onSuccess: () => toast.success(t("system.started")),
                            onError: (err) => toast.error(t("inbox.failed", { error: err.message })),
                          })
                        }
                      >
                        <Play className="size-3.5" /> {t("automations.runNow")}
                      </Button>
                    )}
                  </div>
                </div>
              );
            })}
          </div>
        )}

        <div className="flex items-center gap-2">
          <h2 className="text-sm font-medium">{job ? t("automations.runsOf", { job: title(job) }) : t("automations.allRuns")}</h2>
          {job && (
            <Button size="sm" variant="ghost" className="h-7 px-2 text-xs" onClick={() => setJob(null)}>
              {t("inbox.filter.reset")}
            </Button>
          )}
        </div>
        {runs.isPending ? (
          <Skeleton className="h-64" />
        ) : shown.length === 0 ? (
          <p className="text-sm text-muted-foreground">{t("automations.none")}</p>
        ) : (
          <div className="overflow-hidden rounded-xl border bg-card">
            {shown.map((run) => (
              <button
                key={run.id}
                type="button"
                onClick={() => setOpen(run.id)}
                className="flex w-full flex-wrap items-center gap-x-3 gap-y-1 border-b px-4 py-2.5 text-start text-sm last:border-b-0 hover:bg-accent/40"
              >
                <span className={cn("rounded-md px-1.5 py-0.5 text-[11px] font-medium", PILL[run.status] ?? PILL.cancelled)}>{t(`run.${run.status}` as Key)}</span>
                <span className="min-w-32 flex-1 font-medium">{title(run.job)}</span>
                {run.error && <span className="line-clamp-1 w-full text-xs text-red-700 sm:order-last dark:text-red-300">{run.error}</span>}
                <span className="text-xs text-muted-foreground">{ago(run.started_at ?? run.created_at)}</span>
                <span className="w-14 text-end text-xs text-muted-foreground">{took(run)}</span>
                <span className="w-16 text-end font-mono text-xs text-muted-foreground">{cost(run.cost_usd)}</span>
              </button>
            ))}
          </div>
        )}

        <Sheet open={!!open} onOpenChange={(o) => !o && setOpen(null)}>
          <SheetContent className="w-full overflow-y-auto sm:max-w-lg">
            <SheetTitle className="sr-only">{t("automations.run")}</SheetTitle>
            {open && <RunDetail id={open} title={title} onControl={(run, what) => control.mutate({ run, what })} />}
          </SheetContent>
        </Sheet>
        <Sheet open={!!edited} onOpenChange={(o) => !o && setEditing(null)}>
          <SheetContent className="w-full overflow-y-auto sm:max-w-lg">
            <SheetTitle className="sr-only">{t("jobs.configure")}</SheetTitle>
            <div className="p-5 text-sm">
              {edited && <JobForm key={edited.name} job={edited} accounts={settings.data?.accounts ?? []} title={title(edited.name)} onDone={() => setEditing(null)} />}
            </div>
          </SheetContent>
        </Sheet>
      </TabsContent>
    </Tabs>
  );
}

type Event = { seq: number; kind: string; at: string; data: unknown };

/** A step's data in a line: its text or message when it has one. */
function summary(data: unknown): string {
  if (typeof data === "string") return data;
  if (data && typeof data === "object") {
    const d = data as Record<string, unknown>;
    for (const key of ["text", "message", "tool", "name", "status"]) if (typeof d[key] === "string") return d[key] as string;
    const json = JSON.stringify(data);
    return json.length > 160 ? `${json.slice(0, 160)}…` : json;
  }
  return "";
}

function RunDetail({ id, title, onControl }: { id: string; title: (name: string) => string; onControl: (run: Run, what: "retry" | "cancel") => void }) {
  const { t, ago } = useI18n();
  const run = useQuery({ queryKey: ["run", id], queryFn: () => api<Run & { input: unknown; output: unknown }>(`/v1/runs/${id}`), refetchInterval: 5000 });
  const events = useQuery({ queryKey: ["run-events", id], queryFn: () => api<Event[]>(`/v1/runs/${id}/events`) });
  if (!run.data) return <Skeleton className="m-6 h-40" />;
  const r = run.data;
  return (
    <div className="flex flex-col gap-4 p-5">
      <div className="flex flex-col gap-1">
        <span className={cn("self-start rounded-md px-1.5 py-0.5 text-[11px] font-medium", PILL[r.status] ?? PILL.cancelled)}>{t(`run.${r.status}` as Key)}</span>
        <h2 className="text-lg font-semibold">{title(r.job)}</h2>
        <span className="text-xs text-muted-foreground">{ago(r.started_at ?? r.created_at)}</span>
      </div>
      {r.error && <p className="rounded-md bg-red-500/10 p-3 text-sm text-red-700 dark:text-red-300">{r.error}</p>}
      <div className="flex gap-2">
        {["failed", "dead", "cancelled"].includes(r.status) && (
          <Button size="sm" variant="outline" onClick={() => onControl(r, "retry")}>
            {t("automations.retry")}
          </Button>
        )}
        {["queued", "running", "waiting_for_input"].includes(r.status) && (
          <Button size="sm" variant="outline" onClick={() => confirm(t("automations.cancelConfirm")) && onControl(r, "cancel")}>
            {t("automations.cancel")}
          </Button>
        )}
      </div>
      {(events.data ?? []).length > 0 && (
        <div className="flex flex-col gap-1.5">
          <span className="text-[11px] font-medium tracking-wide text-muted-foreground uppercase">{t("automations.steps")}</span>
          <ol className="flex flex-col gap-1 text-xs">
            {events.data!.map((e) => (
              <li key={e.seq} className="flex gap-2">
                <span className="w-14 shrink-0 text-muted-foreground">{ago(e.at)}</span>
                <span className="min-w-0 break-words"><span className="font-medium">{e.kind}</span> {summary(e.data)}</span>
              </li>
            ))}
          </ol>
        </div>
      )}
      {r.output != null && (
        <details className="text-xs">
          <summary className="cursor-pointer text-muted-foreground">{t("automations.output")}</summary>
          <pre dir="ltr" className="mt-2 max-h-80 overflow-auto rounded-md bg-muted p-3 font-mono whitespace-pre-wrap break-all">{JSON.stringify(r.output, null, 2)}</pre>
        </details>
      )}
    </div>
  );
}
