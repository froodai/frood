import { Skeleton } from "@/components/ui/skeleton";
import { useI18n } from "@/i18n";
import { useHealth } from "@/lib/queries";
import { Card, Dot } from "./System";

export function Sources() {
  const { t, lang } = useI18n();
  const health = useHealth();
  const relative = new Intl.RelativeTimeFormat(lang, { numeric: "auto" });

  if (health.isError) return <p className="p-6 text-sm text-destructive">{health.error.message}</p>;
  if (!health.data) return <Skeleton className="m-6 h-48" />;
  const { sources, modules } = health.data;
  const since = (seconds: number) =>
    seconds < 3600
      ? relative.format(-Math.round(seconds / 60), "minute")
      : seconds < 86400
        ? relative.format(-Math.round(seconds / 3600), "hour")
        : relative.format(-Math.round(seconds / 86400), "day");

  return (
    <div className="flex max-w-6xl flex-col gap-5 px-4 py-5 md:px-7">
      <p className="text-sm text-muted-foreground">{t("sources.how")}</p>
      <div className="grid grid-cols-1 gap-3 sm:grid-cols-2 xl:grid-cols-3">
        {sources.length === 0 && <p className="text-sm text-muted-foreground">{t("sources.none")}</p>}
        {sources.map((s) => (
          <Card key={`${s.job}/${s.source}`} title={t("sources.by", { job: s.job })}>
            <div className="flex items-center gap-2">
              <Dot ok={s.seconds_since_read === null ? null : s.seconds_since_read < 2 * 86400} />
              <span className="font-medium">{s.source}</span>
            </div>
            <span className="text-sm text-muted-foreground">
              {s.seconds_since_read === null ? t("sources.never") : t("sources.read", { when: since(s.seconds_since_read) })}
            </span>
          </Card>
        ))}
      </div>
      <h2 className="text-sm font-medium">{t("sources.modules")}</h2>
      <div className="grid grid-cols-1 gap-3 sm:grid-cols-2 xl:grid-cols-3">
        {modules.map((m) => (
          <section key={m.name} className="flex flex-col gap-1.5 rounded-xl border bg-card p-4">
            <div className="flex items-center gap-2">
              <Dot ok={m.up} />
              <span className="flex-1 font-medium">{m.name}</span>
              <span className="text-xs text-muted-foreground">{m.up ? t("sources.up") : t("sources.down")}</span>
            </div>
            <span className="font-mono text-xs text-muted-foreground" dir="ltr">
              {m.provides.join(" · ")}
            </span>
          </section>
        ))}
      </div>
    </div>
  );
}
