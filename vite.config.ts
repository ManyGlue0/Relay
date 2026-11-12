import { createReadStream, readFileSync } from "node:fs";
import { join, resolve } from "node:path";
import { defineConfig, type Plugin } from "vite";
import react from "@vitejs/plugin-react";

/** pdf.js decodes some scanned-image formats (JBIG2, JPEG 2000) and color
 *  profiles with WebAssembly it fetches at runtime: served at /pdfjs/wasm/ in
 *  dev, copied into the build. Its scripting engine (quickjs) isn't needed. */
function pdfjsWasm(): Plugin {
  const dir = resolve("node_modules/pdfjs-dist/wasm");
  const files = ["jbig2.wasm", "openjpeg.wasm", "qcms_bg.wasm", "LICENSE_JBIG2", "LICENSE_OPENJPEG", "LICENSE_QCMS", "LICENSE_PDFJS_JBIG2", "LICENSE_PDFJS_OPENJPEG", "LICENSE_PDFJS_QCMS"];
  return {
    name: "pdfjs-wasm",
    configureServer(server) {
      server.middlewares.use("/pdfjs/wasm/", (req, res, next) => {
        const name = (req.url ?? "").replace(/^\//, "").split("?")[0];
        if (!files.includes(name)) return next();
        res.setHeader("content-type", name.endsWith(".wasm") ? "application/wasm" : "text/plain");
        createReadStream(join(dir, name)).pipe(res);
      });
    },
    generateBundle() {
      for (const name of files) this.emitFile({ type: "asset", fileName: `pdfjs/wasm/${name}`, source: readFileSync(join(dir, name)) });
      // pdf.js itself is Apache-2.0: its license ships with it.
      this.emitFile({ type: "asset", fileName: "pdfjs/LICENSE", source: readFileSync(resolve("node_modules/pdfjs-dist/LICENSE")) });
    },
  };
}

export default defineConfig({
  plugins: [react(), pdfjsWasm()],
  clearScreen: false,
  server: { port: 1436, strictPort: true, watch: { ignored: ["**/src-tauri/**", "**/target/**"] } },
  build: { target: "chrome120", chunkSizeWarningLimit: 4000 },
});
