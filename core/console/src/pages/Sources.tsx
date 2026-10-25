import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { FileAudio, FileText, MessagesSquare, Paperclip } from "lucide-react";
import { useState } from "react";
import { toast } from "sonner";
import { SourceFile } from "@/components/attachments";
import { Button } from "@/components/ui/button";
import { Sheet, SheetContent, SheetTitle } from "@/components/ui/sheet";
import { Skeleton } from "@/components/ui/skeleton";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { Textarea } from "@/components/ui/textarea";
import { useI18n } from "@/i18n";
import type { Key } from "@/i18n/en";
import { api } from "@/lib/frood";
import { useMe } from "@/lib/queries";
import type { FileInfo } from "@/lib/types";
import { cn } from "@/lib/utils";
import { Connected } from "./Connections";

type SourceSummary = {
  id: string;
  kind: string;
  origin: string;
  title: string | null;
  excerpt: string | null;
  captured_by: string;
  captured_at: string;
  said_at: string | null;
  withdrawn_at: string | null;
  content_type: string | null;
};
type Source = SourceSummary & { content: string | null; uri: string | null; file?: FileInfo | null };
type Status = {
  state: string;
  proposals: { id: string; state: string; risk: string; alternatives: { operations: { op: string; assertion?: { text: string } }[] }[] }[];
};

const KIND_ICON: Record<string, typeof FileText> = { voice: FileAudio, audio: FileAudio, file: Paperclip, conversation: MessagesSquare };

/** Who sent or read it, in words. */
function useWho() {
  const { t } = useI18n();
  const me = useMe();
  return (actor: string) => {
    const [who, , via] = actor.split("/");
    if (who.startsWith("user:")) {
      const self = me.data?.user_id && who === `user:${me.data.user_id}`;
      const person = self ? t("sources.you") : t("origin.person");
      return via ? `${person} · ${via}` : person;
    }
    if (who.startsWith("agent:")) return t("origin.agentJob", { job: who.slice("agent:".length) });
    return who;
  };
}

export function Sources() {
  const { t } = useI18n();
  return (
    <Tabs defaultValue="captured" className="flex flex-col gap-4 px-4 py-5 md:px-7">
      <TabsList>
        <TabsTrigger value="captured">{t("sources.captured")}</TabsTrigger>
        <TabsTrigger value="connected">{t("sources.connected")}</TabsTrigger>
      </TabsList>
      <TabsContent value="captured">
        <Captured />
      </TabsContent>
      <TabsContent value="connected">
        <Connected />
      </TabsContent>
    </Tabs>
  );
}

