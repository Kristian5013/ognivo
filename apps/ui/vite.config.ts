import tailwindcss from "@tailwindcss/vite";
import react from "@vitejs/plugin-react";
import { defineConfig } from "vitest/config";

export default defineConfig({
  plugins: [react(), tailwindcss()],
  // the Electron GUI and the VS Code webview load the build from files
  base: "./",
  // 8000, 8080 and 8090 belong to vLLM, Open WebUI and the night's llama-server
  server: { host: "127.0.0.1", port: 5180, strictPort: true },
  test: { include: ["src/**/*.test.ts"] },
});
