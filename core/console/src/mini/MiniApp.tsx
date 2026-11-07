// The Telegram Mini App (docs/32): the console's mobile view inside Telegram. The digest and
// the Inbox, one item at a time with swipes between them, search, and a knowledge card with its
// sources and history (QM1 b). It reads, writes and decides; the installation's settings stay
// in the console (QM2 a).

import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { ChevronLeft, ExternalLink, Search } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import { createBrowserRouter, Outlet, RouterProvider, useLocation, useNavigate, useParams, useSearchParams } from "react-router";
import { toast } from "sonner";
import { KindBadge, SphereBadge, UnverifiedBadge } from "@/components/badges";
import { Button } from "@/components/ui/button";
import { Skeleton } from "@/components/ui/skeleton";
import { useI18n } from "@/i18n";
import { en, type Key } from "@/i18n/en";
import { api, BASE, telegramSignIn } from "@/lib/frood";
import { useInbox, waiting } from "@/lib/queries";
import { headline, proposal, TONE, titled, typeOf } from "@/lib/situations";
import type { Assertion, InboxEntry, SearchHit } from "@/lib/types";
import { cn } from "@/lib/utils";
import { type Act, Detail, useAct, useUndo } from "@/pages/Inbox";
import { History } from "@/pages/Knowledge";
import { feel, startTelegram, tg } from "./telegram";

const router = createBrowserRouter(
  [
    {
      element: <Layout />,
      children: [
        { index: true, element: <Home /> },
        { path: "inbox/:id", element: <Card /> },
        { path: "search", element: <SearchPage /> },
        { path: "knowledge/:id", element: <KnowledgeCard /> },
        { path: "*", element: <Home /> },
      ],
    },
  ],
  { basename: `${BASE}/admin/m` },
);

export default function MiniApp() {
  const { t } = useI18n();
  const [state, setState] = useState<"loading" | "ready" | { error: string }>("loading");

  useEffect(() => {
    const app = tg();
    if (app) startTelegram(app);
    if (import.meta.env.VITE_FROOD_TOKEN || sessionStorage.getItem("mini_tokens")) {
      setState("ready");
      return;
    }
    if (!app) {
      setState({ error: t("mini.notInTelegram") });
      return;
    }
    telegramSignIn()
      .then(() => setState("ready"))
      .catch((err: Error) =>
        setState({
          error: err.message.includes("not linked") ? t("mini.notLinked") : t("mini.signInFailed", { error: err.message }),
        }),
      );
  }, []);

  if (state === "loading") return <p className="p-6 text-sm text-muted-foreground">{t("app.signingIn")}</p>;
  if (state !== "ready") return <p className="p-6 text-[15px] leading-relaxed">{state.error}</p>;
  return <RouterProvider router={router} />;
}

/** The order the Inbox is gone through: what is urgent first, then the newest. */
function queue(entries: InboxEntry[] | undefined): InboxEntry[] {
  return waiting(entries)
    .filter((e) => e.item.kind !== "digest" && e.item.kind !== "sign_in")
    .sort((a, b) => Number(b.item.urgency === "now") - Number(a.item.urgency === "now") || b.item.created_at.localeCompare(a.item.created_at));
}

/** Telegram's back button off the first screen; in a plain browser, a link at the top. */
function Layout() {
  const { t } = useI18n();
  const location = useLocation();
  const navigate = useNavigate();
  const home = location.pathname === "/";
  const back = () => ((window.history.state as { idx?: number } | null)?.idx ? navigate(-1) : navigate("/", { replace: true }));
  const backRef = useRef(back);
  backRef.current = back;

  // A new screen starts at its top.
  useEffect(() => {
    // Braces matter: newer browsers return a promise from scrollTo, which is no cleanup.
    window.scrollTo(0, 0);
  }, [location.pathname]);

  useEffect(() => {
    const button = tg()?.BackButton;
    if (!button) return;
    if (home) {
      button.hide();
      return;
    }
    const click = () => backRef.current();
    button.show();
    button.onClick(click);
    return () => button.offClick(click);
  }, [home]);

  return (
    <main className="mx-auto flex min-h-dvh max-w-xl flex-col gap-4 px-4 py-4 pb-10">
      {!home && !tg() && (
        <button type="button" onClick={back} className="inline-flex items-center gap-1 self-start text-sm text-primary">
          <ChevronLeft className="size-4 rtl:rotate-180" /> {t("mini.back")}
        </button>
      )}
      <Outlet />
    </main>
  );
}

