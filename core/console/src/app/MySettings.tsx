// A person's own settings (docs/15 §3): time zone, quiet hours, the push limit, reminders of due
// dates, their accounts in channels (Telegram), linked with a one-time code, and their passkeys
// (docs/28 §2), added through a one-time link.

import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Copy, ExternalLink } from "lucide-react";
import { useEffect, useState } from "react";
import { toast } from "sonner";
import { FromConfig, type Reminder, RemindersEditor, useReminderText } from "@/components/reminders";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Skeleton } from "@/components/ui/skeleton";
import { Switch } from "@/components/ui/switch";
import { useI18n } from "@/i18n";
import { api } from "@/lib/frood";

type From = "config" | "console" | "default";
type Setting<T> = { value: T; default: T; from: From };
type Link = { channel: string; external_id: string; label: string | null; linked_at: string };
type Mine = {
  email: string;
  timezone: Setting<string | null>;
  quiet_hours: Setting<string | null>;
  max_push_per_day: Setting<number | null>;
  reminders: Setting<Reminder[] | null>;
  channels: { channel: string; links: Link[]; start_url: string | null }[];
};
type Code = { code: string; channel: string; expires_at: string };
type Passkey = { id: string; label: string | null; created_at: string; last_used_at: string | null };
type PasskeyLink = { url: string; expires_at: string };

const here = Intl.DateTimeFormat().resolvedOptions().timeZone || "UTC";
const zones: string[] = typeof Intl.supportedValuesOf === "function" ? Intl.supportedValuesOf("timeZone") : [];
const channelName = (channel: string) => channel.charAt(0).toUpperCase() + channel.slice(1);

export function MySettings() {
  const mine = useQuery({ queryKey: ["my-settings"], queryFn: () => api<Mine>("/v1/me/settings") });
  if (mine.isError) return <p className="text-destructive">{mine.error.message}</p>;
  if (!mine.data) return <Skeleton className="h-96" />;
  return (
    <div className="flex flex-col gap-6">
      <PersonalForm key={JSON.stringify({ ...mine.data, channels: [] })} mine={mine.data} />
      <Channels channels={mine.data.channels} />
      <Passkeys />
    </div>
  );
}

