import { useMutation, useQueryClient } from "@tanstack/react-query";
import { Check } from "lucide-react";
import { useMemo, useState } from "react";
import { useNavigate, useParams } from "react-router";
import { toast } from "sonner";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Sheet, SheetContent, SheetTitle } from "@/components/ui/sheet";
import { Skeleton } from "@/components/ui/skeleton";
import { useI18n } from "@/i18n";
import type { Key } from "@/i18n/en";
import { api } from "@/lib/frood";
import { useKeys, useMediaQuery } from "@/lib/hooks";
import { useInbox, waiting } from "@/lib/queries";
import type { Change, InboxEntry, ProposalContent, QuestionContent } from "@/lib/types";
import { cn } from "@/lib/utils";

type Filter = "all" | "agents" | "people" | "questions" | "other";
const FILTERS: [Filter, Key][] = [
  ["all", "inbox.filter.all"],
  ["agents", "inbox.filter.agents"],
  ["people", "inbox.filter.people"],
  ["questions", "inbox.filter.questions"],
  ["other", "inbox.filter.other"],
];

type Act = { action: string; alternative?: string; answer?: string };

function group(entry: InboxEntry): Exclude<Filter, "all"> {
  const { kind } = entry.item;
  if (kind === "question") return "questions";
  if (kind !== "proposal") return "other";
  const author = (entry.content as Partial<ProposalContent>).author ?? "";
  return author.startsWith("user:") ? "people" : "agents";
}

function proposal(entry: InboxEntry): ProposalContent | null {
  return entry.item.kind === "proposal" ? (entry.content as ProposalContent) : null;
}

/** The line a person reads first. */
function headline(entry: InboxEntry): string {
  const p = proposal(entry);
  const change = p?.alternatives[0]?.changes[0];
  if (change) return change.after ?? change.before ?? "";
  const content = entry.content as Record<string, unknown>;
  const data = entry.item.data;
  for (const value of [content.text, data.text, data.title, data.message, data.error]) {
    if (typeof value === "string" && value) return value;
  }
  return entry.item.subject;
}

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

export function Inbox() {
  const { t, ago } = useI18n();
  const { id } = useParams();
  const navigate = useNavigate();
  const wide = useMediaQuery("(min-width: 1024px)");
  const inbox = useInbox();
  const act = useAct();
  const undo = useUndo();
  const [filter, setFilter] = useState<Filter>("all");
  const [selected, setSelected] = useState<Set<string>>(new Set());

  const open = useMemo(() => waiting(inbox.data), [inbox.data]);
  const visible = open.filter((e) => filter === "all" || group(e) === filter);
  const current = visible.find((e) => e.item.id === id) ?? (wide ? visible[0] : undefined);
  const chosen = visible.filter((e) => selected.has(e.item.id)).map((e) => e.item.id);

  const show = (entry: InboxEntry | undefined) => navigate(entry ? `../inbox/${entry.item.id}` : "../inbox", { replace: true });

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

  const counts = Object.fromEntries(FILTERS.map(([f]) => [f, f === "all" ? open.length : open.filter((e) => group(e) === f).length]));
  const allChosen = visible.length > 0 && chosen.length === visible.length;
  const proposalsChosen = chosen.length > 0 && visible.filter((e) => chosen.includes(e.item.id)).every((e) => e.item.kind === "proposal");

  const detail = current && <Detail entry={current} busy={act.isPending} onAct={(a) => decide([current.item.id], a)} />;

  return (
    <div className="flex min-h-0 flex-1">
      <section className="flex min-w-0 flex-1 flex-col gap-3.5 px-4 py-5 md:px-6">
        <div className="flex flex-wrap items-center gap-2">
          {FILTERS.filter(([f]) => f === "all" || counts[f] > 0).map(([f, label]) => (
            <Button key={f} size="sm" variant={filter === f ? "default" : "outline"} className="rounded-full" onClick={() => (setFilter(f), setSelected(new Set()))}>
              {t(label)} <span className="opacity-70">{counts[f]}</span>
            </Button>
          ))}
          <span className="hidden flex-1 text-end text-xs text-muted-foreground lg:block">{t("inbox.keys")}</span>
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
              <Skeleton key={i} className="h-16" />
            ))}
          </div>
        ) : visible.length === 0 ? (
          <div className="flex flex-col items-center gap-2 rounded-xl border border-dashed px-6 py-12 text-center">
            <span className="text-base font-semibold">{t("inbox.empty.title")}</span>
            <span className="text-sm text-muted-foreground">{t("inbox.empty.text")}</span>
          </div>
        ) : (
          <div className="overflow-hidden rounded-xl border bg-card">
            <div className="flex items-center gap-2.5 border-b bg-muted/50 px-3.5 py-2">
              <CheckBox checked={allChosen} label={t("inbox.selectAll")} onClick={() => setSelected(allChosen ? new Set() : new Set(visible.map((e) => e.item.id)))} />
              <span className="text-xs text-muted-foreground">{t("inbox.selectAll")}</span>
            </div>
            {visible.map((entry) => {
              const p = proposal(entry);
              const change = p?.alternatives[0]?.changes[0];
              const active = entry.item.id === current?.item.id;
              return (
                <div
                  key={entry.item.id}
                  className={cn("flex items-start gap-3 border-b px-3.5 py-3 last:border-b-0", active && "bg-accent shadow-[inset_2px_0_0_var(--primary)] rtl:shadow-[inset_-2px_0_0_var(--primary)]")}
                >
                  <CheckBox checked={selected.has(entry.item.id)} label={headline(entry)} onClick={() => toggle(entry.item.id)} />
                  <button type="button" onClick={() => show(entry)} className="flex min-w-0 flex-1 flex-col gap-1 text-start">
                    <span className={cn("line-clamp-2 text-sm", !entry.seen && "font-medium")}>{headline(entry)}</span>
                    {change?.before && change.after && (
                      <span className="line-clamp-1 text-[13px] text-muted-foreground">
                        <span className="line-through">{change.before}</span>
                      </span>
                    )}
                  </button>
                  <span className="flex shrink-0 flex-col items-end gap-1">
                    <Badge variant="secondary" className="font-normal">
                      {t(`inbox.kind.${entry.item.kind}` as Key)}
                    </Badge>
                    <span className="text-xs text-muted-foreground">{ago(entry.item.created_at)}</span>
                  </span>
                </div>
              );
            })}
          </div>
        )}
      </section>

      {wide ? (
        <aside className="sticky top-14 flex max-h-[calc(100dvh-3.5rem)] w-[clamp(320px,36%,480px)] shrink-0 flex-col overflow-y-auto border-s bg-card px-6 py-5">
          {detail ?? <p className="text-sm text-muted-foreground">{t("inbox.pick")}</p>}
        </aside>
      ) : (
        <Sheet open={!!current} onOpenChange={(o) => !o && show(undefined)}>
          <SheetContent side="bottom" className="max-h-[85dvh] overflow-y-auto p-5">
            <SheetTitle className="sr-only">{current ? headline(current) : ""}</SheetTitle>
            {detail}
          </SheetContent>
        </Sheet>
      )}
    </div>
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