/** Telegram's main button for the screen's main action; hidden when there is none. */
function useMainButton(text: string | null, onClick: () => void, busy = false) {
  const ref = useRef(onClick);
  ref.current = onClick;
  useEffect(() => {
    const button = tg()?.MainButton;
    if (!button) return;
    if (!text) {
      button.hide();
      return;
    }
    const click = () => ref.current();
    button.setText(text);
    button.enable();
    button.show();
    button.onClick(click);
    return () => {
      button.offClick(click);
      button.hide();
    };
  }, [text]);
  useEffect(() => {
    const button = tg()?.MainButton;
    if (!button || !text) return;
    if (busy) button.showProgress(false);
    else button.hideProgress();
  }, [busy, text]);
}

function SearchField({ value, onChange, autoFocus }: { value: string; onChange?: (v: string) => void; autoFocus?: boolean }) {
  const { t } = useI18n();
  const navigate = useNavigate();
  return (
    <label className="flex min-h-11 items-center gap-2.5 rounded-xl border bg-card px-3 focus-within:ring-2 focus-within:ring-ring/40">
      <Search className="size-4 text-muted-foreground" aria-hidden />
      <input
        type="search"
        value={value}
        autoFocus={autoFocus}
        readOnly={!onChange}
        onFocus={() => !onChange && navigate("/search")}
        onChange={(e) => onChange?.(e.target.value)}
        placeholder={t("knowledge.search")}
        aria-label={t("knowledge.search")}
        // 16 px, or iOS zooms the page on focus.
        className="min-w-0 flex-1 bg-transparent text-base outline-none"
      />
    </label>
  );
}

function Home() {
  const { t } = useI18n();
  const inbox = useInbox();
  const all = waiting(inbox.data);
  const digest = all.find((e) => e.item.kind === "digest");
  const items = queue(inbox.data);
  useMainButton(null, () => {});

  return (
    <>
      <SearchField value="" />
      {digest && <DigestCard entry={digest} open={new Set(all.map((e) => e.item.id))} />}
      <section className="flex flex-col gap-2">
        <h2 className="px-1 text-xs font-medium tracking-wide text-muted-foreground uppercase">
          {t("mini.waiting")} {items.length > 0 && `· ${items.length}`}
        </h2>
        {inbox.isPending ? (
          [0, 1, 2].map((i) => <Skeleton key={i} className="h-14 rounded-xl" />)
        ) : items.length === 0 ? (
          <div className="rounded-xl border bg-card p-4">
            <p className="text-[15px] font-medium">{t("inbox.empty.title")}</p>
            <p className="text-sm text-muted-foreground">{t("inbox.empty.text")}</p>
          </div>
        ) : (
          <ul className="overflow-hidden rounded-xl border bg-card">
            {items.map((e) => (
              <InboxRow key={e.item.id} entry={e} />
            ))}
          </ul>
        )}
      </section>
    </>
  );
}

function host(uri: string) {
  try {
    return new URL(uri).host;
  } catch {
    return "";
  }
}

