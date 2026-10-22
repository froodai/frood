import { Check, Languages, LogOut, Menu, Monitor, Moon, Search, Sun } from "lucide-react";
import { useTheme } from "next-themes";
import { useState } from "react";
import { NavLink, Outlet, useLocation } from "react-router";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuSeparator,
  DropdownMenuSub,
  DropdownMenuSubContent,
  DropdownMenuSubTrigger,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { Sheet, SheetContent, SheetTitle } from "@/components/ui/sheet";
import { LANGUAGES, useI18n } from "@/i18n";
import { signOut } from "@/lib/frood";
import { useHealth, useInbox, useMe, waiting } from "@/lib/queries";
import { cn } from "@/lib/utils";
import { CommandPalette } from "./CommandPalette";
import { SECTIONS } from "./sections";

function useOpenCount() {
  const inbox = useInbox();
  return waiting(inbox.data).length;
}

function Nav({ onPick }: { onPick?: () => void }) {
  const { t } = useI18n();
  const open = useOpenCount();
  return (
    <nav className="flex flex-col gap-0.5" aria-label="Sections">
      {SECTIONS.map(({ path, icon: Icon, title }) => (
        <NavLink
          key={path}
          to={path}
          onClick={onPick}
          className={({ isActive }) =>
            cn(
              "flex min-h-9 items-center gap-2.5 rounded-md px-2.5 text-sm transition-colors",
              isActive ? "bg-accent font-medium text-foreground" : "text-muted-foreground hover:bg-accent/60 hover:text-foreground",
            )
          }
        >
          <Icon className="size-4" aria-hidden />
          <span className="flex-1">{t(title)}</span>
          {path === "inbox" && open > 0 && (
            <span className="min-w-5 rounded-full bg-primary px-1.5 text-center text-xs font-semibold text-primary-foreground">{open}</span>
          )}
        </NavLink>
      ))}
    </nav>
  );
}

function Account() {
  const { t, lang, setLang } = useI18n();
  const { theme, setTheme } = useTheme();
  const me = useMe();
  const email = me.data?.email ?? "";
  return (
    <DropdownMenu>
      <DropdownMenuTrigger className="flex w-full items-center gap-2.5 rounded-md px-2 py-2 text-start hover:bg-accent/60">
        <span className="grid size-7 place-items-center rounded-full bg-secondary text-xs font-semibold uppercase">{email.slice(0, 1) || "·"}</span>
        <span className="min-w-0 flex-1 truncate text-sm">{email}</span>
      </DropdownMenuTrigger>
      <DropdownMenuContent align="start" className="w-60">
        <DropdownMenuLabel className="truncate font-normal text-muted-foreground">{email}</DropdownMenuLabel>
        <DropdownMenuSeparator />
        <DropdownMenuSub>
          <DropdownMenuSubTrigger>
            <Languages className="size-4" /> {t("app.language")}
          </DropdownMenuSubTrigger>
          <DropdownMenuSubContent>
            {LANGUAGES.map((l) => (
              <DropdownMenuItem key={l.code} onSelect={() => setLang(l.code)} lang={l.code}>
                <span className="flex-1">{l.name}</span>
                {l.code === lang && <Check className="size-4" />}
              </DropdownMenuItem>
            ))}
          </DropdownMenuSubContent>
        </DropdownMenuSub>
        <DropdownMenuSub>
          <DropdownMenuSubTrigger>
            <Sun className="size-4" /> {t("app.theme")}
          </DropdownMenuSubTrigger>
          <DropdownMenuSubContent>
            {(
              [
                ["light", Sun, "app.theme.light"],
                ["dark", Moon, "app.theme.dark"],
                ["system", Monitor, "app.theme.system"],
              ] as const
            ).map(([value, Icon, label]) => (
              <DropdownMenuItem key={value} onSelect={() => setTheme(value)}>
                <Icon className="size-4" />
                <span className="flex-1">{t(label)}</span>
                {theme === value && <Check className="size-4" />}
              </DropdownMenuItem>
            ))}
          </DropdownMenuSubContent>
        </DropdownMenuSub>
        <DropdownMenuSeparator />
        <DropdownMenuItem onSelect={signOut}>
          <LogOut className="size-4" /> {t("app.signOut")}
        </DropdownMenuItem>
      </DropdownMenuContent>
    </DropdownMenu>
  );
}