function ChangeBoxes({ change }: { change: Change }) {
  const { t } = useI18n();
  return (
    <div className="grid grid-cols-1 gap-2.5 sm:grid-cols-2">
      <div className="flex flex-col gap-1 rounded-lg border p-3">
        <span className="text-xs text-muted-foreground">{t("inbox.inMemory")}</span>
        <span className={cn("text-sm", !change.before && "text-muted-foreground italic")}>{change.before ?? t("inbox.nothingYet")}</span>
      </div>
      <div className="flex flex-col gap-1 rounded-lg border border-primary bg-accent p-3">
        <span className="text-xs text-muted-foreground">{t("inbox.proposed")}</span>
        <span className={cn("text-sm font-medium", !change.after && "text-muted-foreground italic")}>{change.after ?? t("inbox.removed")}</span>
      </div>
    </div>
  );
}

function author(who: string, t: (key: Key) => string): string {
  if (who.startsWith("agent:")) return t("inbox.by.agent");
  if (who.startsWith("source:")) return t("inbox.by.source");
  return t("inbox.by.person");
}

function Detail({ entry, busy, onAct }: { entry: InboxEntry; busy: boolean; onAct: (act: Act) => void }) {
  const { t, ago } = useI18n();
  const [answer, setAnswer] = useState("");
  const p = proposal(entry);
  const q = entry.item.kind === "question" ? (entry.content as QuestionContent) : null;
  const has = (action: string) => entry.actions.some((a) => a.action === action);

  return (
    <div className="flex flex-col gap-4">
      <div className="flex flex-col gap-1">
        <span className="text-xs text-muted-foreground">
          {t(`inbox.kind.${entry.item.kind}` as Key)} · {ago(entry.item.created_at)}
          {p && <> · {t("inbox.by", { who: author(p.author, t) })}</>}
          {entry.item.urgency === "high" && <> · <span className="text-warning">{t("inbox.urgent")}</span></>}
        </span>
        <h2 className="text-lg font-semibold tracking-tight">{headline(entry)}</h2>
      </div>

      {p?.alternatives.map((alternative) => (
        <div key={alternative.key} className="flex flex-col gap-2.5">
          {p.alternatives.length > 1 && <span className="text-sm font-medium">{alternative.label ?? alternative.key}</span>}
          {alternative.changes.map((change, i) => (
            <ChangeBoxes key={i} change={change} />
          ))}
          {p.alternatives.length > 1 && (
            <Button size="sm" className="self-start" disabled={busy} onClick={() => onAct({ action: "accept", alternative: alternative.key })}>
              {t("inbox.acceptOption", { option: alternative.label ?? alternative.key })}
            </Button>
          )}
        </div>
      ))}

      {p?.rationale && (
        <div className="flex flex-col gap-1.5">
          <span className="text-xs text-muted-foreground">{t("inbox.why")}</span>
          <p className="text-sm">{p.rationale}</p>
        </div>
      )}
      {p?.quote && (
        <div className="flex flex-col gap-1.5">
          <span className="text-xs text-muted-foreground">{t("inbox.from")}</span>
          <blockquote className="rounded-md bg-muted px-3 py-2.5 text-sm">{p.quote}</blockquote>
        </div>
      )}

      {q && (
        <div className="flex flex-col gap-3">
          {q.context && <p className="text-sm text-muted-foreground">{q.context}</p>}
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

      <div className="flex flex-wrap gap-2">
        {p && p.alternatives.length === 1 && has("accept") && (
          <Button disabled={busy} onClick={() => onAct({ action: "accept" })}>
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
  );
}
