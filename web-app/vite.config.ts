import path from "path";
import { defineConfig } from "vite";
import tailwindcss from "@tailwindcss/vite";
import react from "@vitejs/plugin-react";

// https://vitejs.dev/config/
export default defineConfig({
  plugins: [react(), tailwindcss()],
  resolve: {
    alias: {
      "@": path.resolve(__dirname, "./src"),
    },
  },
  // Assets are emitted as separate content-hashed files (not inlined into one HTML file) so
  // they can be cached immutably; index.html stays small and is revalidated on each load.
  base: "/static/react/",
  build: {
    outDir: process.cwd() + "/static/react/assets",
    assetsDir: "",
  },
});
