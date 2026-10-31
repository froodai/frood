import { definePluginEntry } from "openclaw/plugin-sdk/plugin-entry";

import { registerFrood } from "./plugin.js";

export default definePluginEntry({
  id: "frood",
  name: "Memory (Frood)",
  description: "Frood as OpenClaw's memory: knowledge with sources and history, before each turn and as memory tools.",
  register: registerFrood,
});
