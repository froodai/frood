import { CircleHelp } from "lucide-react";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { useI18n } from "@/i18n";
import { kindKey, kindTone } from "@/lib/kinds";
import { TONE, type Type, typeKey } from "@/lib/situations";
import { sphereIcon, useSphereName, useSpheres } from "@/lib/spheres";
import { cn } from "@/lib/utils";

const BADGE = "inline-flex h-5 items-center gap-1 rounded-md px-1.5 text-[11px] font-medium whitespace-nowrap";

/** The kind of knowledge: decision, fact, … — filled with its colour. */
export function KindBadge({ kind, className }: { kind: string; className?: string }) {
  const { t } = useI18n();
  const tone = kindTone(kind);
  const Icon = tone.icon;
  return (
    <span className={cn(BADGE, tone.badge, className)}>
      <Icon className="size-3" aria-hidden />
      {t(kindKey(kind))}
    </span>
  );
}

/** What happened to it: new, changed, in conflict, … */
export function TypeBadge({ type }: { type: Type }) {
  const { t } = useI18n();
  return <span className={cn(BADGE, TONE[type].badge)}>{t(typeKey(type))}</span>;
}

/** Taken without a person's check: an agent's finding, an assistant's memory (docs/27). */
export function UnverifiedBadge({ className }: { className?: string }) {
  const { t } = useI18n();
  return (
    <span className={cn(BADGE, "border border-dashed border-amber-500/60 text-amber-700 dark:text-amber-300", className)} title={t("knowledge.unverifiedHint")}>
      <CircleHelp className="size-3" aria-hidden />
      {t("knowledge.unverified")}
    </span>
  );
}

/** The sphere of life it belongs to: quiet next to the kind, with its sign. */
export function SphereBadge({ sphere, className }: { sphere: string | null | undefined; className?: string }) {
  const name = useSphereName();
  if (!sphere) return null;
  const Icon = sphereIcon(sphere);
  return (
    <span className={cn(BADGE, "border text-muted-foreground", className)}>
      <Icon className="size-3" aria-hidden />
      {name(sphere)}
    </span>
  );
}

/** Spheres to choose from: the installation's, the current one even when it was removed since. */
function useChoices(current: string | null) {
  const spheres = useSpheres().data ?? [];
  const ids = spheres.map((s) => s.id);
  return current && current !== "none" && current !== "all" && !ids.includes(current) ? [...ids, current] : ids;
}

/** Puts knowledge in a sphere, or out of any. Hidden when the installation has no spheres. */
export function SpherePicker({ value, onChange, disabled }: { value: string | null; onChange: (sphere: string | null) => void; disabled?: boolean }) {
  const { t } = useI18n();
  const name = useSphereName();
  const choices = useChoices(value);
  if (choices.length === 0) return null;
  return (
    <Select value={value ?? "none"} onValueChange={(v) => onChange(v === "none" ? null : v)} disabled={disabled}>
      <SelectTrigger size="sm" className="h-7 w-auto gap-1.5 px-2 text-xs" aria-label={t("sphere.label")}>
        <SelectValue />
      </SelectTrigger>
      <SelectContent>
        {choices.map((id) => {
          const Icon = sphereIcon(id);
          return (
            <SelectItem key={id} value={id}>
              <Icon className="size-3.5 text-muted-foreground" aria-hidden />
              {name(id)}
            </SelectItem>
          );
        })}
        <SelectItem value="none">{t("sphere.none")}</SelectItem>
      </SelectContent>
    </Select>
  );
}

/** Narrows a list to one sphere: `all`, a sphere id, or `none`. */
export function SphereFilter({ value, onChange }: { value: string; onChange: (sphere: string) => void }) {
  const { t } = useI18n();
  const name = useSphereName();
  const choices = useChoices(value);
  if (choices.length === 0) return null;
  return (
    <Select value={value} onValueChange={onChange}>
      <SelectTrigger size="sm" className="w-auto min-w-36" aria-label={t("sphere.label")}>
        <SelectValue />
      </SelectTrigger>
      <SelectContent>
        <SelectItem value="all">{t("sphere.all")}</SelectItem>
        {choices.map((id) => {
          const Icon = sphereIcon(id);
          return (
            <SelectItem key={id} value={id}>
              <Icon className="size-3.5 text-muted-foreground" aria-hidden />
              {name(id)}
            </SelectItem>
          );
        })}
        <SelectItem value="none">{t("sphere.none")}</SelectItem>
      </SelectContent>
    </Select>
  );
}