function InboxRow({ entry }: { entry: InboxEntry }) {
  const { t, ago } = useI18n();
  const navigate = useNavigate();
  const origin = proposal(entry)?.origin;
  return (
    <li className="border-b last:border-b-0">
      <button
        type="button"
        onClick={() => navigate(`/inbox/${entry.item.id}`)}
        className="flex w-full items-start gap-3 px-3.5 py-3 text-start active:bg-accent"
      >
        <span className={cn("mt-1.5 size-2 shrink-0 rounded-full", TONE[typeOf(entry)].dot)} aria-hidden />
        <span className="flex min-w-0 flex-1 flex-col gap-0.5">
          <span className="line-clamp-2 text-[15px] leading-snug">{headline(entry)}</span>
          <span className="truncate text-xs text-muted-foreground">
            {entry.item.urgency === "now" && <span className="font-medium text-warning">{t("inbox.urgent")} · </span>}
            {origin ? `${origin.title || host(origin.uri)} · ` : ""}
            {ago(entry.item.created_at)}
          </span>
        </span>
      </button>
    </li>
  );
}

/** What went into memory by itself is folded after a few lines: the rest of the digest matters more. */
const FOLDED = 4;

type Line = {
  id?: string;
  ids?: string[];
  kind?: string;
  why?: string;
  title: string;
  label?: string | null;
  link?: string | null;
  due?: string;
};
type Digest = {
  important?: Line[];
  accepted?: Line[];
  due_soon?: Line[];
  accept_ids?: string[];
};

/** The digest on a screen: what is important, what is due, what went into memory by itself. */
function DigestCard({ entry, open }: { entry: InboxEntry; open: Set<string> }) {
  const { t, ago, date } = useI18n();
  const navigate = useNavigate();
  const client = useQueryClient();
  const d = entry.content as Digest;
  const important = d.important ?? [];
  const due = d.due_soon ?? [];
  const accepted = d.accepted ?? [];
  // Findings still waiting: once accepted here or elsewhere, the button goes.
  const findings = (d.accept_ids ?? []).filter((id) => open.has(id));
  const [allAccepted, setAllAccepted] = useState(false);
  const acceptAll = useMutation({
    mutationFn: () =>
      Promise.all(
        findings.map((id) =>
          api(`/v1/inbox/${id}/act`, {
            method: "POST",
            json: { action: "accept", unverified: true },
          }),
        ),
      ),
    onSuccess: () => {
      feel("success");
      toast(t("mini.acceptedUnverified", { n: findings.length }));
    },
    onError: (err: Error) => {
      feel("error");
      toast.error(t("inbox.failed", { error: err.message }));
    },
    onSettled: () => client.invalidateQueries({ queryKey: ["inbox"] }),
  });
  if (!important.length && !due.length && !accepted.length && !findings.length) return null;
  const why = (w: string | undefined) => (w && `mini.why.${w}` in en ? t(`mini.why.${w}` as Key) : null);
  const when = (w: string | undefined) => (w && `mini.due.${w.replace("due_", "")}` in en ? t(`mini.due.${w.replace("due_", "")}` as Key) : null);

  return (
    <section className="flex flex-col gap-3 rounded-xl border bg-card p-4">
      <h2 className="text-xs font-medium tracking-wide text-muted-foreground uppercase">
        {t("mini.digest")} · {ago(entry.item.created_at)}
      </h2>
      {important.length > 0 && (
        <DigestList title={t("mini.important")}>
          {important.map((line, i) => (
            <DigestLine
              key={i}
              line={line}
              tag={why(line.why)}
              hot
              onOpen={line.kind !== "run" && line.id ? () => navigate(`/inbox/${line.id}`) : undefined}
            />
          ))}
        </DigestList>
      )}
      {due.length > 0 && (
        <DigestList title={t("mini.dueSoon")}>
          {due.map((line, i) => (
            <DigestLine
              key={i}
              line={line}
              tag={when(line.why) ?? (line.due ? date(line.due) : null)}
              hot={!!line.why}
              onOpen={line.id ? () => navigate(`/knowledge/${line.id}`) : undefined}
            />
          ))}
        </DigestList>
      )}
      {accepted.length > 0 && (
        <DigestList
          title={t("inbox.done.accept", {
            n: accepted.reduce((n, l) => n + (l.ids?.length ?? 1), 0),
          })}
        >
          {(allAccepted ? accepted : accepted.slice(0, FOLDED)).map((line, i) => (
            <DigestLine key={i} line={line} tag={null} />
          ))}
          {accepted.length > FOLDED && (
            <li>
              <button type="button" onClick={() => setAllAccepted(!allAccepted)} className="py-1 text-xs text-primary">
                {allAccepted ? t("inbox.less") : t("inbox.more")}
              </button>
            </li>
          )}
        </DigestList>
      )}
      {findings.length > 0 && (
        <Button variant="outline" className="self-start" disabled={acceptAll.isPending} onClick={() => acceptAll.mutate()}>
          {t("mini.acceptUnverified", { n: findings.length })}
        </Button>
      )}
    </section>
  );
}

