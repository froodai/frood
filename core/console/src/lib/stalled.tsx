import { Link } from "react-router";
import { useI18n } from "@/i18n";

/** Why Frood has learned nothing for three working days (docs/33 S1), from `/v1/now` and the digest. */
export type Stalled = {
  reason: "not_scheduled" | "not_run" | "failed" | "access_denied" | "found_nothing";
  job: string;
  source: string | null;
  run_id: string | null;
};

/** The reason in words and where to look: the failed run when there is one, else the jobs. */
export function useStalledText(s: Stalled | null | undefined) {
  const { t } = useI18n();
  if (!s) return null;
  return {
    title: t("stalled.title"),
    reason: t(`stalled.${s.reason}`, { source: s.source ?? "" }),
    open: s.run_id ? t("stalled.openRun") : t("stalled.openJob"),
    to: s.run_id ? `automations?run=${s.run_id}` : "automations",
  };
}

/** The banner on the «Сейчас» page. */
export function StalledBanner({ stalled }: { stalled: Stalled }) {
  const { title, reason, open, to } = useStalledText(stalled)!;
  return (
    <div role="status" className="flex flex-wrap items-baseline gap-x-3 gap-y-1 rounded-xl border border-warning/40 bg-warning-soft px-4 py-3 text-sm">
      <span className="font-semibold">{title}.</span>
      <span>{reason}.</span>
      <Link to={`../${to}`} className="ms-auto text-primary hover:underline">
        {open}
      </Link>
    </div>
  );
}
