import { defineConfig, loadEnv } from "vite";
import react from "@vitejs/plugin-react";

/**
 * The development server proxies the same API namespace that `lightagent
 * serve --web-root frontend/dist` exposes in production.
 *
 * Set LIGHTAGENT_DEV_ORIGIN to point Vite at a non-default server.
 */
export default defineConfig(({ mode }) => {
  const env = loadEnv(mode, process.cwd(), "LIGHTAGENT_");
  const agent = env.LIGHTAGENT_DEV_ORIGIN ?? "http://127.0.0.1:8735";

  return {
    plugins: [react()],
    server: {
      port: 5173,
      proxy: {
        "/api/lightagent": { target: agent, changeOrigin: true },
      },
    },
    build: {
      outDir: "dist",
      assetsDir: "assets",
      sourcemap: true,
    },
  };
});