function DigestList({ title, children }: { title: string; children: React.ReactNode }) {
  return (
    <div className="flex flex-col gap-1">
      <span className="text-[13px] font-medium">{title}</span>
      <ul className="flex flex-col">{children}</ul>
    </div>
  );
}

function DigestLine({ line, tag, hot, onOpen }: { line: Line; tag: string | null; hot?: boolean; onOpen?: () => void }) {
  const body = (
    <>
      <span className={cn("mt-1.5 size-1.5 shrink-0 rounded-full", hot ? "bg-rose-500" : "bg-muted-foreground/40")} aria-hidden />
      <span className="min-w-0 flex-1 text-sm leading-snug">
        {tag && <span className={cn("font-medium", hot && "text-rose-700 dark:text-rose-300")}>{tag} · </span>}
        {line.title}
        {(line.ids?.length ?? 0) > 1 && <span className="text-muted-foreground"> ×{line.ids!.length}</span>}
      </span>
    </>
  );
  return (
    <li className="flex items-start gap-2 py-1">
      {onOpen ? (
        <button type="button" onClick={onOpen} className="flex min-w-0 flex-1 items-start gap-2 text-start active:opacity-70">
          {body}
        </button>
      ) : (
        <span className="flex min-w-0 flex-1 items-start gap-2">{body}</span>
      )}
      {line.link && (
        <a href={line.link} target="_blank" rel="noopener noreferrer" className="inline-flex shrink-0 items-center gap-1 text-xs text-primary">
          {line.label || host(line.link)} <ExternalLink className="size-3" aria-hidden />
        </a>
      )}
    </li>
  );
}

