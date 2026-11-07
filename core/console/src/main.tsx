import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { App } from "@/app/App";
import { MINI_APP } from "@/lib/frood";
import { loadTelegram } from "@/mini/telegram";
import "./index.css";

// The Mini App needs Telegram's script (its launch data, theme and buttons) before it starts.
(MINI_APP ? loadTelegram() : Promise.resolve()).then(() =>
  createRoot(document.getElementById("root")!).render(
    <StrictMode>
      <App />
    </StrictMode>,
  ),
);
