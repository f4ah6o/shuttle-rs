import { defineConfig } from "vitest/config";

export default defineConfig({
  test: {
    environment: "node",
    include: ["test/**/*.test.ts"],
    // node:sqlite is a recent built-in that Vite does not yet treat as
    // external; keep it out of the transform pipeline.
    server: {
      deps: {
        external: [/node:sqlite/],
      },
    },
  },
  ssr: {
    external: ["node:sqlite"],
  },
});