function Status() {
  const { t } = useI18n();
  const health = useHealth();
  if (!health.data) return null;
  const h = health.data;
  const problems =
    h.modules.filter((m) => !m.up).length +
    h.alerts_open +
    (h.backups?.last_backup && h.backups.last_backup.status !== "succeeded" ? 1 : 0);
  return (
    <NavLink
      to="system"
      className={cn(
        "inline-flex items-center gap-1.5 rounded-full px-2.5 py-0.5 text-xs font-medium",
        problems ? "bg-warning-soft text-warning" : "bg-success-soft text-success",
      )}
    >
      <span className={cn("size-1.5 rounded-full", problems ? "bg-warning" : "bg-success")} />
      {problems ? t("app.problems", { n: problems }) : t("app.allGood")}
    </NavLink>
  );
}

export function Shell() {
  const { t } = useI18n();
  const location = useLocation();
  const [palette, setPalette] = useState(false);
  const [more, setMore] = useState(false);
  const section = SECTIONS.find((s) => location.pathname.split("/").includes(s.path)) ?? SECTIONS[0];

  return (
    <div className="flex min-h-dvh">
      <aside className="sticky top-0 hidden h-dvh w-60 shrink-0 flex-col gap-3 border-e bg-sidebar px-3 py-4 md:flex">
        <div className="flex items-center gap-2.5 px-2 pb-2">
          <span className="grid size-7 place-items-center rounded-md bg-primary font-semibold text-primary-foreground">F</span>
          <span className="font-semibold">Frood</span>
        </div>
        <button
          type="button"
          onClick={() => setPalette(true)}
          className="flex min-h-9 items-center gap-2 rounded-md border bg-background px-2.5 text-sm text-muted-foreground hover:text-foreground"
        >
          <Search className="size-4" aria-hidden />
          <span className="flex-1 text-start">{t("app.search")}</span>
          <kbd className="rounded border px-1 font-mono text-[11px]">⌘K</kbd>
        </button>
        <Nav />
        <div className="flex-1" />
        <Account />
      </aside>

      <div className="flex min-w-0 flex-1 flex-col pb-16 md:pb-0">
        <header className="sticky top-0 z-10 flex min-h-14 flex-wrap items-center gap-x-3 gap-y-1 border-b bg-background/85 px-4 py-2 backdrop-blur md:px-7">
          <h1 className="text-base font-semibold">{t(section.title)}</h1>
          <span className="hidden text-sm text-muted-foreground sm:inline">{t(section.sub)}</span>
          <div className="flex-1" />
          <Status />
        </header>
        <main className="flex min-h-0 flex-1 flex-col">
          <Outlet />
        </main>
      </div>

      {/* Phones: the main sections at the bottom, the rest under "More". */}
      <nav className="fixed inset-x-0 bottom-0 z-20 grid grid-cols-5 border-t bg-background md:hidden" aria-label="Sections">
        {SECTIONS.slice(0, 3).map(({ path, icon: Icon, title }) => (
          <NavLink
            key={path}
            to={path}
            className={({ isActive }) =>
              cn("flex min-h-14 flex-col items-center justify-center gap-0.5 text-[11px]", isActive ? "text-primary" : "text-muted-foreground")
            }
          >
            <Icon className="size-5" aria-hidden />
            {t(title)}
          </NavLink>
        ))}
        <button type="button" onClick={() => setPalette(true)} className="flex flex-col items-center justify-center gap-0.5 text-[11px] text-muted-foreground">
          <Search className="size-5" aria-hidden />
          {t("app.search")}
        </button>
        <button type="button" onClick={() => setMore(true)} className="flex flex-col items-center justify-center gap-0.5 text-[11px] text-muted-foreground">
          <Menu className="size-5" aria-hidden />
          {t("app.more")}
        </button>
      </nav>
      <Sheet open={more} onOpenChange={setMore}>
        <SheetContent side="bottom" className="gap-3 p-4">
          <SheetTitle>{t("app.more")}</SheetTitle>
          <Nav onPick={() => setMore(false)} />
          <Account />
        </SheetContent>
      </Sheet>

      <CommandPalette open={palette} onOpenChange={setPalette} />
    </div>
  );
}
