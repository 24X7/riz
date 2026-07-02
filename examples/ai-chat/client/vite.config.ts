import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// The build lands in `dist/`, which riz serves via the [static] block in
// ../riz.toml. `server.proxy` lets `bun run dev` (Vite on :5173) talk to a riz
// instance on :3000 during development — same `/api/*` calls in dev and prod.
export default defineConfig({
  base: "/",
  plugins: [react()],
  build: {
    outDir: "dist",
    emptyOutDir: true,
  },
  server: {
    proxy: {
      "/api": "http://localhost:3000",
    },
  },
});
