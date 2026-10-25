import { useQuery } from "@tanstack/react-query";
import { ArrowRight } from "lucide-react";
import { Link, useSearchParams } from "react-router";
import { UnverifiedBadge } from "@/components/badges";
import { Button } from "@/components/ui/button";
import { Skeleton } from "@/components/ui/skeleton";
import { useI18n } from "@/i18n";
import { api } from "@/lib/frood";
import { useInbox, waiting } from "@/lib/queries";
import { titled } from "@/lib/situations";
import { sphereIcon, useSphereName, useSpheres } from "@/lib/spheres";
import type { Assertion } from "@/lib/types";
import { cn } from "@/lib/utils";

type Due = Assertion & { due: string; overdue: boolean };
type Changed = { assertion: Assertion; before: string | null };
type NowView = {
  projects: { id: string; name: string }[];
  names: Record<string, string>;
  deadlines: Due[];
  decisions: Assertion[];
  owners: Assertion[];
  questions: Assertion[];
  changes: Changed[];
};

function Panel({ title, sub, children, empty }: { title: string; sub?: string; children: React.ReactNode[]; empty: string }) {
  return (
    <section aria-label={title} className="flex flex-col overflow-hidden rounded-xl border bg-card">
      <div className="flex items-baseline justify-between gap-3 border-b px-4 py-3">
        <h2 className="text-sm font-semibold">{title}</h2>
        {sub && <span className="text-xs text-muted-foreground">{sub}</span>}
      </div>
      {children.length ? children : <p className="px-4 py-3 text-sm text-muted-foreground">{empty}</p>}
    </section>
  );
}

/** A title, with the text under it in one line when there is more to say. */
function What({ a }: { a: Assertion }) {
  const { title, description } = titled(a.title, a.text);
  const name = useSphereName();
  const Icon = a.sphere ? sphereIcon(a.sphere) : null;
  return (
    <>
      <span>
        {Icon && a.sphere && (
          <span aria-hidden title={name(a.sphere)} className="me-1.5 inline-flex align-[-2px] text-muted-foreground">
            <Icon className="size-3.5" />
          </span>
        )}
        {title}
        {a.unverified && <UnverifiedBadge className="ms-1.5 align-[1px]" />}
      </span>
      {description && <span className="line-clamp-1 text-[13px] text-muted-foreground">{description}</span>}
    </>
  );
}

function Row({ to, children, className }: { to: string; children: React.ReactNode; className?: string }) {
  return (
    <Link to={to} className={cn("flex gap-3 border-b px-4 py-2.5 text-sm last:border-b-0 hover:bg-accent/50", className)}>
      {children}
    </Link>
  );
}

