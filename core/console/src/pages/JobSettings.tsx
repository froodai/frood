// A job's settings from the console (QY1 — b): on or off, when it runs, its model, who pays
// and the limits of a run. What frood.yaml changes stays there; the rest is checked the same way.

import { useMutation, useQueryClient } from "@tanstack/react-query";
import { Plus, X } from "lucide-react";
import { useState } from "react";
import { toast } from "sonner";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { Switch } from "@/components/ui/switch";
import { useI18n } from "@/i18n";
import { api } from "@/lib/frood";

export type Trigger = { event?: string; schedule?: string; timezone?: string; for_each?: string };
export type JobSetting = {
  name: string;
  uses: string;
  from: "config" | "console" | "default";
  running: boolean;
  why: string | null;
  enabled: boolean;
  schedules: Trigger[];
  on_events: boolean;
  model: string | null;
  account: string | null;
  budget: { max_usd?: number | null; max_minutes?: number | null } | null;
};

type Kind = "daily" | "weekdays" | "hours" | "cron";
type Row = { kind: Kind; time: string; hours: string; cron: string; timezone: string; for_each?: string };

const here = Intl.DateTimeFormat().resolvedOptions().timeZone || "UTC";

/** A cron schedule as the editor shows it: the common shapes, or the expression itself. */
function toRow(trigger: Trigger): Row {
  const cron = trigger.schedule ?? "";
  const [minute, hour, day, month, weekday] = cron.trim().split(/\s+/);
  const base = { time: "08:30", hours: "6", cron, timezone: trigger.timezone ?? "UTC", for_each: trigger.for_each };
  const simple = day === "*" && month === "*" && /^\d+$/.test(minute ?? "");
  if (simple && /^\d+$/.test(hour ?? "")) {
    const time = `${hour.padStart(2, "0")}:${minute.padStart(2, "0")}`;
    if (weekday === "*") return { ...base, kind: "daily", time };
    if (weekday === "1-5") return { ...base, kind: "weekdays", time };
  }
  const every = /^\*\/(\d+)$/.exec(hour ?? "");
  if (simple && every && weekday === "*") return { ...base, kind: "hours", hours: every[1] };
  return { ...base, kind: "cron" };
}

function toTrigger(row: Row): Trigger {
  const [hour, minute] = row.time.split(":").map((part) => String(Number(part)));
  const schedule =
    row.kind === "daily"
      ? `${minute} ${hour} * * *`
      : row.kind === "weekdays"
        ? `${minute} ${hour} * * 1-5`
        : row.kind === "hours"
          ? `0 */${Math.max(1, Number(row.hours) || 1)} * * *`
          : row.cron.trim();
  return { schedule, timezone: row.timezone.trim() || "UTC", ...(row.for_each ? { for_each: row.for_each } : {}) };
}

