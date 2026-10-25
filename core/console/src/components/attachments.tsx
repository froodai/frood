// Photos, videos, documents and links kept with knowledge or with a change that waits
// (docs/26 §2): shown in place; added with a button, by dropping or pasting files, or by pasting
// a link.

import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Download, ExternalLink, FileText, Link2, Paperclip, X } from "lucide-react";
import { useRef, useState } from "react";
import { toast } from "sonner";
import { Button } from "@/components/ui/button";
import { Dialog, DialogContent, DialogTitle } from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { useI18n } from "@/i18n";
import { api } from "@/lib/frood";
import type { Attachment, FileInfo } from "@/lib/types";
import { cn } from "@/lib/utils";

/** The largest file the server takes, as it says (`/v1/version`). */
function useMaxMb() {
  const version = useQuery({ queryKey: ["version"], queryFn: () => api<{ max_file_mb?: number }>("/v1/version"), staleTime: Infinity });
  return version.data?.max_file_mb ?? 25;
}

const host = (uri: string) => {
  try {
    return new URL(uri).host;
  } catch {
    return uri;
  }
};

const size = (bytes: number) => (bytes < 1_048_576 ? `${Math.max(1, Math.round(bytes / 1024))} KB` : `${(bytes / 1_048_576).toFixed(1)} MB`);

/** A source's file as an address the page can show; fetched once with the person's token. */
function useFile(sourceId: string, wanted: boolean) {
  return useQuery({
    queryKey: ["file", sourceId],
    queryFn: async () => URL.createObjectURL(await api<Blob>(`/v1/sources/${sourceId}/file`, { blob: true })),
    enabled: wanted,
    staleTime: Infinity,
  });
}

/** Saves a file as a download, whatever its type, so nothing someone uploaded runs here. */
async function download(a: Attachment) {
  const blob = await api<Blob>(`/v1/sources/${a.source_id}/file`, { blob: true });
  const url = URL.createObjectURL(new Blob([blob], { type: "application/octet-stream" }));
  const link = document.createElement("a");
  link.href = url;
  link.download = a.file?.filename ?? "file";
  link.click();
  setTimeout(() => URL.revokeObjectURL(url), 1000);
}

export function Attachments({ target, also = [] }: { target: string; also?: Attachment[] }) {
  const { t } = useI18n();
  const client = useQueryClient();
  const key = ["attachments", target];
  const list = useQuery({ queryKey: key, queryFn: () => api<Attachment[]>(`/v1/${target}/attachments`) });
  const [link, setLink] = useState("");
  const [over, setOver] = useState(false);
  const input = useRef<HTMLInputElement>(null);
  const settle = () => {
    client.invalidateQueries({ queryKey: key });
    client.invalidateQueries({ queryKey: ["inbox"] });
  };
  const failed = (err: Error) => toast.error(t("attach.failed", { error: err.message }));
  const maxMb = useMaxMb();
  const upload = useMutation({
    mutationFn: async (files: File[]) => {
      for (const file of files) {
        if (file.size > maxMb * 1_048_576) throw new Error(t("attach.tooBig", { name: file.name, mb: maxMb }));
        const form = new FormData();
        form.append("file", file, file.name);
        await api(`/v1/${target}/attachments`, { method: "POST", form });
      }
    },
    onSettled: settle,
    onError: failed,
  });
  const addLink = useMutation({
    mutationFn: (uri: string) => api(`/v1/${target}/attachments`, { method: "POST", json: { uri } }),
    onSuccess: () => setLink(""),
    onSettled: settle,
    onError: failed,
  });
  const remove = useMutation({
    mutationFn: (id: string) => api(`/v1/attachments/${id}`, { method: "DELETE" }),
    onSettled: settle,
    onError: failed,
  });

  const own = list.data ?? [];
  const items = [...own, ...also.filter((a) => !own.some((b) => b.source_id === a.source_id))];
  const files = (data: DataTransfer | null) => [...(data?.files ?? [])];
  const send = (chosen: File[]) => chosen.length > 0 && upload.mutate(chosen);

  return (
    <section
      aria-label={t("attach.title")}
      className="flex flex-col gap-2"
      onDragOver={(e) => {
        if (!e.dataTransfer.types.includes("Files")) return;
        e.preventDefault();
        setOver(true);
      }}
      onDragLeave={() => setOver(false)}
      onDrop={(e) => {
        e.preventDefault();
        setOver(false);
        send(files(e.dataTransfer));
      }}
      onPaste={(e) => {
        const pasted = files(e.clipboardData);
        if (pasted.length === 0) return;
        e.preventDefault();
        send(pasted);
      }}
    >
      <span className="text-[11px] font-medium tracking-wide text-muted-foreground uppercase">{t("attach.title")}</span>
      {items.length > 0 && (
        <div className="grid grid-cols-2 gap-2 sm:grid-cols-3">
          {items.map((a) => (
            <Tile key={a.id} a={a} onRemove={own.some((o) => o.id === a.id) ? () => remove.mutate(a.id) : undefined} />
          ))}
        </div>
      )}
      <div className={cn("flex flex-wrap items-center gap-2 rounded-lg border border-dashed p-2 transition-colors", over && "border-primary bg-accent")}>
        <Button type="button" size="sm" variant="outline" disabled={upload.isPending} onClick={() => input.current?.click()}>
          <Paperclip className="size-3.5" aria-hidden />
          {upload.isPending ? t("attach.uploading") : t("attach.add")}
        </Button>
        <form
          className="flex min-w-40 flex-1 gap-2"
          onSubmit={(e) => {
            e.preventDefault();
            if (link.trim()) addLink.mutate(link.trim());
          }}
        >
          <Input value={link} onChange={(e) => setLink(e.target.value)} placeholder={t("attach.link")} aria-label={t("attach.link")} className="h-8" />
          {link.trim() && (
            <Button type="submit" size="sm" disabled={addLink.isPending}>
              {t("attach.addLink")}
            </Button>
          )}
        </form>
        <input
          ref={input}
          type="file"
          multiple
          hidden
          onChange={(e) => {
            const chosen = [...(e.target.files ?? [])];
            e.target.value = "";
            send(chosen);
          }}
        />
        <span className="w-full text-xs text-muted-foreground">{t("attach.drop")}</span>
      </div>
    </section>
  );
}

