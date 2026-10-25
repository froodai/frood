// Sources connected to the hub (docs/26 §4): frood.yaml's, shown as they are, and those
// connected here — from a catalog, with a token, a check that lists the server's tools, and
// what the nightly pass should read there.

import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { ArrowLeft, ExternalLink, Plug, Plus } from "lucide-react";
import { useEffect, useState } from "react";
import { useSearchParams } from "react-router";
import { toast } from "sonner";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Sheet, SheetContent, SheetTitle } from "@/components/ui/sheet";
import { Skeleton } from "@/components/ui/skeleton";
import { Switch } from "@/components/ui/switch";
import { Textarea } from "@/components/ui/textarea";
import { useI18n } from "@/i18n";
import type { Key } from "@/i18n/en";
import { api } from "@/lib/frood";
import { useHealth } from "@/lib/queries";
import type { Health } from "@/lib/types";
import { cn } from "@/lib/utils";
import { Dot } from "./System";

type Connection = {
  name: string;
  from: "config" | "console";
  server: string | null;
  scope: string | null;
  tools: string[] | null;
  authoritative: boolean;
  has_token: boolean;
  signed_in: boolean;
};
type Tool = { name: string; description: string | null; read_only: boolean | null };

/** Servers people connect most; any other MCP server works the same way. */
type Entry = { id: string; name: string; server: string; placeholder?: string; help?: string; oauth?: boolean; scope?: Key };
const CATALOG: Entry[] = [
  {
    id: "youtrack",
    name: "YouTrack",
    server: "",
    placeholder: "https://example.youtrack.cloud/mcp",
    help: "https://www.jetbrains.com/help/youtrack/cloud/manage-permanent-token.html",
    scope: "connect.scope.youtrack",
  },
  { id: "github", name: "GitHub", server: "https://api.githubcopilot.com/mcp/", help: "https://github.com/settings/personal-access-tokens", scope: "connect.scope.github" },
  { id: "linear", name: "Linear", server: "https://mcp.linear.app/mcp", help: "https://linear.app/settings/account/security", scope: "connect.scope.linear" },
  { id: "jira", name: "Jira", server: "https://mcp.atlassian.com/v1/mcp", oauth: true },
  { id: "confluence", name: "Confluence", server: "https://mcp.atlassian.com/v1/mcp", oauth: true },
  { id: "notion", name: "Notion", server: "https://mcp.notion.com/mcp", oauth: true },
  { id: "gitlab", name: "GitLab", server: "", placeholder: "https://gitlab.example.com/api/v4/mcp", oauth: true },
  { id: "slack", name: "Slack", server: "", placeholder: "https://…/mcp", oauth: true },
  { id: "drive", name: "Google Drive", server: "", placeholder: "https://…/mcp", oauth: true },
];

type Open = null | { kind: "source"; name: string } | { kind: "module"; item: Health["modules"][number] } | { kind: "catalog" } | { kind: "new"; entry: Entry | null };

