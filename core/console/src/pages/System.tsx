import { useQuery } from "@tanstack/react-query";
import { useMemo, useState } from "react";
import { toast } from "sonner";
import { Button } from "@/components/ui/button";
import { Skeleton } from "@/components/ui/skeleton";
import { useI18n } from "@/i18n";
import { api } from "@/lib/frood";
import { useHealth, useStartJob } from "@/lib/queries";
import { cn } from "@/lib/utils";

export function Card({ title, children, className }: { title: string; children: React.ReactNode; className?: string }) {
  return (
    <section className={cn("flex flex-col gap-2 rounded-xl border bg-card p-4", className)}>
      <h2 className="text-sm text-muted-foreground">{title}</h2>
      {children}
    </section>
  );
}

export function Dot({ ok }: { ok: boolean | null }) {
  return <span className={cn("inline-block size-2 shrink-0 rounded-full", ok === null ? "bg-warning" : ok ? "bg-success" : "bg-destructive")} />;
}

/** "3 minutes", from seconds. */
export function useSpan() {
  const { lang } = useI18n();
  return (seconds: number) => {
    const unit = seconds < 3600 ? "minute" : seconds < 86400 ? "hour" : "day";
    const value = Math.max(1, Math.round(seconds / (unit === "minute" ? 60 : unit === "hour" ? 3600 : 86400)));
    return new Intl.NumberFormat(lang, { style: "unit", unit, unitDisplay: "long" }).format(value);
  };
}

export function System() {
  const { t, ago } = useI18n();
  const health = useHealth();
  const start = useStartJob();
  const span = useSpan();
  const usd = (value: number) => new Intl.NumberFormat("en", { style: "currency", currency: "USD" }).format(value);

  if (health.isError) return <p className="p-6 text-sm text-destructive">{health.error.message.includes("admin") ? t("app.noAdmin") : health.error.message}</p>;
  if (!health.data) return <Skeleton className="m-6 h-48" />;
  const h = health.data;
  const job = (name: string) =>
    start.mutate(name, {
      onSuccess: () => toast.success(t("system.started")),
      onError: (err) => toast.error(t("inbox.failed", { error: err.message })),
    });

  return (
    <div className="flex flex-col gap-4 px-4 py-5 md:px-7">
      <div className="grid grid-cols-1 gap-3 sm:grid-cols-2 xl:grid-cols-4">
        <Card title={t("system.health")}>
          <span className="text-lg font-semibold">{t("system.version", { version: h.version })}</span>
          <ul className="flex flex-col gap-1 text-sm">
            {h.modules.map((m) => (
              <li key={m.name} className="flex items-center gap-2">
                <Dot ok={m.up} /> {m.name} <span className="text-muted-foreground">{m.up ? t("sources.up") : t("sources.down")}</span>
              </li>
            ))}
          </ul>
        </Card>
        <Card title={t("system.dispatcher")}>
          <span className="text-lg font-semibold">{h.dispatcher_lag_seconds > 60 ? t("system.lag", { when: span(h.dispatcher_lag_seconds) }) : t("system.upToDate")}</span>
          <span className="text-sm text-muted-foreground">{t("system.queueLine", { running: h.runs.running, queued: h.runs.queued, waiting: h.runs.waiting_for_input })}</span>
          <span className="text-sm text-muted-foreground">{t("system.dayLine", { ok: h.runs.succeeded_last_day, failed: h.runs.failed_last_day })}</span>
        </Card>
        <Card title={t("system.inbox")}>
          <span className="text-lg font-semibold">{t("system.inboxLine", { open: h.inbox_open, alerts: h.alerts_open })}</span>
        </Card>
        <Card title={t("system.spending")}>
          {h.accounts.map((a) => (
            <div key={a.account} className="flex items-baseline justify-between gap-3 text-sm">
              <span>{a.account}</span>
              <span className="font-mono">
                {usd(a.spent_usd)} <span className="text-muted-foreground">{a.monthly_usd ? t("system.ofLimit", { limit: usd(a.monthly_usd) }) : t("system.noLimit")}</span>
              </span>
            </div>
          ))}
        </Card>
      </div>

      <Card title={t("system.backups")}>
        {h.backups ? (
          <div className="flex flex-col gap-3">
            <div className="grid grid-cols-1 gap-3 sm:grid-cols-2">
              <div className="flex items-center gap-2 text-sm">
                <Dot ok={h.backups.last_backup ? h.backups.last_backup.status === "succeeded" : null} />
                <span>{t("system.lastBackup")}:</span>
                <span className="text-muted-foreground">{h.backups.last_backup ? ago(h.backups.last_backup.finished_at) : t("system.never")}</span>
              </div>
              <div className="flex items-center gap-2 text-sm">
                <Dot ok={h.backups.last_verify ? h.backups.last_verify.status === "succeeded" : null} />
                <span>{t("system.lastVerify")}:</span>
                <span className="text-muted-foreground">
                  {h.backups.last_verify
                    ? `${h.backups.last_verify.status === "succeeded" ? t("system.passed") : t("system.notPassed")} · ${ago(h.backups.last_verify.finished_at)}`
                    : t("system.never")}
                </span>
              </div>
            </div>
            <span className="text-sm text-muted-foreground">{t("system.targets", { targets: h.backups.targets.join(", ") })}</span>
            <div className="flex flex-wrap gap-2">
              <Button size="sm" disabled={start.isPending} onClick={() => job("backup")}>
                {t("system.backupNow")}
              </Button>
              <Button size="sm" variant="outline" disabled={start.isPending} onClick={() => job("backup-verify")}>
                {t("system.verifyNow")}
              </Button>
            </div>
          </div>
        ) : (
          <p className="text-sm text-muted-foreground">{t("system.backupsOff")}</p>
        )}
      </Card>

      <Logs />
    </div>
  );
}