function Captured() {
  const { t, ago } = useI18n();
  const who = useWho();
  const client = useQueryClient();
  const [text, setText] = useState("");
  const [file, setFile] = useState<File | null>(null);
  const [only, setOnly] = useState<"all" | "notes" | "read">("all");
  const [open, setOpen] = useState<string | null>(null);
  const list = useQuery({ queryKey: ["sources"], queryFn: () => api<SourceSummary[]>("/v1/sources?limit=100"), refetchInterval: 30_000 });
  const note = useMutation({
    mutationFn: (body: string) => {
      if (!file) return api("/v1/capture", { method: "POST", json: { text: body, kind: "note" } });
      // A photo, a video or a document with the words about it.
      const form = new FormData();
      if (body) form.append("text", body);
      form.append("file", file, file.name);
      return api("/v1/capture", { method: "POST", form });
    },
    onSuccess: () => {
      setText("");
      setFile(null);
      toast.success(t("sources.noted"));
      client.invalidateQueries({ queryKey: ["sources"] });
    },
    onError: (err) => toast.error(t("inbox.failed", { error: err.message })),
  });
  const shown = (list.data ?? []).filter((s) => only === "all" || (only === "notes" ? s.origin === "capture" : s.origin !== "capture"));

  return (
    <div className="flex max-w-4xl flex-col gap-4">
      <form
        className="flex flex-col gap-2 rounded-xl border bg-card p-3"
        onSubmit={(e) => {
          e.preventDefault();
          if (text.trim() || file) note.mutate(text.trim());
        }}
      >
        <Textarea
          value={text}
          onChange={(e) => setText(e.target.value)}
          placeholder={t("sources.notePlaceholder")}
          aria-label={t("sources.notePlaceholder")}
          className="min-h-20 resize-y border-0 shadow-none focus-visible:ring-0"
        />
        <div className="flex flex-wrap items-center justify-between gap-2">
          <div className="flex min-w-0 items-center gap-2">
            <label className="inline-flex size-8 cursor-pointer items-center justify-center rounded-md text-muted-foreground hover:bg-accent hover:text-foreground" aria-label={t("attach.add")} title={t("attach.add")}>
              <Paperclip className="size-4" aria-hidden />
              <input type="file" hidden onChange={(e) => (setFile(e.target.files?.[0] ?? null), (e.target.value = ""))} />
            </label>
            {file ? (
              <span className="inline-flex min-w-0 items-center gap-1 rounded-md border px-2 py-0.5 text-xs">
                <span className="truncate">{file.name}</span>
                <button type="button" onClick={() => setFile(null)} aria-label={t("attach.remove")} className="text-muted-foreground hover:text-foreground">
                  ×
                </button>
              </span>
            ) : (
              <span className="text-xs text-muted-foreground">{t("sources.noteHint")}</span>
            )}
          </div>
          <Button type="submit" size="sm" disabled={(!text.trim() && !file) || note.isPending}>
            {t("sources.note")}
          </Button>
        </div>
      </form>

      <div className="flex flex-wrap gap-1.5">
        {(
          [
            ["all", "sources.all"],
            ["notes", "sources.notes"],
            ["read", "sources.read.tab"],
          ] as const
        ).map(([value, label]) => (
          <button
            key={value}
            type="button"
            aria-pressed={only === value}
            onClick={() => setOnly(value)}
            className={cn(
              "h-8 rounded-full border px-3 text-[13px]",
              only === value ? "border-foreground bg-foreground text-background" : "bg-card hover:bg-accent",
            )}
          >
            {t(label)}
          </button>
        ))}
      </div>

      {list.isPending ? (
        <Skeleton className="h-64" />
      ) : shown.length === 0 ? (
        <p className="text-sm text-muted-foreground">{t("sources.empty")}</p>
      ) : (
        <div className="overflow-hidden rounded-xl border bg-card">
          {shown.map((s) => {
            const Icon = KIND_ICON[s.kind] ?? FileText;
            return (
              <button
                key={s.id}
                type="button"
                onClick={() => setOpen(s.id)}
                className="flex w-full items-start gap-3 border-b px-4 py-3 text-start last:border-b-0 hover:bg-accent/40"
              >
                <Icon className="mt-0.5 size-4 shrink-0 text-muted-foreground" aria-hidden />
                <span className="flex min-w-0 flex-1 flex-col gap-1">
                  <span className={cn("line-clamp-2 text-sm", s.withdrawn_at && "text-muted-foreground line-through")}>
                    {s.title ?? s.excerpt ?? t(`sources.kind.${s.kind}` as Key)}
                  </span>
                  <span className="flex flex-wrap gap-x-2 text-xs text-muted-foreground">
                    <span>{who(s.captured_by)}</span>
                    <span aria-hidden>·</span>
                    <span>{ago(s.said_at ?? s.captured_at)}</span>
                    {s.withdrawn_at && <span className="text-warning">{t("sources.withdrawn")}</span>}
                  </span>
                </span>
              </button>
            );
          })}
        </div>
      )}

      <Sheet open={!!open} onOpenChange={(o) => !o && setOpen(null)}>
        <SheetContent className="w-full overflow-y-auto sm:max-w-lg">
          <SheetTitle className="sr-only">{t("sources.captured")}</SheetTitle>
          {open && <SourceDetail id={open} onDone={() => client.invalidateQueries({ queryKey: ["sources"] })} />}
        </SheetContent>
      </Sheet>
    </div>
  );
}