function PersonalForm({ mine }: { mine: Mine }) {
  const { t } = useI18n();
  const text = useReminderText();
  const client = useQueryClient();
  const [zone, setZone] = useState(mine.timezone.value ?? "");
  const [quiet, setQuiet] = useState(!!mine.quiet_hours.value);
  const [from, to] = (mine.quiet_hours.value ?? "23:00-08:00").split("-");
  const [quietFrom, setQuietFrom] = useState(from);
  const [quietTo, setQuietTo] = useState(to);
  const [maxPush, setMaxPush] = useState(mine.max_push_per_day.value?.toString() ?? "");
  const [own, setOwn] = useState(mine.reminders.value !== null);
  const [reminders, setReminders] = useState<Reminder[]>(mine.reminders.value ?? mine.reminders.default ?? []);
  const fixed = (s: Setting<unknown>) => s.from === "config";
  const save = useMutation({
    mutationFn: () =>
      api("/v1/me/settings", {
        method: "PUT",
        json: {
          timezone: zone.trim() || null,
          quiet_hours: quiet ? `${quietFrom}-${quietTo}` : null,
          max_push_per_day: maxPush ? Number(maxPush) : null,
          reminders: own ? reminders : null,
        },
      }),
    onSuccess: () => {
      toast.success(t("me.saved"));
      client.invalidateQueries({ queryKey: ["my-settings"] });
      client.invalidateQueries({ queryKey: ["attention"] });
    },
    onError: (err) => toast.error(t("inbox.failed", { error: err.message })),
  });
  const label = "flex items-center gap-2 text-xs font-medium text-muted-foreground";
  const shared = mine.reminders.default ?? [];

  return (
    <form
      className="flex flex-col gap-5"
      onSubmit={(e) => {
        e.preventDefault();
        save.mutate();
      }}
    >
      <h2 className="text-lg font-semibold">{t("me.settings")}</h2>

      <div className="flex flex-col gap-1.5">
        <span className={label}>
          {t("me.timezone")} {fixed(mine.timezone) && <FromConfig />}
        </span>
        <Input
          value={zone}
          onChange={(e) => setZone(e.target.value)}
          list="frood-zones"
          disabled={fixed(mine.timezone)}
          placeholder={mine.timezone.default ? t("me.asEveryone", { value: mine.timezone.default }) : undefined}
          dir="ltr"
          aria-label={t("me.timezone")}
        />
        <datalist id="frood-zones">
          {zones.map((z) => (
            <option key={z} value={z} />
          ))}
        </datalist>
        <span className="flex flex-wrap items-center gap-2 text-xs text-muted-foreground">
          {t("me.timezoneHint")}
          {!fixed(mine.timezone) && zone !== here && (
            <Button type="button" variant="link" size="sm" className="h-auto p-0 text-xs" onClick={() => setZone(here)}>
              {t("me.useHere", { zone: here })}
            </Button>
          )}
        </span>
      </div>

      <div className="flex flex-col gap-2">
        <label className="flex items-center gap-3">
          <Switch checked={quiet} onCheckedChange={setQuiet} disabled={fixed(mine.quiet_hours)} />
          <span>{t("me.quiet")}</span>
          {fixed(mine.quiet_hours) && <FromConfig />}
        </label>
        {quiet && (
          <span className="flex items-center gap-2">
            <Input type="time" value={quietFrom} onChange={(e) => setQuietFrom(e.target.value)} disabled={fixed(mine.quiet_hours)} className="h-8 w-28" aria-label={t("me.from")} />
            <span aria-hidden>–</span>
            <Input type="time" value={quietTo} onChange={(e) => setQuietTo(e.target.value)} disabled={fixed(mine.quiet_hours)} className="h-8 w-28" aria-label={t("me.to")} />
          </span>
        )}
        <span className="text-xs text-muted-foreground">{t("me.quietHint")}</span>
      </div>

      <label className="flex flex-col gap-1.5">
        <span className={label}>
          {t("attention.maxPush")} {fixed(mine.max_push_per_day) && <FromConfig />}
        </span>
        <Input
          type="number"
          min={0}
          value={maxPush}
          onChange={(e) => setMaxPush(e.target.value)}
          disabled={fixed(mine.max_push_per_day)}
          placeholder={t("me.asEveryone", { value: String(mine.max_push_per_day.default ?? 30) })}
          className="w-56"
        />
      </label>

      <div className="flex flex-col gap-2">
        <span className={label}>
          {t("attention.reminders")} {fixed(mine.reminders) && <FromConfig />}
        </span>
        <label className="flex items-center gap-3">
          <Switch checked={own} onCheckedChange={setOwn} disabled={fixed(mine.reminders)} />
          <span>{t("me.remindersOwn")}</span>
        </label>
        {own ? (
          <RemindersEditor value={reminders} onChange={setReminders} disabled={fixed(mine.reminders)} />
        ) : (
          <p className="text-muted-foreground">{shared.length ? t("me.asEveryone", { value: shared.map(text).join("; ") }) : t("attention.remindersNone")}</p>
        )}
      </div>

      {!zone.trim() && !mine.timezone.default && (quiet || (own ? reminders.length > 0 : shared.length > 0)) && <p className="rounded-md border border-warning/50 p-3 text-warning">{t("me.needsZone")}</p>}

      <Button type="submit" className="self-start" disabled={save.isPending}>
        {t("connect.saveChanges")}
      </Button>
    </form>
  );
}

