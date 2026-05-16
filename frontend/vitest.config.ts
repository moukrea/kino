import { defineConfig, mergeConfig } from "vitest/config";
import solid from "vite-plugin-solid";
import viteConfig from "./vite.config";

export default mergeConfig(
  viteConfig,
  defineConfig({
    plugins: [solid()],
    test: {
      globals: true,
      environment: "jsdom",
      setupFiles: ["./src/test-setup.ts"],
      include: ["src/**/*.{test,spec}.{ts,tsx}"],
      coverage: {
        provider: "v8",
        reporter: ["text", "html"],
      },
    },
    resolve: {
      conditions: ["development", "browser"],
    },
  }),
);
