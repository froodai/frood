// Who is told what and when, for the whole installation (QY1 — b): how urgent each kind of
// situation is, what an agent looks into first, the daily push fuse, due-date reminders and
// when something counts as stalled. What frood.yaml sets stays there; the rest is checked the
// same way.

import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useState } from "react";
import { toast } from "sonner";
import { TypeBadge } from "@/components/badges";
import { FromConfig, type Reminder, RemindersEditor } from "@/components/reminders";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { Skeleton } from "@/components/ui/skeleton";
import { useI18n } from "@/i18n";
import type { Key } from "@/i18n/en";
import { api } from "@/lib/frood";
import type { Type } from "@/lib/situations";
import { Card } from "./System";
import { useJobTitle } from "./Automations";

type From = "config" | "console" | "default";
type Before = { run: string; max_wait: string };
type Situation = {
  kind: string;
  risk: string | null;
  urgency: string;
  default_urgency: string;
  urgency_from: From;
  before: Before | null;
  before_from: From;
};
type Setting<T> = { value: T; default: T; from: From };
type Detector = Setting<string | number | null> & { name: string; kind: "duration" | "count" | "share" };
export type AttentionSettings = {
  situations: Situation[];
  max_push_per_day: Setting<number | null>;
  reminders: Setting<Reminder[]>;
  detectors: Detector[];
  investigators: string[];
};

const TYPE_OF: Record<string, Type> = {
  "proposal:new": "new",
  "proposal:change": "change",
  "proposal:conflict": "conflict",
  "proposal:stale_knowledge": "stale",
  "proposal:entity_merge": "merge",
  question: "question",
  reminder: "reminder",
  alert: "alert",
};
const typeOf = (s: { kind: string; risk: string | null }) => TYPE_OF[s.risk ? `${s.kind}:${s.risk}` : s.kind] ?? "alert";

export function Notifications() {
  const { t } = useI18n();
  const settings = useQuery({ queryKey: ["attention"], queryFn: () => api<AttentionSettings>("/v1/attention"), retry: false });
  if (settings.isError) return <p className="text-sm text-destructive">{settings.error.message.includes("admin") ? t("app.noAdmin") : settings.error.message}</p>;
  if (!settings.data) return <Skeleton className="h-96" />;
  return <AttentionForm key={JSON.stringify(settings.data)} settings={settings.data} />;
}