export function Connected() {
  const { t, lang } = useI18n();
  const health = useHealth();
  const connections = useQuery({ queryKey: ["connections"], queryFn: () => api<{ sources: Connection[]; vault: boolean }>("/v1/connections") });
  const [open, setOpen] = useState<Open>(null);
  const [params, setParams] = useSearchParams();
  // Back from signing in: say how it went, and open the source to choose its tools.
  useEffect(() => {
    const connected = params.get("connected");
    const error = params.get("connect_error");
    if (!connected && !error) return;
    if (connected) {
      toast.success(t("connect.signInDone", { name: connected }));
      setOpen({ kind: "source", name: connected });
    } else if (error) {
      toast.error(t("connect.signInFailed", { error }));
    }
    setParams({}, { replace: true });
  }, [params, setParams, t]);
  const relative = new Intl.RelativeTimeFormat(lang, { numeric: "auto" });
  if (health.isError) return <p className="text-sm text-destructive">{health.error.message}</p>;
  if (!health.data || connections.isPending) return <Skeleton className="h-48" />;
  const sources = connections.data?.sources ?? [];
  const { modules } = health.data;
  const lastRead = (name: string) => {
    const reads = health.data.sources.filter((s) => s.source === name).map((s) => s.seconds_since_read);
    return reads.length === 0 || reads.every((r) => r === null) ? null : Math.min(...reads.filter((r): r is number => r !== null));
  };
  const since = (seconds: number) =>
    seconds < 3600
      ? relative.format(-Math.round(seconds / 60), "minute")
      : seconds < 86400
        ? relative.format(-Math.round(seconds / 3600), "hour")
        : relative.format(-Math.round(seconds / 86400), "day");
  const card = "flex flex-col gap-1.5 rounded-xl border bg-card p-4 text-start transition-colors hover:border-foreground/20";
  const chosen = open?.kind === "source" ? sources.find((s) => s.name === open.name) : undefined;

  return (
    <div className="flex max-w-6xl flex-col gap-5">
      <h2 className="text-sm font-medium">{t("sources.title")}</h2>
      <div className="grid grid-cols-1 gap-3 sm:grid-cols-2 xl:grid-cols-3">
        {sources.map((s) => {
          const read = lastRead(s.name);
          return (
            <button key={s.name} type="button" className={card} onClick={() => setOpen({ kind: "source", name: s.name })}>
              <span className="flex items-center gap-2">
                <Dot ok={read === null ? null : read < 2 * 86400} />
                <span className="flex-1 font-medium">{s.name}</span>
                {s.from === "config" && <span className="rounded-md border px-1.5 text-[11px] text-muted-foreground">{t("connect.fromConfig")}</span>}
              </span>
              <span className="text-sm text-muted-foreground">{read === null ? t("sources.never") : t("sources.read", { when: since(read) })}</span>
            </button>
          );
        })}
        <button type="button" className={cn(card, "items-start border-dashed")} onClick={() => setOpen({ kind: "catalog" })}>
          <span className="flex items-center gap-2 font-medium">
            <Plus className="size-4" /> {t("sources.add")}
          </span>
          <span className="text-sm text-muted-foreground">{t("sources.addHint")}</span>
        </button>
      </div>
      <h2 className="text-sm font-medium">{t("sources.modules")}</h2>
      <div className="grid grid-cols-1 gap-3 sm:grid-cols-2 xl:grid-cols-3">
        {modules.map((m) => (
          <button key={m.name} type="button" className={card} onClick={() => setOpen({ kind: "module", item: m })}>
            <span className="flex items-center gap-2">
              <Dot ok={m.up} />
              <span className="flex-1 font-medium">{m.name}</span>
              <span className="text-xs text-muted-foreground">{m.up ? t("sources.up") : t("sources.down")}</span>
            </span>
            <span className="font-mono text-xs text-muted-foreground" dir="ltr">
              {m.provides.join(" · ")}
            </span>
          </button>
        ))}
      </div>

      <Sheet open={!!open} onOpenChange={(o) => !o && setOpen(null)}>
        <SheetContent className="w-full overflow-y-auto sm:max-w-lg">
          <SheetTitle className="sr-only">{t("sources.connected")}</SheetTitle>
          <div className="flex flex-col gap-4 p-5 text-sm">
            {open?.kind === "source" && chosen?.from === "config" && (
              <>
                <h2 className="text-lg font-semibold">{chosen.name}</h2>
                <p className="font-mono text-xs break-all text-muted-foreground" dir="ltr">
                  {chosen.server}
                </p>
                {chosen.scope && <p>{chosen.scope}</p>}
                <p className="text-muted-foreground">{t("connect.configOnly")}</p>
              </>
            )}
            {open?.kind === "source" && chosen?.from === "console" && (
              <ConnectForm key={chosen.name} existing={chosen} entry={null} vault={!!connections.data?.vault} checkNow={chosen.signed_in && chosen.tools?.length === 0} onDone={() => setOpen(null)} />
            )}
            {open?.kind === "module" && (
              <>
                <h2 className="text-lg font-semibold">{open.item.name}</h2>
                <p className="flex items-center gap-2">
                  <Dot ok={open.item.up} /> {open.item.up ? t("sources.up") : t("sources.down")}
                </p>
                <ul className="list-disc ps-5 font-mono text-xs" dir="ltr">
                  {open.item.provides.map((p) => (
                    <li key={p}>{p}</li>
                  ))}
                </ul>
              </>
            )}
            {open?.kind === "catalog" && (
              <>
                <h2 className="text-lg font-semibold">{t("connect.catalog")}</h2>
                <div className="grid grid-cols-2 gap-2">
                  {CATALOG.map((entry) => (
                    <button key={entry.id} type="button" className="flex flex-col items-start gap-0.5 rounded-lg border p-3 text-start hover:bg-accent" onClick={() => setOpen({ kind: "new", entry })}>
                      <span className="font-medium">{entry.name}</span>
                      {entry.oauth && <span className="text-xs text-muted-foreground">OAuth</span>}
                    </button>
                  ))}
                  <button type="button" className="col-span-2 flex items-center gap-2 rounded-lg border border-dashed p-3 text-start hover:bg-accent" onClick={() => setOpen({ kind: "new", entry: null })}>
                    <Plug className="size-4 text-muted-foreground" aria-hidden />
                    <span className="flex flex-col">
                      <span className="font-medium">{t("connect.any")}</span>
                      <span className="text-xs text-muted-foreground">{t("connect.anyHint")}</span>
                    </span>
                  </button>
                </div>
              </>
            )}
            {open?.kind === "new" && (
              <>
                <button type="button" className="inline-flex items-center gap-1 self-start text-xs text-muted-foreground hover:text-foreground" onClick={() => setOpen({ kind: "catalog" })}>
                  <ArrowLeft className="size-3.5 rtl:rotate-180" /> {t("connect.back")}
                </button>
                <ConnectForm existing={null} entry={open.entry} vault={!!connections.data?.vault} taken={sources.map((s) => s.name)} onDone={() => setOpen(null)} />
              </>
            )}
          </div>
        </SheetContent>
      </Sheet>
    </div>
  );
}

