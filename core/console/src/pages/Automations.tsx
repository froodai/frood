import { useMutation, useQueryClient } from "@tanstack/react-query";
import { useState } from "react";
import { toast } from "sonner";
import { Button } from "@/components/ui/button";
import { Skeleton } from "@/components/ui/skeleton";
import { useI18n } from "@/i18n";
import type { Key } from "@/i18n/en";
import { api } from "@/lib/frood";
import { useRuns } from "@/lib/queries";
import type { Run } from "@/lib/types";
import { cn } from "@/lib/utils";

const FILTERS: [string, Key][] = [
  ["", "automations.filter.all"],
  ["running", "automations.filter.active"],
  ["failed", "automations.filter.failed"],
];

const tone = (status: string) =>
  status === "succeeded" ? "bg-success" : status === "failed" || status === "dead" ? "bg-destructive" : status === "cancelled" ? "bg-border" : "bg-warning";

export function Automations() {
  const { t, ago, lang } = useI18n();
  const [filter, setFilter] = useState("");
  const runs = useRuns(filter);
  const client = useQueryClient();
  const control = useMutation({
    mutationFn: ({ run, what }: { run: Run; what: "retry" | "cancel" }) => api(`/v1/runs/${run.id}/${what}`, { method: "POST" }),
    onSuccess: () => client.invalidateQueries({ queryKey: ["runs"] }),
    onError: (err) => toast.error(t("inbox.failed", { error: err.message })),
  });
  const took = (run: Run) => {
    if (!run.started_at || !run.finished_at) return "";
    const seconds = (new Date(run.finished_at).getTime() - new Date(run.started_at).getTime()) / 1000;
    const [value, unit] = seconds < 90 ? [Math.round(seconds), "second"] : seconds < 5400 ? [Math.round(seconds / 60), "minute"] : [Math.round(seconds / 3600), "hour"];
    return new Intl.NumberFormat(lang, { style: "unit", unit, unitDisplay: "short" }).format(value);
  };
  const cost = (usd: number) => (usd > 0 ? new Intl.NumberFormat("en", { style: "currency", currency: "USD", maximumFractionDigits: 4 }).format(usd) : "—");

  return (
    <div className="flex flex-col gap-3.5 px-4 py-5 md:px-7">
      <div className="flex flex-wrap gap-2">
        {FILTERS.map(([value, label]) => (
          <Button key={value} size="sm" variant={filter === value ? "default" : "outline"} className="rounded-full" onClick={() => setFilter(value)}>
            {t(label)}
          </Button>
        ))}
      </div>
      {runs.isPending ? (
        <Skeleton className="h-64" />
      ) : runs.data?.length === 0 ? (
        <p className="text-sm text-muted-foreground">{t("automations.none")}</p>
      ) : (
        <div className="overflow-x-auto rounded-xl border bg-card">
          <div className="min-w-[640px] text-sm">
            <div className="grid grid-cols-[minmax(0,2fr)_minmax(0,1.4fr)_7rem_5rem_6rem_8rem] gap-3 border-b bg-muted/50 px-4 py-2 text-xs text-muted-foreground">
              <span>{t("automations.col.job")}</span>
              <span>{t("automations.col.state")}</span>
              <span>{t("automations.col.when")}</span>
              <span>{t("automations.col.took")}</span>
              <span>{t("automations.col.cost")}</span>
              <span />
            </div>
            {runs.data?.map((run) => (
              <div key={run.id} className="grid grid-cols-[minmax(0,2fr)_minmax(0,1.4fr)_7rem_5rem_6rem_8rem] items-baseline gap-3 border-b px-4 py-2.5 last:border-b-0">
                <span className="flex flex-col">
                  <span className="font-medium">{run.job}</span>
                  {run.error && <span className="line-clamp-2 text-xs text-destructive">{run.error}</span>}
                </span>
                <span className="inline-flex items-center gap-2">
                  <span className={cn("size-2 rounded-full", tone(run.status))} />
                  {t(`run.${run.status}` as Key)}
                </span>
                <span className="text-muted-foreground">{ago(run.started_at ?? run.created_at)}</span>
                <span className="text-muted-foreground">{took(run)}</span>
                <span className="font-mono text-xs">{cost(run.cost_usd)}</span>
                <span className="flex justify-end gap-1">
                  {["failed", "dead", "cancelled"].includes(run.status) && (
                    <Button size="sm" variant="outline" disabled={control.isPending} onClick={() => control.mutate({ run, what: "retry" })}>
                      {t("automations.retry")}
                    </Button>
                  )}
                  {["queued", "running", "waiting_for_input"].includes(run.status) && (
                    <Button
                      size="sm"
                      variant="outline"
                      disabled={control.isPending}
                      onClick={() => confirm(t("automations.cancelConfirm")) && control.mutate({ run, what: "cancel" })}
                    >
                      {t("automations.cancel")}
                    </Button>
                  )}
                </span>
              </div>
            ))}
          </div>
        </div>
      )}
    </div>
  );
}
