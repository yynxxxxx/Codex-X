import { readFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import react from "@vitejs/plugin-react";
import { createServer } from "vite";

// This renderer-only server never starts Tauri or connects to the real Codex store.
// Run: node apps/desktop/tests/serve-ui-fixture.mjs [port]
const root = fileURLToPath(new URL("../", import.meta.url));
const port = Number(process.argv[2] || 1421);
if (!Number.isInteger(port) || port < 1024 || port > 65535) {
  throw new Error("Fixture port must be an integer between 1024 and 65535");
}
const fixture = await readFile(new URL("./fixtures/ui-runtime.js", import.meta.url), "utf8");
const server = await createServer({
  root,
  configFile: false,
  clearScreen: false,
  plugins: [
    {
      name: "codex-x-ui-fixture",
      transformIndexHtml: {
        order: "pre",
        // Keep the original charset declaration before the large inline fixture.
        handler: () => [{ tag: "script", children: fixture, injectTo: "head" }],
      },
    },
    react(),
  ],
  server: {
    host: "127.0.0.1",
    port,
    strictPort: true,
    watch: { ignored: ["**/src-tauri/**"] },
    headers: {
      "Content-Security-Policy": "default-src 'self'; script-src 'self' 'unsafe-inline'; style-src 'self' 'unsafe-inline'; connect-src 'self' ws://127.0.0.1:*; img-src 'self' data:; font-src 'self' data:; object-src 'none'; frame-src 'none'; base-uri 'none'",
    },
  },
});
await server.listen();
console.log(`Fixture ready: http://127.0.0.1:${port}/`);
console.log("All IPC is mocked. Use the Fixture controls to resolve pending template requests.");
for (const signal of ["SIGINT", "SIGTERM"]) {
  process.once(signal, async () => {
    await server.close();
    process.exit(0);
  });
}
