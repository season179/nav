import { fileURLToPath } from "node:url";
import tailwindcss from "@tailwindcss/vite";
import react from "@vitejs/plugin-react";
import { defineConfig } from "vite";

const rendererRoot = fileURLToPath(new URL(".", import.meta.url));

export default defineConfig({
  root: rendererRoot,
  base: "./",
  plugins: [react(), tailwindcss()],
  resolve: {
    alias: {
      "@": fileURLToPath(new URL("./src", import.meta.url)),
    },
  },
  build: {
    emptyOutDir: true,
    outDir: "dist",
    rolldownOptions: {
      output: {
        codeSplitting: true,
      },
    },
    sourcemap: true,
  },
});
