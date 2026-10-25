// Reminders of due dates: days from the due date and a time of day, in the person's zone.

import { Plus, X } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { useI18n } from "@/i18n";

export type Reminder = { days: number; at: string };

/** "2 days before, at 09:00". */
export function useReminderText() {
  const { t } = useI18n();
  return ({ days, at }: Reminder) =>
    days < 0 ? t("attention.remind.before", { n: -days, time: at }) : days > 0 ? t("attention.remind.after", { n: days, time: at }) : t("attention.remind.on", { time: at });
}

export function RemindersEditor({ value, onChange, disabled }: { value: Reminder[]; onChange: (value: Reminder[]) => void; disabled?: boolean }) {
  const { t } = useI18n();
  const change = (i: number, patch: Partial<Reminder>) => onChange(value.map((r, n) => (n === i ? { ...r, ...patch } : r)));
  return (
    <div className="flex flex-col gap-2">
      {value.length === 0 && <p className="text-muted-foreground">{t("attention.remindersNone")}</p>}
      {value.map((r, i) => {
        const when = r.days < 0 ? "before" : r.days > 0 ? "after" : "on";
        return (
          <div key={i} className="flex flex-wrap items-center gap-2 rounded-md border p-2">
            <Select
              disabled={disabled}
              value={when}
              onValueChange={(w) => change(i, { days: w === "on" ? 0 : (w === "before" ? -1 : 1) * Math.max(1, Math.abs(r.days)) })}
            >
              <SelectTrigger size="sm" className="w-auto min-w-36" aria-label={t("attention.reminders")}>
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value="before">{t("attention.when.before")}</SelectItem>
                <SelectItem value="on">{t("attention.when.on")}</SelectItem>
                <SelectItem value="after">{t("attention.when.after")}</SelectItem>
              </SelectContent>
            </Select>
            {when !== "on" && (
              <Input
                type="number"
                min={1}
                max={365}
                disabled={disabled}
                value={Math.abs(r.days)}
                onChange={(e) => change(i, { days: (when === "before" ? -1 : 1) * Math.max(1, Number(e.target.value) || 1) })}
                className="h-8 w-20"
                aria-label={t("attention.days")}
              />
            )}
            <Input type="time" disabled={disabled} value={r.at} onChange={(e) => change(i, { at: e.target.value })} className="h-8 w-28" aria-label={t("attention.at")} />
            {!disabled && (
              <button type="button" onClick={() => onChange(value.filter((_, n) => n !== i))} aria-label={t("attach.remove")} className="ms-auto text-muted-foreground hover:text-foreground">
                <X className="size-4" />
              </button>
            )}
          </div>
        );
      })}
      {!disabled && (
        <Button type="button" variant="outline" size="sm" className="self-start" onClick={() => onChange([...value, { days: value.length ? 0 : -1, at: "09:00" }])}>
          <Plus className="size-3.5" /> {t("attention.addReminder")}
        </Button>
      )}
    </div>
  );
}

/** A small mark next to a setting frood.yaml makes. */
export function FromConfig() {
  const { t } = useI18n();
  return <span className="rounded-md border px-1.5 text-[11px] font-normal text-muted-foreground">{t("connect.fromConfig")}</span>;
}