function Tile({ a, onRemove }: { a: Attachment; onRemove?: () => void }) {
  const { t } = useI18n();
  const [open, setOpen] = useState(false);
  const shown = a.kind === "photo" || a.kind === "video" || a.kind === "audio";
  const file = useFile(a.source_id, shown && !!a.file);
  const wide = a.kind !== "photo";
  const caption = a.text?.trim();

  let body: React.ReactNode;
  if (a.kind === "link" && a.uri) {
    body = (
      <a href={a.uri} target="_blank" rel="noopener noreferrer" className="flex items-center gap-2.5 rounded-lg border p-2.5 hover:bg-accent">
        <Link2 className="size-4 shrink-0 text-muted-foreground" aria-hidden />
        <span className="flex min-w-0 flex-1 flex-col">
          <span className="truncate text-sm">{a.title || host(a.uri)}</span>
          <span className="truncate text-xs text-muted-foreground" dir="ltr">
            {host(a.uri)}
          </span>
        </span>
        <ExternalLink className="size-3.5 shrink-0 text-muted-foreground" aria-hidden />
      </a>
    );
  } else if (a.kind === "photo" && file.data) {
    body = (
      <>
        <button type="button" onClick={() => setOpen(true)} className="block overflow-hidden rounded-lg border" aria-label={a.file?.filename ?? t("attach.title")}>
          <img src={file.data} alt={caption ?? a.file?.filename ?? ""} className="aspect-square w-full object-cover" />
        </button>
        <Dialog open={open} onOpenChange={setOpen}>
          <DialogContent className="max-w-[min(92vw,1100px)] p-2">
            <DialogTitle className="sr-only">{a.file?.filename ?? t("attach.title")}</DialogTitle>
            <img src={file.data} alt={caption ?? ""} className="max-h-[85dvh] w-full rounded-md object-contain" />
          </DialogContent>
        </Dialog>
      </>
    );
  } else if (a.kind === "video" && file.data) {
    body = <video src={file.data} controls preload="metadata" className="w-full rounded-lg border bg-black" />;
  } else if (a.kind === "audio" && file.data) {
    body = <audio src={file.data} controls className="w-full" />;
  } else if (shown && file.isPending) {
    body = <div className="aspect-square w-full animate-pulse rounded-lg border bg-muted" />;
  } else {
    body = (
      <button type="button" onClick={() => download(a)} className="flex w-full items-center gap-2.5 rounded-lg border p-2.5 text-start hover:bg-accent">
        <FileText className="size-4 shrink-0 text-muted-foreground" aria-hidden />
        <span className="flex min-w-0 flex-1 flex-col">
          <span className="truncate text-sm">{a.title || a.file?.filename || t("attach.title")}</span>
          {a.file && <span className="text-xs text-muted-foreground">{size(a.file.size_bytes)}</span>}
        </span>
        <Download className="size-3.5 shrink-0 text-muted-foreground" aria-label={t("attach.download")} />
      </button>
    );
  }

  return (
    <div className={cn("group relative flex flex-col gap-1", wide && "col-span-full")}>
      {body}
      {caption && <span className="line-clamp-2 text-xs text-muted-foreground">{caption}</span>}
      {onRemove && (
        <button
          type="button"
          onClick={onRemove}
          aria-label={t("attach.remove")}
          className="absolute -end-1.5 -top-1.5 grid size-5 place-items-center rounded-full border bg-background text-muted-foreground opacity-0 shadow-sm group-hover:opacity-100 hover:text-foreground focus-visible:opacity-100"
        >
          <X className="size-3" />
        </button>
      )}
    </div>
  );
}

/** The file of a source (a photo, a video, a voice message, a document), shown the same way. */
export function SourceFile({ sourceId, kind, file, title }: { sourceId: string; kind: string; file: FileInfo; title?: string | null }) {
  const t = file.content_type;
  const shownKind = t.startsWith("image/") ? "photo" : t.startsWith("video/") ? "video" : t.startsWith("audio/") ? "audio" : kind === "photo" ? "photo" : "file";
  const a: Attachment = {
    id: sourceId,
    assertion_id: null,
    proposal_id: null,
    source_id: sourceId,
    kind: shownKind,
    uri: null,
    title: title ?? null,
    text: null,
    file,
    added_by: "",
    added_at: "",
  };
  return (
    <div className="grid grid-cols-2 gap-2 sm:grid-cols-3">
      <Tile a={a} />
    </div>
  );
}