export function Now() {
  const { t, date, ago } = useI18n();
  const [params, setParams] = useSearchParams();
  const project = params.get("project") ?? "";
  const sphere = params.get("sphere") ?? "";
  const spheres = useSpheres().data ?? [];
  const sphereName = useSphereName();
  const inbox = useInbox();
  const now = useQuery({
    queryKey: ["now", project, sphere],
    queryFn: () => {
      const query = new URLSearchParams({ ...(project ? { project } : {}), ...(sphere ? { sphere } : {}) }).toString();
      return api<NowView>(`/v1/now${query ? `?${query}` : ""}`);
    },
    refetchInterval: 60_000,
  });
  const narrow = (next: { project?: string; sphere?: string }) => {
    const merged = { project, sphere, ...next };
    setParams({ ...(merged.project ? { project: merged.project } : {}), ...(merged.sphere ? { sphere: merged.sphere } : {}) });
  };
  const count = waiting(inbox.data).length;
  const v = now.data;
  const about = (a: Assertion) => a.subjects.map((s) => v?.names[s]).filter(Boolean).join(", ");
  const link = (a: Assertion) => `../knowledge/${a.assertion_id}`;

  return (
    <div className="flex max-w-6xl flex-col gap-5 px-4 py-5 md:px-7">
      <div className="grid grid-cols-2 gap-3 lg:grid-cols-4">
        <Link to="../inbox" className="flex flex-col gap-1 rounded-xl border bg-card p-4 hover:border-primary">
          <span className="text-sm text-muted-foreground">{t("now.waiting")}</span>
          <span className="text-3xl font-semibold tracking-tight">{count}</span>
          <span className="inline-flex items-center gap-1 text-sm text-primary">
            {t("now.review")} <ArrowRight className="size-3.5 rtl:rotate-180" />
          </span>
        </Link>
        <div className="flex flex-col gap-1 rounded-xl border bg-card p-4">
          <span className="text-sm text-muted-foreground">{t("now.deadlines")}</span>
          <span className="text-3xl font-semibold tracking-tight">{v?.deadlines.length ?? "–"}</span>
          {v && v.deadlines.some((d) => d.overdue) ? (
            <span className="text-sm text-destructive">{t("now.overdueCount", { n: v.deadlines.filter((d) => d.overdue).length })}</span>
          ) : (
            v?.deadlines[0] && <span className="text-sm text-warning">{t("now.soonest", { when: date(v.deadlines[0].due) })}</span>
          )}
        </div>
        <div className="flex flex-col gap-1 rounded-xl border bg-card p-4">
          <span className="text-sm text-muted-foreground">{t("now.changes")}</span>
          <span className="text-3xl font-semibold tracking-tight">{v?.changes.length ?? "–"}</span>
          <span className="text-sm text-muted-foreground">{t("now.changes.sub")}</span>
        </div>
        <div className="flex flex-col gap-1 rounded-xl border bg-card p-4">
          <span className="text-sm text-muted-foreground">{t("now.questions")}</span>
          <span className="text-3xl font-semibold tracking-tight">{v?.questions.length ?? "–"}</span>
        </div>
      </div>

      {spheres.length > 0 && (
        <div className="flex flex-wrap items-center gap-2">
          <span className="me-1 text-sm text-muted-foreground">{t("sphere.label")}</span>
          {[{ id: "", name: t("sphere.all") }, ...spheres.map((s) => ({ id: s.id, name: sphereName(s.id) }))].map((s) => {
            const Icon = s.id ? sphereIcon(s.id) : null;
            return (
              <Button key={s.id} size="sm" variant={sphere === s.id ? "default" : "outline"} className="rounded-full" onClick={() => narrow({ sphere: s.id })}>
                {Icon && <Icon className="size-3.5" aria-hidden />}
                {s.name}
              </Button>
            );
          })}
        </div>
      )}

      {v && v.projects.length > 0 && (
        <div className="flex flex-wrap items-center gap-2">
          <span className="me-1 text-sm text-muted-foreground">{t("now.project")}</span>
          {[{ id: "", name: t("now.allProjects") }, ...v.projects].map((p) => (
            <Button key={p.id} size="sm" variant={project === p.id ? "default" : "outline"} className="rounded-full" onClick={() => narrow({ project: p.id })}>
              {p.name}
            </Button>
          ))}
        </div>
      )}

      {now.isError ? (
        <p className="text-sm text-destructive">{now.error.message}</p>
      ) : !v ? (
        <div className="grid grid-cols-1 gap-4 lg:grid-cols-2">
          {[0, 1, 2, 3].map((i) => (
            <Skeleton key={i} className="h-48" />
          ))}
        </div>
      ) : (
        <div className="grid grid-cols-1 gap-4 lg:grid-cols-2">
          <Panel title={t("now.deadlines")} sub={t("now.deadlines.sub")} empty={t("now.nothing")}>
            {v.deadlines.map((d) => (
              <Row key={d.assertion_id} to={link(d)} className="items-baseline">
                <span className={cn("w-20 shrink-0 font-mono text-xs", d.overdue ? "text-destructive" : "text-muted-foreground")}>{d.overdue ? t("now.overdue") : date(d.due)}</span>
                <span className="flex min-w-0 flex-1 flex-col gap-0.5">
                  <What a={d} />
                  {about(d) && <span className="text-xs text-muted-foreground">{about(d)}</span>}
                </span>
              </Row>
            ))}
          </Panel>
          <Panel title={t("now.decisions")} empty={t("now.nothing")}>
            {v.decisions.map((d) => (
              <Row key={d.assertion_id} to={link(d)} className="flex-col gap-0.5">
                <What a={d} />
                <span className="text-xs text-muted-foreground">
                  {[about(d), date(d.valid_from)].filter(Boolean).join(" · ")}
                </span>
              </Row>
            ))}
          </Panel>
          <Panel title={t("now.owners")} empty={t("now.nothing")}>
            {v.owners.map((o) => (
              <Row key={o.assertion_id} to={link(o)}>
                <span className="flex min-w-0 flex-1 flex-col gap-0.5">
                  <What a={o} />
                </span>
              </Row>
            ))}
          </Panel>
          <Panel title={t("now.changes")} sub={t("now.changes.sub")} empty={t("now.nothing")}>
            {v.changes.map(({ assertion: a, before }) => (
              <Row key={`${a.assertion_id}/${a.version}`} to={link(a)} className="flex-col gap-0.5">
                {before && <span className="text-[13px] text-muted-foreground line-through">{before}</span>}
                <What a={a} />
                <span className="text-xs text-muted-foreground">{ago(a.recorded_from)}</span>
              </Row>
            ))}
          </Panel>
          {v.questions.length > 0 && (
            <Panel title={t("now.questions")} empty={t("now.nothing")}>
              {v.questions.map((q) => (
                <Row key={q.assertion_id} to={link(q)}>
                  <span className="flex min-w-0 flex-1 flex-col gap-0.5">
                    <What a={q} />
                  </span>
                </Row>
              ))}
            </Panel>
          )}
        </div>
      )}
    </div>
  );
}
