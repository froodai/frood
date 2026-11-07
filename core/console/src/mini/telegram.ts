// Telegram's side of the Mini App (docs/32 M4): its script, theme, back and main buttons,
// haptics and links that open in the browser without closing the Mini App.

type Haptic = {
  impactOccurred: (style: "light" | "medium" | "heavy" | "rigid" | "soft") => void;
  notificationOccurred: (type: "error" | "success" | "warning") => void;
  selectionChanged: () => void;
};

type BottomButton = {
  setText: (text: string) => BottomButton;
  show: () => BottomButton;
  hide: () => BottomButton;
  enable: () => BottomButton;
  disable: () => BottomButton;
  showProgress: (leaveActive?: boolean) => BottomButton;
  hideProgress: () => BottomButton;
  onClick: (callback: () => void) => BottomButton;
  offClick: (callback: () => void) => BottomButton;
};

export type WebApp = {
  initData: string;
  initDataUnsafe: {
    user?: { id: number; language_code?: string };
    start_param?: string;
  };
  colorScheme: "light" | "dark";
  themeParams: Record<string, string | undefined>;
  ready: () => void;
  expand: () => void;
  openLink: (url: string) => void;
  onEvent: (event: string, callback: () => void) => void;
  BackButton: {
    show: () => void;
    hide: () => void;
    onClick: (cb: () => void) => void;
    offClick: (cb: () => void) => void;
  };
  MainButton: BottomButton;
  HapticFeedback: Haptic;
};

declare global {
  interface Window {
    Telegram?: { WebApp?: WebApp };
  }
}

const SCRIPT = "https://telegram.org/js/telegram-web-app.js";

/** Telegram's web app, when the page runs inside Telegram. */
export function tg(): WebApp | null {
  const app = window.Telegram?.WebApp;
  return app && app.initData ? app : null;
}

/** Loads Telegram's script; resolves without it when it cannot load (a plain browser offline). */
export function loadTelegram(): Promise<void> {
  if (window.Telegram?.WebApp) return Promise.resolve();
  return new Promise((resolve) => {
    const script = document.createElement("script");
    script.src = SCRIPT;
    script.onload = () => resolve();
    script.onerror = () => resolve();
    document.head.appendChild(script);
  });
}

/**
 * Telegram's colours for the console's own variables, so the Mini App looks like the rest of
 * Telegram in the person's theme. Missing colours keep the console's.
 */
export function applyTheme(app: WebApp) {
  const p = app.themeParams;
  const root = document.documentElement;
  root.classList.toggle("dark", app.colorScheme === "dark");
  const set = (name: string, value: string | undefined) => {
    if (value) root.style.setProperty(name, value);
  };
  set("--background", p.secondary_bg_color ?? p.bg_color);
  set("--foreground", p.text_color);
  set("--card", p.section_bg_color ?? p.bg_color);
  set("--card-foreground", p.text_color);
  set("--popover", p.section_bg_color ?? p.bg_color);
  set("--popover-foreground", p.text_color);
  set("--muted-foreground", p.hint_color ?? p.subtitle_text_color);
  set("--primary", p.button_color);
  set("--primary-foreground", p.button_text_color);
  set("--ring", p.button_color);
  set("--destructive", p.destructive_text_color);
  set("--border", p.section_separator_color);
  set("--input", p.section_separator_color);
  set("--sidebar", p.bg_color);
}

/** Starts the Mini App: full height, Telegram's theme, links out to the browser. */
export function startTelegram(app: WebApp) {
  app.ready();
  app.expand();
  applyTheme(app);
  app.onEvent("themeChanged", () => applyTheme(app));
  // Web links open in the browser and the Mini App stays (M4).
  document.addEventListener("click", (event) => {
    const link = (event.target as HTMLElement | null)?.closest<HTMLAnchorElement>("a[href]");
    if (!link || link.origin === location.origin || !/^https?:$/.test(link.protocol)) return;
    event.preventDefault();
    app.openLink(link.href);
  });
}

/** A tap that the person should feel: a decision went through, or failed. */
export function feel(kind: "success" | "error" | "select") {
  const haptic = tg()?.HapticFeedback;
  if (!haptic) return;
  if (kind === "select") haptic.selectionChanged();
  else haptic.notificationOccurred(kind);
}
