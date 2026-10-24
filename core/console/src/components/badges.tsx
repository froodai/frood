import { useI18n } from "@/i18n";
import { kindKey, kindTone } from "@/lib/kinds";
import { TONE, type Type, typeKey } from "@/lib/situations";
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