/** One Inbox item with its actions; a swipe goes to the next or the previous one. */
function Card() {
  const { t } = useI18n();
  const { id } = useParams();
  const navigate = useNavigate();
  const inbox = useInbox();
  const act = useAct();
  const undo = useUndo();
  const accept = useRef<(() => void) | null>(null);
  const touch = useRef<{ x: number; y: number } | null>(null);
  const items = queue(inbox.data);
  const index = items.findIndex((e) => e.item.id === id);
  const entry = index >= 0 ? items[index] : undefined;
  const p = entry && proposal(entry);
  const single = !!p && p.alternatives.length <= 1 && !!entry?.actions.some((a) => a.action === "accept");
  const inTelegram = !!tg();

  const go = (i: number) => {
    const next = items[i];
    if (!next) return;
    feel("select");
    navigate(`/inbox/${next.item.id}`, { replace: true });
  };

  const decide = (a: Act) => {
    if (!id) return;
    const rest = items.filter((e) => e.item.id !== id);
    const next = rest[Math.min(Math.max(index, 0), rest.length - 1)];
    const done = act.mutateAsync({ ids: [id], act: a });
    done.then(() => feel("success")).catch((err: Error) => (feel("error"), toast.error(t("inbox.failed", { error: err.message }))));
    const key: Key =
      a.action === "accept"
        ? "inbox.done.accept"
        : a.action === "reject"
          ? "inbox.done.keep"
          : a.action === "defer"
            ? "inbox.done.defer"
            : "inbox.done.other";
    const undoable = ["accept", "reject", "defer"].includes(a.action);
    toast(t(key, { n: 1 }), {
      duration: undoable ? 6000 : 3000,
      action: undoable
        ? {
            label: t("inbox.undo"),
            onClick: () =>
              done
                .then(() => undo.mutateAsync([id]))
                .then(() => toast(t("inbox.undone")))
                .catch((err: Error) => toast.error(t("inbox.failed", { error: err.message }))),
          }
        : undefined,
    });
    navigate(next ? `/inbox/${next.item.id}` : "/", { replace: true });
  };

  useMainButton(entry && single && inTelegram ? t("inbox.accept") : null, () => accept.current?.(), act.isPending);

  if (inbox.isPending) return <Skeleton className="h-64 rounded-xl" />;
  if (!entry)
    return (
      <div className="rounded-xl border bg-card p-4">
        <p className="text-[15px]">{t("mini.gone")}</p>
        <Button variant="link" className="px-0" onClick={() => navigate("/", { replace: true })}>
          {t("mini.waiting")}
        </Button>
      </div>
    );

  return (
    <div
      className="flex flex-col gap-2"
      onTouchStart={(e) => (touch.current = { x: e.touches[0].clientX, y: e.touches[0].clientY })}
      onTouchEnd={(e) => {
        const start = touch.current;
        touch.current = null;
        if (!start) return;
        const dx = e.changedTouches[0].clientX - start.x;
        const dy = e.changedTouches[0].clientY - start.y;
        if (Math.abs(dx) < 70 || Math.abs(dx) < 2 * Math.abs(dy)) return;
        const forward = document.documentElement.dir === "rtl" ? dx > 0 : dx < 0;
        go(index + (forward ? 1 : -1));
      }}
    >
      <span className="px-1 text-xs text-muted-foreground">
        {t("mini.position", { i: index + 1, n: items.length })}
        {items.length > 1 && ` · ${t("mini.swipe")}`}
      </span>
      <div className="overflow-hidden rounded-xl border bg-card">
        <Detail key={entry.item.id} entry={entry} busy={act.isPending} onAct={decide} keys={false} accept={inTelegram ? accept : undefined} />
      </div>
    </div>
  );
}

function SearchPage() {
  const { t, date } = useI18n();
  const navigate = useNavigate();
  const [params, setParams] = useSearchParams();
  const q = params.get("q") ?? "";
  const [text, setText] = useState(q);
  useMainButton(null, () => {});

  useEffect(() => {
    const timer = setTimeout(() => text.trim() !== q && setParams(text.trim() ? { q: text.trim() } : {}, { replace: true }), 300);
    return () => clearTimeout(timer);
  }, [text]);

  const list = useQuery({
    queryKey: ["mini-search", q],
    queryFn: async (): Promise<Assertion[]> => {
      if (q) return api<SearchHit[]>(`/v1/search?limit=30&q=${encodeURIComponent(q)}`);
      const recent = await api<Assertion[]>("/v1/assertions?limit=30");
      return recent.sort((a, b) => b.recorded_from.localeCompare(a.recorded_from));
    },
  });

  return (
    <>
      <SearchField value={text} onChange={setText} autoFocus />
      <span className="px-1 text-xs text-muted-foreground">{q ? t("knowledge.results", { n: list.data?.length ?? 0 }) : t("knowledge.recent")}</span>
      {list.isPending ? (
        [0, 1, 2, 3].map((i) => <Skeleton key={i} className="h-14 rounded-xl" />)
      ) : list.data && list.data.length === 0 ? (
        <p className="px-1 text-sm text-muted-foreground">{q ? t("knowledge.empty", { q }) : t("now.nothing")}</p>
      ) : (
        <ul className="overflow-hidden rounded-xl border bg-card">
          {list.data?.map((a) => {
            const { title, description } = titled(a.title, a.text);
            return (
              <li key={a.assertion_id} className="border-b last:border-b-0">
                <button
                  type="button"
                  onClick={() => navigate(`/knowledge/${a.assertion_id}`)}
                  className="flex w-full flex-col gap-1 px-3.5 py-3 text-start active:bg-accent"
                >
                  <span className="line-clamp-2 text-[15px] leading-snug">{title}</span>
                  {description && <span className="line-clamp-1 text-[13px] text-muted-foreground">{description}</span>}
                  <span className="flex flex-wrap items-center gap-1">
                    <KindBadge kind={a.kind} />
                    <SphereBadge sphere={a.sphere} />
                    {a.unverified && <UnverifiedBadge />}
                    <span className="font-mono text-xs text-muted-foreground">{date(a.valid_from)}</span>
                  </span>
                </button>
              </li>
            );
          })}
        </ul>
      )}
    </>
  );
}