const STATE_PILL: Record<string, string> = {
  accepted: "bg-emerald-500/12 text-emerald-700 dark:text-emerald-300",
  open: "bg-sky-500/12 text-sky-700 dark:text-sky-300",
  deferred: "bg-amber-500/15 text-amber-800 dark:text-amber-300",
  rejected: "bg-zinc-500/15 text-zinc-700 dark:text-zinc-300",
  stale: "bg-zinc-500/15 text-zinc-700 dark:text-zinc-300",
};

function SourceDetail({ id, onDone }: { id: string; onDone: () => void }) {
  const { t, ago } = useI18n();
  const who = useWho();
  const me = useMe();
  const client = useQueryClient();
  const source = useQuery({ queryKey: ["source", id], queryFn: () => api<Source>(`/v1/sources/${id}`) });
  const status = useQuery({ queryKey: ["source-status", id], queryFn: () => api<Status>(`/v1/sources/${id}/status`) });
  const withdraw = useMutation({
    mutationFn: () => api<{ closed: number; kept: number }>(`/v1/sources/${id}/withdraw`, { method: "POST", json: {} }),
    onSuccess: (done) => {
      toast(done.kept ? t("sources.withdrawnKept", { n: done.kept }) : t("sources.withdrawnDone"));
      client.invalidateQueries({ queryKey: ["source", id] });
      client.invalidateQueries({ queryKey: ["source-status", id] });
      client.invalidateQueries({ queryKey: ["inbox"] });
      onDone();
    },
    onError: (err) => toast.error(t("inbox.failed", { error: err.message })),
  });
  if (!source.data) return <Skeleton className="m-6 h-40" />;
  const s = source.data;
  const mine = !!me.data?.user_id && (s.captured_by === `user:${me.data.user_id}` || s.captured_by.startsWith(`user:${me.data.user_id}/`));
  const made = status.data?.proposals ?? [];
  return (
    <div className="flex flex-col gap-4 p-5">
      <div className="flex flex-col gap-1">
        <span className="text-xs text-muted-foreground">
          {t(`sources.kind.${s.kind}` as Key)} · {who(s.captured_by)} · {ago(s.said_at ?? s.captured_at)}
        </span>
        {s.title && <h2 className="text-lg font-semibold">{s.title}</h2>}
      </div>
      {s.file && <SourceFile sourceId={s.id} kind={s.kind} file={s.file} title={s.title} />}
      {s.content && <p className={cn("text-sm whitespace-pre-line", s.withdrawn_at && "text-muted-foreground line-through")}>{s.content}</p>}
      {s.uri && (
        <a href={s.uri} target="_blank" rel="noreferrer" className="text-sm break-all">
          {s.uri}
        </a>
      )}
      <div className="flex flex-col gap-2">
        <span className="text-[11px] font-medium tracking-wide text-muted-foreground uppercase">{t("sources.made")}</span>
        {made.length === 0 ? (
          <p className="text-sm text-muted-foreground">{status.data?.state === "done" || status.data?.state === "idle" ? t("sources.madeNothing") : t("sources.processing")}</p>
        ) : (
          made.map((p) => (
            <div key={p.id} className="flex items-start gap-2 text-sm">
              <span className={cn("shrink-0 rounded-md px-1.5 py-0.5 text-[11px] font-medium", STATE_PILL[p.state] ?? STATE_PILL.rejected)}>
                {t(`proposal.${p.state}` as Key)}
              </span>
              <span className="min-w-0">
                {p.alternatives
                  .flatMap((a) => a.operations)
                  .map((op) => op.assertion?.text)
                  .filter(Boolean)
                  .join("; ") || "—"}
              </span>
            </div>
          ))
        )}
      </div>
      {mine && !s.withdrawn_at && (
        <Button variant="outline" className="self-start" disabled={withdraw.isPending} onClick={() => confirm(t("sources.withdrawConfirm")) && withdraw.mutate()}>
          {t("sources.withdraw")}
        </Button>
      )}
      {s.withdrawn_at && <p className="text-xs text-warning">{t("sources.withdrawn")}</p>}
    </div>
  );
}

