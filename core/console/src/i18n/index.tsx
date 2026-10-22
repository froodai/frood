// Interface languages (docs/25 §3.4). English is the source: every other catalog has the same
// keys, and a key a catalog lacks falls back to English (a test keeps catalogs complete).

import { createContext, useCallback, useContext, useEffect, useMemo, useState, type ReactNode } from "react";
import { ar } from "./ar";
import { en, type Key, type Message } from "./en";
import { es } from "./es";
import { fr } from "./fr";
import { hi } from "./hi";
import { pt } from "./pt";
import { ru } from "./ru";
import { zh } from "./zh";

export type Catalog = Partial<Record<Key, Message>>;

export const LANGUAGES = [
  { code: "en", name: "English", catalog: en as Catalog },
  { code: "zh", name: "中文", catalog: zh },
  { code: "es", name: "Español", catalog: es },
  { code: "hi", name: "हिन्दी", catalog: hi },
  { code: "ar", name: "العربية", catalog: ar, rtl: true },
  { code: "fr", name: "Français", catalog: fr },
  { code: "pt", name: "Português", catalog: pt },
  { code: "ru", name: "Русский", catalog: ru },
] as const;

export type Lang = (typeof LANGUAGES)[number]["code"];
type Vars = Record<string, string | number>;

const STORAGE = "frood.lang";

function detect(): Lang {
  const known = LANGUAGES.map((l) => l.code as string);
  let saved: string | null = null;
  try {
    saved = localStorage.getItem(STORAGE);
  } catch {
    // Storage can be off; the browser's language then decides.
  }
  if (saved && known.includes(saved)) return saved as Lang;
  for (const tag of navigator.languages ?? [navigator.language]) {
    const code = tag.toLowerCase().split("-")[0];
    if (known.includes(code)) return code as Lang;
  }
  return "en";
}

function format(lang: Lang, message: Message, vars: Vars = {}): string {
  let text: string;
  if (typeof message === "string") {
    text = message;
  } else {
    const n = Number(vars.n ?? 0);
    const rule = new Intl.PluralRules(lang).select(n);
    text = message[rule] ?? message.other;
  }
  return text.replace(/\{(\w+)\}/g, (_, name: string) =>
    name in vars ? (typeof vars[name] === "number" ? new Intl.NumberFormat(lang).format(vars[name]) : String(vars[name])) : `{${name}}`,
  );
}

type I18n = {
  lang: Lang;
  setLang: (lang: Lang) => void;
  t: (key: Key, vars?: Vars) => string;
  /** "5 minutes ago", "in 2 days". */
  ago: (iso: string | null | undefined) => string;
  date: (iso: string | null | undefined, style?: "short" | "long") => string;
};

const Context = createContext<I18n | null>(null);

export function I18nProvider({ children }: { children: ReactNode }) {
  const [lang, setLangState] = useState<Lang>(detect);
  const language = LANGUAGES.find((l) => l.code === lang)!;

  useEffect(() => {
    document.documentElement.lang = lang;
    document.documentElement.dir = "rtl" in language && language.rtl ? "rtl" : "ltr";
  }, [lang, language]);

  const setLang = useCallback((next: Lang) => {
    try {
      localStorage.setItem(STORAGE, next);
    } catch {
      // Not remembered, still switched.
    }
    setLangState(next);
  }, []);

  const value = useMemo<I18n>(() => {
    const catalog = language.catalog;
    const relative = new Intl.RelativeTimeFormat(lang, { numeric: "auto" });
    return {
      lang,
      setLang,
      t: (key, vars) => format(lang, catalog[key] ?? en[key], vars),
      ago: (iso) => {
        if (!iso) return "";
        const seconds = (new Date(iso).getTime() - Date.now()) / 1000;
        const abs = Math.abs(seconds);
        if (abs < 60) return relative.format(Math.round(seconds), "second");
        if (abs < 3600) return relative.format(Math.round(seconds / 60), "minute");
        if (abs < 86400) return relative.format(Math.round(seconds / 3600), "hour");
        if (abs < 86400 * 45) return relative.format(Math.round(seconds / 86400), "day");
        return new Intl.DateTimeFormat(lang, { dateStyle: "medium" }).format(new Date(iso));
      },
      date: (iso, style = "short") =>
        iso
          ? new Intl.DateTimeFormat(lang, style === "short" ? { day: "numeric", month: "short" } : { dateStyle: "long" }).format(new Date(iso))
          : "",
    };
  }, [lang, language, setLang]);

  return <Context.Provider value={value}>{children}</Context.Provider>;
}

export function useI18n(): I18n {
  const value = useContext(Context);
  if (!value) throw new Error("useI18n outside I18nProvider");
  return value;
}
