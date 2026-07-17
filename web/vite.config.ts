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
  // Without this, solid-js resolves to its server (SSR) build under Vitest's
  // Node environment, so client-only reactivity (onMount, createResource,
  // etc.) throws "Client-only API called on the server side" in tests.
  // Only "browser" is needed for that — "development" was also present but
  // it makes `vite build` resolve solid-js to its DEV build in production
  // too (bigger bundle, runtime assertions, console warnings), so it's
  // dropped here.
  resolve: {
    conditions: ["browser"],
  },
  test: {
    environment: "jsdom",
    globals: true,
  },
});