export function JobForm({ job, accounts, title, onDone }: { job: JobSetting; accounts: string[]; title: string; onDone: () => void }) {
  const { t } = useI18n();
  const client = useQueryClient();
  const [enabled, setEnabled] = useState(job.enabled);
  const [rows, setRows] = useState<Row[]>(job.schedules.map(toRow));
  const [model, setModel] = useState(job.model ?? "");
  const [account, setAccount] = useState(job.account ?? "");
  const [maxUsd, setMaxUsd] = useState(job.budget?.max_usd?.toString() ?? "");
  const [maxMinutes, setMaxMinutes] = useState(job.budget?.max_minutes?.toString() ?? "");
  const usesModel = job.uses === "llm.complete" || job.uses === "agents.execute";
  const agent = job.uses === "agents.execute";
  const fixed = job.from === "config";
  const settle = () => {
    client.invalidateQueries({ queryKey: ["job-settings"] });
    client.invalidateQueries({ queryKey: ["jobs"] });
  };
  const failed = (err: Error) => toast.error(t("inbox.failed", { error: err.message }));
  const save = useMutation({
    mutationFn: () =>
      api<{ why: string | null }>(`/v1/jobs/${job.name}`, {
        method: "PUT",
        json: {
          enabled,
          ...(job.on_events ? {} : { schedules: rows.map(toTrigger) }),
          ...(usesModel ? { model, account } : {}),
          ...(agent
            ? { budget: { max_usd: maxUsd ? Number(maxUsd) : undefined, max_minutes: maxMinutes ? Number(maxMinutes) : undefined } }
            : {}),
        },
      }),
    onSuccess: ({ why }) => {
      if (why && enabled) toast.warning(t("jobs.savedNotRunning", { why }));
      else toast.success(t("jobs.saved", { job: title }));
      settle();
      onDone();
    },
    onError: failed,
  });
  const reset = useMutation({
    mutationFn: () => api(`/v1/jobs/${job.name}`, { method: "DELETE" }),
    onSuccess: () => {
      settle();
      onDone();
    },
    onError: failed,
  });
  const change = (i: number, patch: Partial<Row>) => setRows((all) => all.map((row, n) => (n === i ? { ...row, ...patch } : row)));
  const label = "text-xs font-medium text-muted-foreground";

  return (
    <form
      className="flex flex-col gap-4"
      onSubmit={(e) => {
        e.preventDefault();
        save.mutate();
      }}
    >
      <h2 className="text-lg font-semibold">{title}</h2>
      {!job.running && job.why && <p className="rounded-md border border-warning/50 p-3 text-warning">{job.why}</p>}
      {fixed && <p className="text-muted-foreground">{t("jobs.configOnly")}</p>}
      <fieldset disabled={fixed} className="flex flex-col gap-4 disabled:opacity-60">
        <label className="flex items-center gap-3">
          <Switch checked={enabled} onCheckedChange={setEnabled} />
          <span>{t("jobs.enabled")}</span>
        </label>
        <div className="flex flex-col gap-2">
          <span className={label}>{t("jobs.schedules")}</span>
          {job.on_events ? (
            <p className="text-muted-foreground">{t("jobs.onEvents")}</p>
          ) : (
            <>
              {rows.length === 0 && <p className="text-muted-foreground">{t("trigger.byHand")}</p>}
              {rows.map((row, i) => (
                <div key={i} className="flex flex-wrap items-center gap-2 rounded-md border p-2">
                  <Select value={row.kind} onValueChange={(kind) => change(i, { kind: kind as Kind })}>
                    <SelectTrigger size="sm" className="w-auto min-w-36" aria-label={t("jobs.schedules")}>
                      <SelectValue />
                    </SelectTrigger>
                    <SelectContent>
                      <SelectItem value="daily">{t("jobs.kind.daily")}</SelectItem>
                      <SelectItem value="weekdays">{t("jobs.kind.weekdays")}</SelectItem>
                      <SelectItem value="hours">{t("jobs.kind.hours")}</SelectItem>
                      <SelectItem value="cron">{t("jobs.kind.cron")}</SelectItem>
                    </SelectContent>
                  </Select>
                  {(row.kind === "daily" || row.kind === "weekdays") && (
                    <Input type="time" value={row.time} onChange={(e) => change(i, { time: e.target.value })} className="h-8 w-28" aria-label={t("jobs.time")} />
                  )}
                  {row.kind === "hours" && (
                    <Input type="number" min={1} max={23} value={row.hours} onChange={(e) => change(i, { hours: e.target.value })} className="h-8 w-20" aria-label={t("jobs.every")} />
                  )}
                  {row.kind === "cron" && (
                    <Input value={row.cron} onChange={(e) => change(i, { cron: e.target.value })} placeholder="30 8 * * 1-5" className="h-8 w-40 font-mono" aria-label={t("jobs.cron")} dir="ltr" />
                  )}
                  <Input value={row.timezone} onChange={(e) => change(i, { timezone: e.target.value })} className="h-8 w-44" aria-label={t("jobs.timezone")} dir="ltr" />
                  <button type="button" onClick={() => setRows((all) => all.filter((_, n) => n !== i))} aria-label={t("attach.remove")} className="ms-auto text-muted-foreground hover:text-foreground">
                    <X className="size-4" />
                  </button>
                </div>
              ))}
              <Button
                type="button"
                variant="outline"
                size="sm"
                className="self-start"
                onClick={() => setRows((all) => [...all, { kind: "daily", time: "08:30", hours: "6", cron: "", timezone: here, for_each: job.name === "daily-sweep" ? "source" : undefined }])}
              >
                <Plus className="size-3.5" /> {t("jobs.addSchedule")}
              </Button>
            </>
          )}
        </div>
        {usesModel && (
          <div className="grid grid-cols-1 gap-3 sm:grid-cols-2">
            <label className="flex flex-col gap-1.5">
              <span className={label}>{t("jobs.model")}</span>
              <Input value={model} onChange={(e) => setModel(e.target.value)} dir="ltr" />
            </label>
            <label className="flex flex-col gap-1.5">
              <span className={label}>{t("jobs.account")}</span>
              <Select value={account || "none"} onValueChange={(v) => setAccount(v === "none" ? "" : v)}>
                <SelectTrigger className="w-full" aria-label={t("jobs.account")}>
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="none">—</SelectItem>
                  {accounts.map((a) => (
                    <SelectItem key={a} value={a}>
                      {a}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
            </label>
          </div>
        )}
        {agent && (
          <div className="flex flex-col gap-2">
            <span className={label}>{t("jobs.budget")}</span>
            <div className="grid grid-cols-2 gap-3">
              <Input type="number" min={0} step="0.01" value={maxUsd} onChange={(e) => setMaxUsd(e.target.value)} aria-label={t("jobs.maxUsd")} placeholder={t("jobs.maxUsd")} />
              <Input type="number" min={1} value={maxMinutes} onChange={(e) => setMaxMinutes(e.target.value)} aria-label={t("jobs.maxMinutes")} placeholder={t("jobs.maxMinutes")} />
            </div>
          </div>
        )}
        <div className="flex flex-wrap gap-2">
          <Button type="submit" disabled={save.isPending}>
            {t("connect.saveChanges")}
          </Button>
          {job.from === "console" && (
            <Button type="button" variant="outline" disabled={reset.isPending} onClick={() => reset.mutate()}>
              {t("jobs.reset")}
            </Button>
          )}
        </div>
      </fieldset>
    </form>
  );
}
