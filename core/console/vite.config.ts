import path from "node:path";
import tailwindcss from "@tailwindcss/vite";
import react from "@vitejs/plugin-react";
import { defineConfig } from "vite";

// The server serves the build under `<base>/admin/` and puts a <base href> into index.html,
// so assets are referenced relatively and deep links work under any path.
const frood = process.env.FROOD_URL ?? "http://127.0.0.1:8080";

export default defineConfig({
  base: "./",
  plugins: [react(), tailwindcss()],
  resolve: { alias: { "@": path.resolve(__dirname, "src") } },
  server: {
    proxy: Object.fromEntries(["/v1", "/oauth", "/.well-known"].map((p) => [p, frood])),
  },
  build: { outDir: "dist", emptyOutDir: true, chunkSizeWarningLimit: 900 },
});
