import { useMutation, useQueryClient } from "@tanstack/react-query";
import { ArrowRight, Check, ChevronDown, ExternalLink, Search, X } from "lucide-react";
import { useMemo, useState } from "react";
import { useNavigate, useParams } from "react-router";
import { toast } from "sonner";
import { KindBadge, SphereBadge, SphereFilter, SpherePicker, TypeBadge } from "@/components/badges";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { Sheet, SheetContent, SheetTitle } from "@/components/ui/sheet";
import { Skeleton } from "@/components/ui/skeleton";
import { useI18n } from "@/i18n";
import type { Key } from "@/i18n/en";
import { api } from "@/lib/frood";
import { useKeys, useMediaQuery } from "@/lib/hooks";
import { useInbox, waiting } from "@/lib/queries";
import { kindTone } from "@/lib/kinds";
import { headline, kindOf, type Origin, originOf, proposal, sphereOf, summary, TONE, type Type, TYPES, typeKey, typeOf } from "@/lib/situations";
import type { Change, InboxEntry, QuestionContent } from "@/lib/types";
import { cn } from "@/lib/utils";

type Act = { action: string; alternative?: string; answer?: string; sphere?: string };
const ORIGINS: Origin[] = ["agent", "extract", "person"];

/** Decides at once on screen; the server catches up, and a failure puts the items back. */
function useAct() {
  const client = useQueryClient();
  return useMutation({
    mutationFn: ({ ids, act }: { ids: string[]; act: Act }) =>
      Promise.all(ids.map((id) => api(`/v1/inbox/${id}/act`, { method: "POST", json: act }))),
    onMutate: async ({ ids }) => {
      await client.cancelQueries({ queryKey: ["inbox"] });
      const before = client.getQueryData<InboxEntry[]>(["inbox"]);
      client.setQueryData<InboxEntry[]>(["inbox"], (entries) =>
        entries?.map((e) => (ids.includes(e.item.id) ? { ...e, item: { ...e.item, state: "resolved" } } : e)),
      );
      return { before };
    },
    onError: (_err, _vars, context) => context?.before && client.setQueryData(["inbox"], context.before),
    onSettled: () => client.invalidateQueries({ queryKey: ["inbox"] }),
  });
}

function useUndo() {
  const client = useQueryClient();
  return useMutation({
    mutationFn: (ids: string[]) => Promise.all(ids.map((id) => api(`/v1/inbox/${id}/undo`, { method: "POST" }))),
    onSettled: () => client.invalidateQueries({ queryKey: ["inbox"] }),
  });
}

function useOriginLabel() {
  const { t } = useI18n();
  return (entry: InboxEntry) => {
    const { origin, job } = originOf(entry);
    if (origin === "agent") return job ? t("origin.agentJob", { job }) : t("origin.agent");
    return t(`origin.${origin}` as Key);
  };
}

