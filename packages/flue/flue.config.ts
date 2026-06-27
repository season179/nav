import { defineConfig } from "@flue/cli/config";

export default defineConfig({
  target: "node",
});

export const vite = {
  server: {
    watch: {
      ignored: ["**/data/**"],
    },
  },
};