/** A name for a new source: the catalog's, made unique among those taken. */
function freeName(base: string, taken: string[]) {
  const name = base.toLowerCase().replace(/[^a-z0-9-]+/g, "-").replace(/^-+|-+$/g, "") || "source";
  let n = 1;
  let candidate = name;
  while (taken.includes(candidate)) candidate = `${name}-${++n}`;
  return candidate;
}

function ConnectForm({
  existing,
  entry,
  vault,
  taken = [],
  checkNow = false,
  onDone,
}: {
  existing: Connection | null;
  entry: Entry | null;
  vault: boolean;
  taken?: string[];
  /** Check at once, as after signing in, so the tools can be chosen. */
  checkNow?: boolean;
  onDone: () => void;
}) {
  const { t } = useI18n();
  const client = useQueryClient();
  const [name, setName] = useState(existing?.name ?? freeName(entry?.id ?? "mcp", taken));
  const [server, setServer] = useState(existing?.server ?? entry?.server ?? "");
  const [token, setToken] = useState("");
  const [scope, setScope] = useState(existing?.scope ?? (entry?.scope ? t(entry.scope) : t("connect.scope.default")));
  const [authoritative, setAuthoritative] = useState(existing?.authoritative ?? false);
  const [tools, setTools] = useState<Tool[] | null>(null);
  const [chosen, setChosen] = useState<Set<string>>(new Set(existing?.tools ?? []));
  const failed = (err: Error) => toast.error(t("inbox.failed", { error: err.message }));
  const settle = () => {
    client.invalidateQueries({ queryKey: ["connections"] });
    client.invalidateQueries({ queryKey: ["health"] });
  };

  const check = useMutation({
    mutationFn: () =>
      api<{ server: string | null; tools: Tool[] }>("/v1/connections/check", {
        method: "POST",
        json: { server: server.trim(), token: token.trim() || undefined, name: existing?.name },
      }),
    onSuccess: (found) => {
      setTools(found.tools);
      // Reading tools by default; all of them when the server says nothing about any.
      const said = found.tools.some((tool) => tool.read_only !== null);
      if (!existing?.tools?.length) setChosen(new Set(found.tools.filter((tool) => (said ? tool.read_only : true)).map((tool) => tool.name)));
      toast.success(t("connect.checked", { n: found.tools.length }));
    },
    onError: failed,
  });
  // Signing in leaves for the server's page; it sends the person back to this one.
  const signIn = useMutation({
    mutationFn: () =>
      api<{ authorize_url: string }>(`/v1/connections/${name}/sign-in`, {
        method: "POST",
        json: { server: server.trim(), scope: scope.trim(), authoritative },
      }),
    onSuccess: ({ authorize_url }) => location.assign(authorize_url),
    onError: failed,
  });
  useEffect(() => {
    // Once, when the form opens.
    if (checkNow) check.mutate();
  }, []);
  const save = useMutation({
    mutationFn: () =>
      api(`/v1/connections/${name}`, {
        method: "PUT",
        json: { server: server.trim(), token: token.trim() || undefined, scope: scope.trim(), tools: [...chosen], authoritative },
      }),
    onSuccess: () => {
      toast.success(t("connect.saved", { name }));
      settle();
      onDone();
    },
    onError: failed,
  });
  const remove = useMutation({
    mutationFn: () => api(`/v1/connections/${name}`, { method: "DELETE" }),
    onSuccess: () => {
      toast(t("connect.removed", { name }));
      settle();
      onDone();
    },
    onError: failed,
  });

  const field = "flex flex-col gap-1.5";
  const label = "text-xs font-medium text-muted-foreground";
  return (
    <form
      className="flex flex-col gap-4"
      onSubmit={(e) => {
        e.preventDefault();
        save.mutate();
      }}
    >
      <h2 className="text-lg font-semibold">{existing ? existing.name : (entry?.name ?? t("connect.any"))}</h2>
      {existing?.signed_in && <p className="text-muted-foreground">{t("connect.signedIn")}</p>}
      {!vault && <p className="rounded-md border border-warning/50 p-3 text-warning">{t("connect.noVault")}</p>}
      {!existing && (
        <label className={field}>
          <span className={label}>{t("connect.name")}</span>
          <Input value={name} onChange={(e) => setName(e.target.value)} dir="ltr" />
        </label>
      )}
      <label className={field}>
        <span className={label}>{t("connect.server")}</span>
        <Input value={server} onChange={(e) => setServer(e.target.value)} placeholder={entry?.placeholder ?? "https://…/mcp"} dir="ltr" required />
      </label>
      <label className={field}>
        <span className="flex items-center justify-between gap-2">
          <span className={label}>{t("connect.token")}</span>
          {entry?.help && (
            <a href={entry.help} target="_blank" rel="noopener noreferrer" className="inline-flex items-center gap-1 text-xs text-primary">
              {t("connect.tokenWhere")} <ExternalLink className="size-3" />
            </a>
          )}
        </span>
        <Input type="password" value={token} onChange={(e) => setToken(e.target.value)} placeholder={existing?.has_token ? t("connect.tokenKept") : ""} autoComplete="off" dir="ltr" />
      </label>
      <div className="flex flex-wrap items-center gap-2">
        <Button type="button" variant="outline" disabled={!server.trim() || check.isPending} onClick={() => check.mutate()}>
          {check.isPending ? t("connect.checking") : t("connect.check")}
        </Button>
        {!existing && (
          <Button type="button" variant={entry?.oauth ? "default" : "outline"} disabled={!server.trim() || !name.trim() || signIn.isPending || !vault} onClick={() => signIn.mutate()}>
            {t("connect.signIn")}
          </Button>
        )}
      </div>
      {!existing && <p className="-mt-2 text-xs text-muted-foreground">{t("connect.signInHint")}</p>}
      {tools && (
        <fieldset className={field}>
          <legend className={label}>{t("connect.tools")}</legend>
          <span className="text-xs text-muted-foreground">{t("connect.toolsHint")}</span>
          <div className="mt-1 flex max-h-64 flex-col gap-1 overflow-y-auto rounded-md border p-2">
            {tools.map((tool) => (
              <label key={tool.name} className="flex items-start gap-2 rounded px-1 py-1 hover:bg-accent">
                <input
                  type="checkbox"
                  className="mt-0.5"
                  checked={chosen.has(tool.name)}
                  onChange={(e) =>
                    setChosen((set) => {
                      const next = new Set(set);
                      if (e.target.checked) next.add(tool.name);
                      else next.delete(tool.name);
                      return next;
                    })
                  }
                />
                <span className="flex min-w-0 flex-col">
                  <span className="font-mono text-xs" dir="ltr">
                    {tool.name}
                    {tool.read_only && <span className="ms-1.5 rounded border px-1 font-sans text-[10px] text-muted-foreground">{t("connect.readOnly")}</span>}
                  </span>
                  {tool.description && <span className="line-clamp-2 text-xs text-muted-foreground">{tool.description}</span>}
                </span>
              </label>
            ))}
          </div>
        </fieldset>
      )}
      <label className={field}>
        <span className={label}>{t("connect.scope")}</span>
        <Textarea value={scope} onChange={(e) => setScope(e.target.value)} className="min-h-20" />
      </label>
      <label className="flex items-start gap-3">
        <Switch checked={authoritative} onCheckedChange={setAuthoritative} className="mt-0.5" />
        <span className="flex flex-col gap-0.5">
          <span>{t("connect.authoritative")}</span>
          <span className="text-xs text-muted-foreground">{t("connect.authoritativeHint")}</span>
        </span>
      </label>
      <div className="flex flex-wrap gap-2">
        <Button type="submit" disabled={save.isPending || !server.trim() || (!existing && !name.trim())}>
          {existing ? t("connect.saveChanges") : t("connect.save")}
        </Button>
        {existing && (
          <Button type="button" variant="outline" disabled={remove.isPending} onClick={() => remove.mutate()}>
            {t("connect.remove")}
          </Button>
        )}
      </div>
    </form>
  );
}