export function Inbox() {
  const { t, ago } = useI18n();
  const { id } = useParams();
  const navigate = useNavigate();
  const wide = useMediaQuery("(min-width: 1024px)");
  const inbox = useInbox();
  const act = useAct();
  const undo = useUndo();
  const originLabel = useOriginLabel();
  const [type, setType] = useState<Type | "all">("all");
  const [origin, setOrigin] = useState<Origin | "all">("all");
  const [kind, setKind] = useState<string>("all");
  const [sphere, setSphere] = useState<string>("all");
  const [query, setQuery] = useState("");
  const [selected, setSelected] = useState<Set<string>>(new Set());

  const open = useMemo(() => waiting(inbox.data), [inbox.data]);
  const needle = query.trim().toLowerCase();
  const visible = open.filter(
    (e) =>
      (type === "all" || typeOf(e) === type) &&
      (origin === "all" || originOf(e).origin === origin) &&
      (kind === "all" || kindOf(e) === kind) &&
      (sphere === "all" || (sphereOf(e) ?? "none") === sphere) &&
      (!needle || JSON.stringify(e.content).toLowerCase().includes(needle)),
  );
  const current = visible.find((e) => e.item.id === id) ?? (wide ? visible[0] : undefined);
  const chosen = visible.filter((e) => selected.has(e.item.id)).map((e) => e.item.id);
  const filtered = type !== "all" || origin !== "all" || kind !== "all" || sphere !== "all" || !!needle;
  const kinds = [...new Set(open.map(kindOf).filter((k): k is string => !!k))];

  const show = (entry: InboxEntry | undefined) => navigate(entry ? `../inbox/${entry.item.id}` : "../inbox", { replace: true });
  const reset = () => (setType("all"), setOrigin("all"), setKind("all"), setSphere("all"), setQuery(""), setSelected(new Set()));

  const decide = (ids: string[], a: Act) => {
    if (ids.length === 0) return;
    // The next item takes the place of the one just decided.
    const index = visible.findIndex((e) => e.item.id === current?.item.id);
    const rest = visible.filter((e) => !ids.includes(e.item.id));
    const next = rest[Math.min(Math.max(index, 0), rest.length - 1)];
    setSelected(new Set());
    show(next);
    const done = act.mutateAsync({ ids, act: a });
    done.catch((err: Error) => toast.error(t("inbox.failed", { error: err.message })));
    const key: Key = a.action === "accept" ? "inbox.done.accept" : a.action === "reject" ? "inbox.done.keep" : a.action === "defer" ? "inbox.done.defer" : "inbox.done.other";
    const undoable = ["accept", "reject", "defer"].includes(a.action);
    toast(t(key, { n: ids.length }), {
      duration: undoable ? 8000 : 4000,
      action: undoable
        ? {
            label: t("inbox.undo"),
            // An undo waits for the decision to reach the server.
            onClick: () =>
              done
                .then(() => undo.mutateAsync(ids))
                .then(() => toast(t("inbox.undone")))
                .catch((err: Error) => toast.error(t("inbox.failed", { error: err.message }))),
          }
        : undefined,
    });
  };

  const toggle = (itemId: string) =>
    setSelected((s) => {
      const next = new Set(s);
      if (next.has(itemId)) next.delete(itemId);
      else next.add(itemId);
      return next;
    });

  const move = (step: number) => {
    const index = visible.findIndex((e) => e.item.id === current?.item.id);
    const entry = visible[Math.min(visible.length - 1, Math.max(0, index + step))];
    if (entry) show(entry);
  };
  const targets = () => (chosen.length ? chosen : current ? [current.item.id] : []);
  useKeys(
    useMemo(
      () => ({
        j: () => move(1),
        k: () => move(-1),
        x: () => current && toggle(current.item.id),
        a: () => current?.item.kind === "proposal" && decide(targets(), { action: "accept" }),
        o: () => current?.item.kind === "proposal" && decide(targets(), { action: "reject" }),
        s: () => current?.item.kind === "proposal" && decide(targets(), { action: "defer" }),
      }),
      [visible, current, chosen],
    ),
  );

  const counts = new Map<Type, number>();
  for (const e of open) counts.set(typeOf(e), (counts.get(typeOf(e)) ?? 0) + 1);
  const allChosen = visible.length > 0 && chosen.length === visible.length;
  const proposalsChosen = chosen.length > 0 && visible.filter((e) => chosen.includes(e.item.id)).every((e) => e.item.kind === "proposal");

  const detail = current && <Detail key={current.item.id} entry={current} busy={act.isPending} onAct={(a) => decide([current.item.id], a)} />;

  return (
    <div className="flex min-h-0 flex-1">
      <section className="flex min-w-0 flex-1 flex-col gap-3 px-4 py-5 md:px-6">
        <div className="flex flex-wrap items-center gap-2">
          <label className="flex h-9 min-w-48 flex-1 items-center gap-2 rounded-md border bg-card px-2.5 focus-within:ring-2 focus-within:ring-ring/40">
            <Search className="size-4 text-muted-foreground" aria-hidden />
            <input
              value={query}
              onChange={(e) => setQuery(e.target.value)}
              placeholder={t("inbox.search")}
              aria-label={t("inbox.search")}
              className="min-w-0 flex-1 bg-transparent text-sm outline-none"
            />
          </label>
          <Select value={origin} onValueChange={(v) => setOrigin(v as Origin | "all")}>
            <SelectTrigger size="sm" className="w-auto min-w-36" aria-label={t("inbox.filter.origin")}>
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value="all">{t("inbox.filter.anyOrigin")}</SelectItem>
              {ORIGINS.map((o) => (
                <SelectItem key={o} value={o}>
                  {t(`origin.${o}` as Key)}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
          {kinds.length > 1 && (
            <Select value={kind} onValueChange={setKind}>
              <SelectTrigger size="sm" className="w-auto min-w-36" aria-label={t("inbox.filter.kind")}>
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value="all">{t("inbox.filter.anyKind")}</SelectItem>
                {kinds.map((k) => (
                  <SelectItem key={k} value={k}>
                    <span className={cn("size-2 rounded-full", kindTone(k).dot)} />
                    {t(`kind.${k}` as Key)}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          )}
          <SphereFilter value={sphere} onChange={setSphere} />
        </div>

        <div className="flex flex-wrap items-center gap-1.5">
          <Chip active={type === "all"} onClick={() => setType("all")}>
            {t("inbox.filter.all")} <span className="opacity-60">{open.length}</span>
          </Chip>
          {TYPES.filter((ty) => counts.get(ty)).map((ty) => (
            <Chip key={ty} active={type === ty} onClick={() => setType(ty)}>
              <span className={cn("size-2 rounded-full", TONE[ty].dot)} />
              {t(typeKey(ty))} <span className="opacity-60">{counts.get(ty)}</span>
            </Chip>
          ))}
          {filtered && (
            <button type="button" onClick={reset} className="inline-flex items-center gap-1 px-2 text-xs text-muted-foreground hover:text-foreground">
              <X className="size-3.5" /> {t("inbox.filter.reset")}
            </button>
          )}
          <span className="hidden flex-1 text-end text-xs text-muted-foreground xl:block">{t("inbox.keys")}</span>
        </div>

        {chosen.length > 0 && (
          // Floats over the list, so choosing never moves the rows under the pointer.
          <div className="fixed inset-x-0 bottom-20 z-30 mx-auto flex w-fit max-w-[calc(100%-2rem)] flex-wrap items-center gap-2 rounded-xl border bg-popover px-3 py-2 shadow-xl md:bottom-6">
            <span className="px-1 text-sm font-medium">{t("inbox.selected", { n: chosen.length })}</span>
            {proposalsChosen && (
              <>
                <Button size="sm" disabled={act.isPending} onClick={() => decide(chosen, { action: "accept" })}>
                  {t("inbox.accept")}
                </Button>
                <Button size="sm" variant="outline" disabled={act.isPending} onClick={() => decide(chosen, { action: "reject" })}>
                  {t("inbox.keep")}
                </Button>
                <Button size="sm" variant="outline" disabled={act.isPending} onClick={() => decide(chosen, { action: "defer" })}>
                  {t("inbox.defer")}
                </Button>
              </>
            )}
            <Button size="sm" variant="ghost" onClick={() => setSelected(new Set())}>
              {t("inbox.clear")}
            </Button>
          </div>
        )}

        {inbox.isPending ? (
          <div className="flex flex-col gap-2">
            {[0, 1, 2, 3].map((i) => (
              <Skeleton key={i} className="h-14" />
            ))}
          </div>
        ) : visible.length === 0 ? (
          <div className="flex flex-col items-center gap-2 rounded-xl border border-dashed px-6 py-12 text-center">
            <span className="text-base font-semibold">{filtered ? t("inbox.noMatch") : t("inbox.empty.title")}</span>
            <span className="text-sm text-muted-foreground">{filtered ? t("inbox.noMatchText") : t("inbox.empty.text")}</span>
          </div>
        ) : (
          <div className="overflow-hidden rounded-xl border bg-card">
            <div className="flex items-center gap-2.5 border-b bg-muted/40 px-3.5 py-2">
              <CheckBox checked={allChosen} label={t("inbox.selectAll")} onClick={() => setSelected(allChosen ? new Set() : new Set(visible.map((e) => e.item.id)))} />
              <span className="text-xs text-muted-foreground">{filtered ? t("inbox.shown", { n: visible.length, total: open.length }) : t("inbox.selectAll")}</span>
            </div>
            {visible.map((entry) => {
              const ty = typeOf(entry);
              const k = kindOf(entry);
              const { title, description } = summary(entry);
              const active = entry.item.id === current?.item.id;
              return (
                <div
                  key={entry.item.id}
                  className={cn(
                    "group flex items-start gap-3 border-b px-3.5 py-2.5 last:border-b-0",
                    active ? "bg-accent shadow-[inset_2px_0_0_var(--primary)] rtl:shadow-[inset_-2px_0_0_var(--primary)]" : "hover:bg-accent/40",
                  )}
                >
                  <CheckBox checked={selected.has(entry.item.id)} label={title} onClick={() => toggle(entry.item.id)} />
                  <button type="button" onClick={() => show(entry)} className="flex min-w-0 flex-1 flex-col gap-1 text-start">
                    <span className={cn("line-clamp-1 text-sm leading-snug", !entry.seen && "font-medium")}>{title}</span>
                    {description && <span className="line-clamp-1 text-[13px] leading-snug text-muted-foreground">{description}</span>}
                    <span className="mt-0.5 flex flex-wrap items-center gap-x-1.5 gap-y-1 text-xs text-muted-foreground">
                      {k && <KindBadge kind={k} />}
                      <SphereBadge sphere={sphereOf(entry)} />
                      {(!k || ty !== "new") && <TypeBadge type={ty} />}
                      <span className="ms-0.5">{originLabel(entry)}</span>
                    </span>
                  </button>
                  <span className="shrink-0 pt-0.5 text-xs text-muted-foreground">{ago(entry.item.created_at)}</span>
                </div>
              );
            })}
          </div>
        )}
      </section>

      {wide ? (
        <aside className="sticky top-14 flex max-h-[calc(100dvh-3.5rem)] w-[clamp(320px,38%,520px)] shrink-0 flex-col overflow-y-auto border-s bg-card">
          {detail ?? <p className="p-6 text-sm text-muted-foreground">{t("inbox.pick")}</p>}
        </aside>
      ) : (
        <Sheet open={!!current} onOpenChange={(o) => !o && show(undefined)}>
          <SheetContent side="bottom" className="max-h-[88dvh] overflow-y-auto p-0">
            <SheetTitle className="sr-only">{current ? headline(current) : ""}</SheetTitle>
            {detail}
          </SheetContent>
        </Sheet>
      )}
    </div>
  );
}

function Chip({ active, onClick, children }: { active: boolean; onClick: () => void; children: React.ReactNode }) {
  return (
    <button
      type="button"
      onClick={onClick}
      aria-pressed={active}
      className={cn(
        "inline-flex h-8 items-center gap-1.5 rounded-full border px-3 text-[13px] transition-colors",
        active ? "border-foreground bg-foreground text-background" : "bg-card hover:bg-accent",
      )}
    >
      {children}
    </button>
  );
}

function CheckBox({ checked, label, onClick }: { checked: boolean; label: string; onClick: () => void }) {
  return (
    <button
      type="button"
      role="checkbox"
      aria-checked={checked}
      aria-label={label}
      onClick={onClick}
      className={cn(
        "mt-0.5 grid size-5 shrink-0 place-items-center rounded-[5px] border transition-colors",
        checked ? "border-primary bg-primary text-primary-foreground" : "bg-background text-transparent hover:border-foreground/40",
      )}
    >
      <Check className="size-3.5" strokeWidth={3} />
    </button>
  );
}

function ChangeBoxes({ change, title }: { change: Change; title: string }) {
  const { t } = useI18n();
  if (change.sphere_before !== undefined) {
    // Only the sphere moves.
    return (
      <div className="flex flex-wrap items-center gap-2 rounded-lg border border-primary/50 bg-accent p-3 text-sm">
        {change.sphere_before ? <SphereBadge sphere={change.sphere_before} /> : <span className="text-xs text-muted-foreground">{t("sphere.none")}</span>}
        <ArrowRight className="size-3.5 text-muted-foreground rtl:rotate-180" aria-hidden />
        {change.sphere ? <SphereBadge sphere={change.sphere} /> : <span className="text-xs text-muted-foreground">{t("sphere.none")}</span>}
      </div>
    );
  }
  if (!change.before) {
    // New knowledge: the text under the title, unless the title already says it all.
    return change.after && change.after.trim() !== title ? <p className="text-sm leading-relaxed text-foreground/85">{change.after}</p> : null;
  }
  return (
    <div className="grid grid-cols-1 gap-2">
      <div className="flex flex-col gap-1 rounded-lg border p-3">
        <span className="text-[11px] font-medium tracking-wide text-muted-foreground uppercase">{t("inbox.inMemory")}</span>
        <span className="text-sm text-muted-foreground">{change.before}</span>
      </div>
      <div className="flex flex-col gap-1 rounded-lg border border-primary/50 bg-accent p-3">
        <span className="text-[11px] font-medium tracking-wide text-muted-foreground uppercase">{t("inbox.proposed")}</span>
        <span className={cn("text-sm font-medium", !change.after && "italic text-muted-foreground")}>{change.after ?? t("inbox.removed")}</span>
      </div>
    </div>
  );
}

function host(uri: string) {
  try {
    return new URL(uri).host;
  } catch {
    return "";
  }
}

/** Long text folded to a few lines, open on demand. */
function Fold({ text }: { text: string }) {
  const { t } = useI18n();
  const [open, setOpen] = useState(false);
  const long = text.length > 220;
  return (
    <div className="flex flex-col items-start gap-1">
      <p className={cn("text-sm whitespace-pre-line text-muted-foreground", !open && long && "line-clamp-3")}>{text}</p>
      {long && (
        <button type="button" onClick={() => setOpen(!open)} className="inline-flex items-center gap-1 text-xs text-primary">
          {open ? t("inbox.less") : t("inbox.more")} <ChevronDown className={cn("size-3", open && "rotate-180")} />
        </button>
      )}
    </div>
  );
}

function Detail({ entry, busy, onAct }: { entry: InboxEntry; busy: boolean; onAct: (act: Act) => void }) {
  const { t, ago } = useI18n();
  const originLabel = useOriginLabel();
  const [answer, setAnswer] = useState("");
  const p = proposal(entry);
  const q = entry.item.kind === "question" ? (entry.content as QuestionContent) : null;
  const has = (action: string) => entry.actions.some((a) => a.action === action);
  const ty = typeOf(entry);
  const k = kindOf(entry);
  const { title, description } = summary(entry);
  const several = (p?.alternatives.length ?? 0) > 1;
  // The person may put the knowledge in another sphere as they accept it.
  const proposed = sphereOf(entry);
  const [sphere, setSphere] = useState<string | null>(proposed);
  const movable = !!p && has("accept") && !!k && p.alternatives[0]?.changes[0]?.sphere_before === undefined;
  const into = sphere !== proposed ? { sphere: sphere ?? "none" } : {};

  return (
    <div className="flex min-h-full flex-col">
      <div className="flex flex-1 flex-col gap-4 p-5">
        <div className="flex flex-col gap-2">
          <div className="flex flex-wrap items-center gap-1.5 text-xs text-muted-foreground">
            {k && <KindBadge kind={k} />}
            {movable ? <SpherePicker value={sphere} onChange={setSphere} disabled={busy} /> : <SphereBadge sphere={proposed} />}
            <TypeBadge type={ty} />
            {entry.item.urgency === "now" && <span className="font-medium text-warning">{t("inbox.urgent")}</span>}
          </div>
          <h2 className="text-[15px] leading-snug font-semibold">{title}</h2>
          <span className="text-xs text-muted-foreground">
            {originLabel(entry)} · {ago(entry.item.created_at)}
          </span>
          {!p && description && <p className="text-sm leading-relaxed text-foreground/85">{description}</p>}
        </div>

        {p?.alternatives.map((alternative) => (
          <div key={alternative.key} className="flex flex-col gap-2">
            {several && <span className="text-sm font-medium">{alternative.label ?? alternative.key}</span>}
            {alternative.changes.map((change, i) => (
              <ChangeBoxes key={i} change={change} title={title} />
            ))}
            {several && (
              <Button size="sm" className="self-start" disabled={busy} onClick={() => onAct({ action: "accept", alternative: alternative.key, ...into })}>
                {t("inbox.acceptOption", { option: alternative.label ?? alternative.key })}
              </Button>
            )}
          </div>
        ))}

        {(p?.quote || p?.origin) && (
          <div className="flex flex-col gap-1.5">
            <span className="text-[11px] font-medium tracking-wide text-muted-foreground uppercase">{t("inbox.from")}</span>
            {p.quote && <blockquote className="border-s-2 border-primary/50 ps-3 text-sm italic">{p.quote}</blockquote>}
            {p.origin && (
              <a
                href={p.origin.uri}
                target="_blank"
                rel="noopener noreferrer"
                className="inline-flex max-w-full items-center gap-1.5 self-start rounded-md border px-2.5 py-1.5 text-sm hover:bg-accent"
              >
                <ExternalLink className="size-3.5 shrink-0 text-muted-foreground" aria-hidden />
                <span className="truncate">{p.origin.title || t("inbox.openOriginal")}</span>
                <span className="shrink-0 text-xs text-muted-foreground" dir="ltr">
                  {host(p.origin.uri)}
                </span>
              </a>
            )}
          </div>
        )}
        {p?.rationale && (
          <div className="flex flex-col gap-1.5">
            <span className="text-[11px] font-medium tracking-wide text-muted-foreground uppercase">{t("inbox.why")}</span>
            <Fold text={p.rationale.replace(/<\/?\w+[^>]*>/g, " ").trim()} />
          </div>
        )}

        {q && (
          <div className="flex flex-col gap-3">
            {q.context && <Fold text={q.context} />}
            <div className="flex flex-wrap gap-2">
              {(q.options ?? []).map((option) => (
                <Button key={option} size="sm" variant="outline" disabled={busy} onClick={() => onAct({ action: "answer", answer: option })}>
                  {option}
                </Button>
              ))}
            </div>
            <form
              className="flex gap-2"
              onSubmit={(e) => {
                e.preventDefault();
                if (answer.trim()) onAct({ action: "answer", answer: answer.trim() });
              }}
            >
              <Input value={answer} onChange={(e) => setAnswer(e.target.value)} placeholder={t("inbox.answerPlaceholder")} aria-label={t("inbox.answerPlaceholder")} />
              <Button type="submit" disabled={busy || !answer.trim()}>
                {t("inbox.answer")}
              </Button>
            </form>
          </div>
        )}
      </div>

      <div className="sticky bottom-0 flex flex-col gap-2 border-t bg-card/95 p-4 backdrop-blur">
        <div className="flex flex-wrap gap-2">
          {p && !several && has("accept") && (
            <Button disabled={busy} onClick={() => onAct({ action: "accept", ...into })}>
              {t("inbox.accept")} <kbd dir="ltr" className="font-mono text-[11px] opacity-70">A</kbd>
            </Button>
          )}
          {has("reject") && (
            <Button variant="outline" disabled={busy} onClick={() => onAct({ action: "reject" })}>
              {t("inbox.keep")} <kbd dir="ltr" className="font-mono text-[11px] text-muted-foreground">O</kbd>
            </Button>
          )}
          {has("defer") && (
            <Button variant="outline" disabled={busy} onClick={() => onAct({ action: "defer" })}>
              {t("inbox.defer")} <kbd dir="ltr" className="font-mono text-[11px] text-muted-foreground">S</kbd>
            </Button>
          )}
          {(
            [
              ["snooze", "inbox.snooze"],
              ["done", "inbox.done"],
              ["postpone", "inbox.postpone"],
              ["retry", "inbox.retry"],
              ["dismiss", "inbox.dismiss"],
              ["seen", "inbox.seen"],
            ] as const
          )
            .filter(([action]) => has(action))
            .map(([action, label]) => (
              <Button key={action} variant="outline" disabled={busy} onClick={() => onAct({ action })}>
                {t(label)}
              </Button>
            ))}
        </div>
        {p && <p className="text-xs text-muted-foreground">{t("inbox.recordedAs")}</p>}
      </div>
    </div>
  );
}
