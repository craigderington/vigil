/// <reference types="vitest/config" />
import { defineConfig } from "vite";
import solid from "vite-plugin-solid";

export default defineConfig({
  plugins: [solid()],
  server: {
    proxy: {
      "/api": { target: "http://localhost:8080", ws: false },
      "/events": { target: "http://localhost:8080", ws: false },
      "/ping": { target: "http://localhost:8080", ws: false },
    },
  },
  build: {
    outDir: "dist",
  },
  test: {
    environment: "jsdom",
    globals: true,
  },
});