function AttentionForm({ settings }: { settings: AttentionSettings }) {
  const { t } = useI18n();
  const title = useJobTitle();
  const client = useQueryClient();
  const [situations, setSituations] = useState(settings.situations);
  const [maxPush, setMaxPush] = useState(settings.max_push_per_day.value?.toString() ?? "");
  const [reminders, setReminders] = useState<Reminder[]>(settings.reminders.value);
  const [detectors, setDetectors] = useState<Record<string, string>>(
    Object.fromEntries(settings.detectors.filter((d) => d.from !== "default").map((d) => [d.name, String(d.value ?? "")])),
  );
  const save = useMutation({
    mutationFn: () =>
      api("/v1/attention", {
        method: "PUT",
        json: {
          situations: situations.map(({ kind, risk, urgency, before }) => ({ kind, risk, urgency, before })),
          max_push_per_day: maxPush ? Number(maxPush) : null,
          reminders,
          detectors: Object.fromEntries(settings.detectors.map((d) => [d.name, detectors[d.name]?.trim() || null])),
        },
      }),
    onSuccess: () => {
      toast.success(t("attention.saved"));
      client.invalidateQueries({ queryKey: ["attention"] });
      client.invalidateQueries({ queryKey: ["my-settings"] });
    },
    onError: (err) => toast.error(t("inbox.failed", { error: err.message })),
  });
  const change = (i: number, patch: Partial<Situation>) => setSituations((all) => all.map((s, n) => (n === i ? { ...s, ...patch } : s)));
  const label = "text-xs font-medium text-muted-foreground";

  return (
    <form
      className="flex flex-col gap-4 text-sm"
      onSubmit={(e) => {
        e.preventDefault();
        save.mutate();
      }}
    >
      <Card title={t("attention.situations")}>
        <p className="text-muted-foreground">{t("attention.urgencyHint")}</p>
        <div className="flex flex-col divide-y">
          <div className="hidden gap-x-3 pb-2 text-xs font-medium text-muted-foreground sm:flex">
            <span className="w-36 shrink-0" />
            <span className="w-40">{t("attention.urgency")}</span>
            <span>{t("attention.before")}</span>
          </div>
          {situations.map((s, i) => {
            const urgencyFixed = s.urgency_from === "config";
            const beforeFixed = s.before_from === "config";
            return (
              <div key={`${s.kind}:${s.risk}`} className="flex flex-wrap items-center gap-x-3 gap-y-2 py-2.5">
                <span className="min-w-28 shrink-0 sm:w-36">
                  <TypeBadge type={typeOf(s)} />
                </span>
                <Select disabled={urgencyFixed} value={s.urgency} onValueChange={(urgency) => change(i, { urgency })}>
                  <SelectTrigger size="sm" className="w-40" aria-label={`${t("attention.urgency")}: ${t(`type.${typeOf(s)}` as Key)}`}>
                    <SelectValue>{t(`attention.urgency.${s.urgency}` as Key)}</SelectValue>
                  </SelectTrigger>
                  <SelectContent>
                    {(["now", "digest", "silent"] as const).map((u) => (
                      <SelectItem key={u} value={u}>
                        {t(`attention.urgency.${u}`)}
                        {u === s.default_urgency && <span className="text-muted-foreground"> · {t("attention.default")}</span>}
                      </SelectItem>
                    ))}
                  </SelectContent>
                </Select>
                <span className="flex flex-wrap items-center gap-2">
                  <span className="text-muted-foreground sm:hidden">{t("attention.before")}</span>
                  <Select
                    disabled={beforeFixed}
                    value={s.before?.run ?? "none"}
                    onValueChange={(run) => change(i, { before: run === "none" ? null : { run, max_wait: s.before?.max_wait ?? "30m" } })}
                  >
                    <SelectTrigger size="sm" className="w-auto min-w-28" aria-label={`${t("attention.before")}: ${t(`type.${typeOf(s)}` as Key)}`}>
                      <SelectValue />
                    </SelectTrigger>
                    <SelectContent>
                      <SelectItem value="none">{t("attention.beforeNone")}</SelectItem>
                      {settings.investigators.map((job) => (
                        <SelectItem key={job} value={job}>
                          {title(job)}
                        </SelectItem>
                      ))}
                    </SelectContent>
                  </Select>
                  {s.before && (
                    <Input
                      disabled={beforeFixed}
                      value={s.before.max_wait}
                      onChange={(e) => change(i, { before: { ...s.before!, max_wait: e.target.value } })}
                      className="h-8 w-20"
                      dir="ltr"
                      aria-label={t("attention.maxWait")}
                      title={t("attention.maxWait")}
                    />
                  )}
                </span>
                {(urgencyFixed || beforeFixed) && <FromConfig />}
              </div>
            );
          })}
        </div>
      </Card>

      <div className="grid grid-cols-1 gap-4 lg:grid-cols-2">
        <Card title={t("attention.maxPush")}>
          <p className="text-muted-foreground">{t("attention.maxPushHint")}</p>
          <span className="flex items-center gap-2">
            <Input
              type="number"
              min={0}
              disabled={settings.max_push_per_day.from === "config"}
              value={maxPush}
              onChange={(e) => setMaxPush(e.target.value)}
              placeholder={String(settings.max_push_per_day.default)}
              className="w-28"
              aria-label={t("attention.maxPush")}
            />
            {settings.max_push_per_day.from === "config" && <FromConfig />}
          </span>
        </Card>
        <Card title={t("attention.reminders")}>
          <p className="text-muted-foreground">{t("attention.remindersHint")}</p>
          {settings.reminders.from === "config" && <FromConfig />}
          <RemindersEditor value={reminders} onChange={setReminders} disabled={settings.reminders.from === "config"} />
        </Card>
      </div>

      <Card title={t("attention.detectors")}>
        <p className="text-muted-foreground">{t("attention.detectorsHint")}</p>
        <div className="grid grid-cols-1 gap-x-6 gap-y-2 md:grid-cols-2">
          {settings.detectors.map((d) => (
            <label key={d.name} className="flex items-center justify-between gap-3">
              <span className={label}>{t(`detector.${d.name}` as Key)}</span>
              <span className="flex items-center gap-2">
                {d.from === "config" && <FromConfig />}
                <Input
                  disabled={d.from === "config"}
                  value={d.from === "config" ? String(d.value ?? "") : (detectors[d.name] ?? "")}
                  onChange={(e) => setDetectors((all) => ({ ...all, [d.name]: e.target.value }))}
                  placeholder={String(d.default)}
                  inputMode={d.kind === "duration" ? "text" : "decimal"}
                  className="h-8 w-24"
                  dir="ltr"
                />
              </span>
            </label>
          ))}
        </div>
      </Card>

      <Button type="submit" className="self-start" disabled={save.isPending}>
        {t("connect.saveChanges")}
      </Button>
    </form>
  );
}
