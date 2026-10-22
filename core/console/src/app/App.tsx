import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { ThemeProvider } from "next-themes";
import { useEffect, useState } from "react";
import { createBrowserRouter, Navigate, RouterProvider } from "react-router";
import { Toaster } from "@/components/ui/sonner";
import { TooltipProvider } from "@/components/ui/tooltip";
import { I18nProvider, useI18n } from "@/i18n";
import { BASE, finishSignIn, signIn } from "@/lib/frood";
import { Automations } from "@/pages/Automations";
import { Inbox } from "@/pages/Inbox";
import { Knowledge } from "@/pages/Knowledge";
import { Now } from "@/pages/Now";
import { Sources } from "@/pages/Sources";
import { System } from "@/pages/System";
import { Shell } from "./Shell";

const queries = new QueryClient({
  defaultOptions: { queries: { staleTime: 15_000, retry: 1 } },
});

const router = createBrowserRouter(
  [
    {
      element: <Shell />,
      children: [
        { index: true, element: <Navigate to="now" replace /> },
        { path: "now", element: <Now /> },
        { path: "inbox/:id?", element: <Inbox /> },
        { path: "knowledge/:id?", element: <Knowledge /> },
        { path: "sources", element: <Sources /> },
        { path: "automations", element: <Automations /> },
        { path: "system", element: <System /> },
        { path: "*", element: <Navigate to="now" replace /> },
      ],
    },
  ],
  { basename: `${BASE}/admin` },
);

/** Finishes a sign-in that came back with a code, or starts one when there is no token. */
function SignedIn({ children }: { children: React.ReactNode }) {
  const { t } = useI18n();
  const [ready, setReady] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    finishSignIn()
      .then((back) => {
        if (back && back !== location.href) {
          location.replace(back);
          return;
        }
        const signedIn = import.meta.env.VITE_FROOD_TOKEN || sessionStorage.getItem("tokens");
        if (!signedIn) return signIn();
        setReady(true);
      })
      .catch((err: unknown) => setError(String(err)));
  }, []);

  if (error) return <p className="p-8 text-destructive">{error}</p>;
  if (!ready) return <p className="p-8 text-muted-foreground">{t("app.signingIn")}</p>;
  return children;
}

export function App() {
  return (
    <ThemeProvider attribute="class" defaultTheme="system" enableSystem disableTransitionOnChange>
      <I18nProvider>
        <QueryClientProvider client={queries}>
          <TooltipProvider delayDuration={300}>
            <SignedIn>
              <RouterProvider router={router} />
            </SignedIn>
            <Toaster position="bottom-center" />
          </TooltipProvider>
        </QueryClientProvider>
      </I18nProvider>
    </ThemeProvider>
  );
}