type SourceRef = {
  source_id: string;
  kind: string;
  uri: string | null;
  title: string | null;
  said_at: string | null;
  captured_at: string;
  withdrawn: boolean;
  quotes: string[];
  excerpt: string | null;
};

/** Knowledge with its history and the sources it comes from; "Right" as the main button. */
function KnowledgeCard() {
  const { t } = useI18n();
  const { id = "" } = useParams();
  const client = useQueryClient();
  // The same query as the history below, so it is asked once.
  const history = useQuery({
    queryKey: ["history", id],
    queryFn: () => api<Assertion[]>(`/v1/assertions/${id}/history`),
  });
  const head = history.data?.find((v) => !v.recorded_to);
  const verify = useMutation({
    mutationFn: () => api(`/v1/assertions/${id}/verify`, { method: "POST" }),
    onSuccess: () => {
      feel("success");
      toast(t("knowledge.verified"));
      for (const key of [["history", id], ["mini-search"], ["knowledge"]]) client.invalidateQueries({ queryKey: key });
    },
    onError: (err: Error) => (feel("error"), toast.error(t("inbox.failed", { error: err.message }))),
  });
  useMainButton(head?.unverified && tg() ? t("knowledge.verify") : null, () => verify.mutate(), verify.isPending);

  return (
    <>
      <div className="rounded-xl border bg-card p-4">
        <History id={id} />
      </div>
      <Sources id={id} />
    </>
  );
}

function Sources({ id }: { id: string }) {
  const { t, date } = useI18n();
  const sources = useQuery({
    queryKey: ["sources-of", id],
    queryFn: () => api<SourceRef[]>(`/v1/assertions/${id}/sources`),
  });
  const kind = (k: string) => (`sources.kind.${k}` in en ? t(`sources.kind.${k}` as Key) : k);
  return (
    <section className="flex flex-col gap-2">
      <h2 className="px-1 text-xs font-medium tracking-wide text-muted-foreground uppercase">{t("mini.sources")}</h2>
      {sources.isPending ? (
        <Skeleton className="h-20 rounded-xl" />
      ) : !sources.data?.length ? (
        <p className="px-1 text-sm text-muted-foreground">{t("mini.noSources")}</p>
      ) : (
        sources.data.map((s) => (
          <div key={s.source_id} className={cn("flex flex-col gap-1.5 rounded-xl border bg-card p-3.5", s.withdrawn && "opacity-60")}>
            <span className="text-xs text-muted-foreground">
              {kind(s.kind)} · {date(s.said_at ?? s.captured_at, "long")}
              {s.withdrawn && ` · ${t("mini.withdrawn")}`}
            </span>
            {s.title && <span className="text-sm font-medium">{s.title}</span>}
            {s.excerpt && <p className="text-sm whitespace-pre-line text-foreground/85">{s.excerpt}</p>}
            {s.quotes.map((quote, i) => (
              <blockquote key={i} className="border-s-2 border-primary/50 ps-3 text-sm italic">
                {quote}
              </blockquote>
            ))}
            {s.uri && /^https?:\/\//.test(s.uri) && (
              <a href={s.uri} target="_blank" rel="noopener noreferrer" className="inline-flex items-center gap-1.5 self-start text-sm text-primary">
                <ExternalLink className="size-3.5" aria-hidden /> {host(s.uri)}
              </a>
            )}
          </div>
        ))
      )}
    </section>
  );
}