type LogLine = { seq: number; line: string | { timestamp?: string; level?: string; target?: string; fields?: Record<string, unknown> } };
type Parsed = { seq: number; time: string; level: string; target: string; message: string; rest: string };

/** A log line: the server writes JSON (time, level, target, message and fields); anything else as it is. */
function parse({ seq, line }: LogLine): Parsed {
  if (typeof line === "string") return { seq, time: "", level: "", target: "", message: line, rest: "" };
  const { message, ...fields } = line.fields ?? {};
  const rest = Object.entries(fields)
    .map(([k, v]) => `${k}=${typeof v === "string" ? v : JSON.stringify(v)}`)
    .join(" ");
  return {
    seq,
    time: line.timestamp ? new Date(line.timestamp).toLocaleTimeString([], { hour12: false }) : "",
    level: (line.level ?? "").toUpperCase(),
    target: line.target ?? "",
    message: typeof message === "string" ? message : JSON.stringify(message ?? ""),
    rest,
  };
}

const LEVEL_TONE: Record<string, string> = {
  ERROR: "bg-destructive/15 text-destructive",
  WARN: "bg-warning-soft text-warning",
  INFO: "bg-secondary text-muted-foreground",
  DEBUG: "bg-secondary text-muted-foreground",
};

function Logs() {
  const { t } = useI18n();
  const [filter, setFilter] = useState("");
  const [level, setLevel] = useState<"" | "WARN" | "ERROR">("");
  const logs = useQuery({
    queryKey: ["logs"],
    queryFn: () => api<LogLine[]>("/v1/logs?limit=500"),
    refetchInterval: 10_000,
  });
  const lines = useMemo(() => {
    const needle = filter.toLowerCase();
    const wanted = level === "ERROR" ? ["ERROR"] : level === "WARN" ? ["WARN", "ERROR"] : null;
    return (logs.data ?? [])
      .map(parse)
      .filter((l) => !wanted || wanted.includes(l.level))
      .filter((l) => !needle || `${l.target} ${l.message} ${l.rest}`.toLowerCase().includes(needle))
      .slice(-300)
      .reverse();
  }, [logs.data, filter, level]);
  return (
    <Card title={t("system.logs")}>
      <div className="flex flex-wrap items-center gap-2">
        {(
          [
            ["", "system.logsAll"],
            ["WARN", "system.logsWarn"],
            ["ERROR", "system.logsError"],
          ] as const
        ).map(([value, label]) => (
          <Button key={value} size="sm" variant={level === value ? "default" : "outline"} className="rounded-full" onClick={() => setLevel(value)}>
            {t(label)}
          </Button>
        ))}
        <input
          value={filter}
          onChange={(e) => setFilter(e.target.value)}
          placeholder={t("system.logsFilter")}
          aria-label={t("system.logsFilter")}
          className="h-9 min-w-40 flex-1 rounded-md border bg-background px-3 text-sm outline-none focus:ring-2 focus:ring-ring/40"
        />
      </div>
      <div dir="ltr" className="max-h-[32rem] overflow-auto rounded-md border bg-background font-mono text-xs">
        {lines.map((l) => (
          <div key={l.seq} className="flex gap-2 border-b px-3 py-1.5 last:border-b-0">
            <span className="w-16 shrink-0 text-muted-foreground">{l.time}</span>
            {l.level && <span className={cn("h-fit shrink-0 rounded px-1.5 text-[10px] font-semibold", LEVEL_TONE[l.level] ?? LEVEL_TONE.INFO)}>{l.level}</span>}
            <span className="min-w-0 break-words">
              <span className={cn(l.level === "ERROR" && "text-destructive")}>{l.message}</span>
              {l.rest && <span className="text-muted-foreground"> {l.rest}</span>}
              {l.target && <span className="block text-[11px] text-muted-foreground/70">{l.target}</span>}
            </span>
          </div>
        ))}
        {lines.length === 0 && <p className="p-3 text-muted-foreground">{t("now.nothing")}</p>}
      </div>
    </Card>
  );
}
