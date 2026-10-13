import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// Для разработки запустите dock с LOGNARA_DOCK_PUBLIC_URL=http://localhost:5173:
// Origin и passkey проверяются по адресу, который открыт в браузере.
export default defineConfig({
  plugins: [react()],
  server: {
    proxy: {
      "/api": "http://127.0.0.1:7403",
      "/health": "http://127.0.0.1:7403",
    },
  },
});