function Channels({ channels }: { channels: Mine["channels"] }) {
  const { t, ago } = useI18n();
  const client = useQueryClient();
  // The code shown, with how many accounts of its channel were linked before it.
  const [code, setCode] = useState<(Code & { before: number }) | null>(null);
  const count = (channel: string) => channels.find((c) => c.channel === channel)?.links.length ?? 0;
  const failed = (err: Error) => toast.error(t("inbox.failed", { error: err.message }));
  const link = useMutation({
    mutationFn: (channel: string) => api<Code>(`/v1/channels/${encodeURIComponent(channel)}/link-codes`, { method: "POST" }),
    onSuccess: (c) => setCode({ ...c, before: count(c.channel) }),
    onError: failed,
  });
  // While a code waits to be sent to the bot, look for the account it links.
  useEffect(() => {
    if (!code) return;
    const timer = setInterval(() => client.invalidateQueries({ queryKey: ["my-settings"] }), 3000);
    const expired = setTimeout(() => setCode(null), new Date(code.expires_at).getTime() - Date.now());
    return () => {
      clearInterval(timer);
      clearTimeout(expired);
    };
  }, [code, client]);
  useEffect(() => {
    if (code && count(code.channel) > code.before) {
      toast.success(t("me.linked", { channel: channelName(code.channel) }));
      setCode(null);
    }
  });
  const unlink = useMutation({
    mutationFn: (l: Link) => api(`/v1/me/links/${encodeURIComponent(l.channel)}/${encodeURIComponent(l.external_id)}`, { method: "DELETE" }),
    onSuccess: () => {
      toast.success(t("me.unlinked"));
      client.invalidateQueries({ queryKey: ["my-settings"] });
    },
    onError: failed,
  });
  const command = code ? `/link ${code.code}` : "";

  return (
    <section className="flex flex-col gap-3 border-t pt-5">
      <h3 className="font-medium">{t("me.channels")}</h3>
      {channels.length === 0 && <p className="text-muted-foreground">{t("me.channelsNone")}</p>}
      {channels.map(({ channel, links, start_url }) => (
        <div key={channel} className="flex flex-col gap-2 rounded-xl border p-3">
          <span className="font-medium">{channelName(channel)}</span>
          {links.length === 0 && <span className="text-muted-foreground">{t("me.notLinked")}</span>}
          {links.map((l) => (
            <span key={l.external_id} className="flex flex-wrap items-center gap-2">
              <span>{l.label ?? l.external_id}</span>
              <span className="text-xs text-muted-foreground">{t("me.linkedAt", { when: ago(l.linked_at) })}</span>
              <Button type="button" size="sm" variant="ghost" className="ms-auto h-7 px-2 text-xs" disabled={unlink.isPending} onClick={() => unlink.mutate(l)}>
                {t("me.unlink")}
              </Button>
            </span>
          ))}
          {code?.channel === channel ? (
            <div className="flex flex-col gap-2 rounded-md bg-secondary p-3">
              {start_url && (
                <Button asChild className="self-start">
                  <a href={start_url + encodeURIComponent(code.code)} target="_blank" rel="noreferrer">
                    <ExternalLink className="size-4" /> {t("me.openChannel", { channel: channelName(channel) })}
                  </a>
                </Button>
              )}
              <span>{start_url ? t("me.linkOr") : t("me.linkSteps")}</span>
              <span className="flex items-center gap-2">
                <code className="rounded bg-background px-2 py-1 font-mono text-base" dir="ltr">
                  {command}
                </code>
                <Button
                  type="button"
                  size="sm"
                  variant="outline"
                  onClick={() => navigator.clipboard.writeText(command).then(() => toast.success(t("me.copied")), failed)}
                  aria-label={t("me.copy")}
                >
                  <Copy className="size-3.5" /> {t("me.copy")}
                </Button>
              </span>
              <span className="text-xs text-muted-foreground">{t("me.linkThen")}</span>
            </div>
          ) : (
            <Button type="button" size="sm" variant="outline" className="self-start" disabled={link.isPending} onClick={() => link.mutate(channel)}>
              {links.length ? t("me.linkAnother") : t("me.link")}
            </Button>
          )}
        </div>
      ))}
    </section>
  );
}

function Passkeys() {
  const { t, ago } = useI18n();
  const client = useQueryClient();
  const keys = useQuery({ queryKey: ["my-passkeys"], queryFn: () => api<{ on: boolean; passkeys: Passkey[] }>("/v1/me/passkeys") });
  const list = keys.data?.passkeys ?? [];
  // The link shown, with how many passkeys there were before it.
  const [link, setLink] = useState<(PasskeyLink & { before: number }) | null>(null);
  const failed = (err: Error) => toast.error(t("inbox.failed", { error: err.message }));
  const add = useMutation({
    mutationFn: () => api<PasskeyLink>("/v1/me/passkeys/links", { method: "POST" }),
    onSuccess: (l) => setLink({ ...l, before: list.length }),
    onError: failed,
  });
  // While the link waits to be opened, look for the passkey it adds.
  useEffect(() => {
    if (!link) return;
    const timer = setInterval(() => client.invalidateQueries({ queryKey: ["my-passkeys"] }), 3000);
    const expired = setTimeout(() => setLink(null), new Date(link.expires_at).getTime() - Date.now());
    return () => {
      clearInterval(timer);
      clearTimeout(expired);
    };
  }, [link, client]);
  useEffect(() => {
    if (link && list.length > link.before) {
      toast.success(t("me.passkeyDone"));
      setLink(null);
    }
  });
  const remove = useMutation({
    mutationFn: (id: string) => api(`/v1/me/passkeys/${encodeURIComponent(id)}`, { method: "DELETE" }),
    onSuccess: () => {
      toast.success(t("me.passkeyRemoved"));
      client.invalidateQueries({ queryKey: ["my-passkeys"] });
    },
    onError: failed,
  });
  if (!keys.data?.on) return null;

  return (
    <section className="flex flex-col gap-3 border-t pt-5">
      <h3 className="font-medium">{t("me.passkeys")}</h3>
      <span className="text-xs text-muted-foreground">{t("me.passkeysHint")}</span>
      {list.length === 0 && <p className="text-muted-foreground">{t("me.passkeysNone")}</p>}
      {list.map((k) => (
        <span key={k.id} className="flex flex-wrap items-center gap-2">
          <span>{k.label ?? t("me.passkeyUnnamed")}</span>
          <span className="text-xs text-muted-foreground">
            {k.last_used_at ? t("me.passkeyUsed", { when: ago(k.last_used_at) }) : t("me.passkeyAdded", { when: ago(k.created_at) })}
          </span>
          <Button type="button" size="sm" variant="ghost" className="ms-auto h-7 px-2 text-xs" disabled={remove.isPending} onClick={() => remove.mutate(k.id)}>
            {t("me.passkeyRemove")}
          </Button>
        </span>
      ))}
      {link ? (
        <div className="flex flex-col gap-2 rounded-md bg-secondary p-3">
          <span className="flex flex-wrap items-center gap-2">
            <Button asChild>
              <a href={link.url} target="_blank" rel="noreferrer">
                <ExternalLink className="size-4" /> {t("me.passkeyOpen")}
              </a>
            </Button>
            <Button
              type="button"
              variant="outline"
              onClick={() => navigator.clipboard.writeText(link.url).then(() => toast.success(t("me.copied")), failed)}
            >
              <Copy className="size-3.5" /> {t("me.passkeyCopy")}
            </Button>
          </span>
          <span className="text-xs text-muted-foreground">{t("me.passkeyThen")}</span>
        </div>
      ) : (
        <Button type="button" size="sm" variant="outline" className="self-start" disabled={add.isPending} onClick={() => add.mutate()}>
          {list.length ? t("me.passkeyAnother") : t("me.passkeyAdd")}
        </Button>
      )}
    </section>
  );
}
