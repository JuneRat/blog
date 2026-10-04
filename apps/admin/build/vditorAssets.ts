import { createHash } from "node:crypto";
import { createRequire } from "node:module";
import { readFile, readdir } from "node:fs/promises";
import { dirname, resolve } from "node:path";
import { build } from "esbuild";
import type { Plugin } from "vite";

/** Package Vditor's runtime scripts on the same origin in dev and production. */
export function vditorAssets(): Plugin {
  const require = createRequire(import.meta.url);
  const assets = new Map<string, Uint8Array>();
  let prefix: string;
  let base: string;
  async function copy(directory: string, destination: string) {
    for (const entry of (await readdir(directory, { withFileTypes: true })).sort((a, b) => a.name.localeCompare(b.name))) {
      const path = resolve(directory, entry.name);
      const name = `${destination}/${entry.name}`;
      if (entry.isDirectory()) await copy(path, name);
      else assets.set(name, await readFile(path));
    }
  }
  return {
    name: "local-vditor-assets",
    async config(config) {
      base = config.base ?? "/";
      const vditor = dirname(require.resolve("vditor/package.json"));
      for (const path of ["dist/js/lute", "dist/js/i18n", "dist/js/icons", "dist/js/highlight.js", "dist/css", "dist/images"]) {
        await copy(resolve(vditor, path), path);
      }
      // Share pinned, maintained renderers with the public Markdown plugin.
      const katex = dirname(require.resolve("katex/package.json"));
      await copy(resolve(katex, "dist/fonts"), "dist/js/katex/fonts");
      for (const file of ["katex.min.js", "katex.min.css"]) {
        assets.set(`dist/js/katex/${file}`, await readFile(resolve(katex, "dist", file)));
      }
      assets.set("dist/js/katex/mhchem.min.js", await readFile(resolve(katex, "dist/contrib/mhchem.min.js")));
      const mermaid = await build({
        stdin: { contents: `import mermaid from ${JSON.stringify(require.resolve("mermaid"))}; window.mermaid = mermaid;`, resolveDir: dirname(require.resolve("mermaid/package.json")) },
        bundle: true, write: false, format: "iife", platform: "browser", target: "es2024", minify: true,
        legalComments: "inline",
      });
      assets.set("dist/js/mermaid/mermaid.min.js", mermaid.outputFiles[0].contents);
      for (const name of ["vditor", "katex", "mermaid", "diff-match-patch"]) {
        const directory = dirname(require.resolve(`${name}/package.json`));
        const licenses = (await readdir(directory)).filter(file => /^(license|notice)(\.|$)/i.test(file));
        for (const file of licenses) assets.set(`licenses/${name}-${file}`, await readFile(resolve(directory, file)));
      }
      const hash = createHash("sha256");
      for (const [name, bytes] of assets) { hash.update(name); hash.update(bytes); }
      prefix = `assets/vditor-${hash.digest("hex").slice(0, 12)}`;
      return { define: { __VDITOR_CDN__: JSON.stringify(`${base}${prefix}`) } };
    },
    configureServer(server) {
      server.middlewares.use((req, res, next) => {
        const path = new URL(req.url ?? "/", "http://localhost").pathname;
        const bytes = path.startsWith(`${base}${prefix}/`) ? assets.get(path.slice(`${base}${prefix}/`.length)) : undefined;
        if (!bytes) return next();
        const extension = path.split(".").pop();
        res.setHeader("Content-Type", ({ js: "text/javascript", css: "text/css", svg: "image/svg+xml", png: "image/png", woff2: "font/woff2", woff: "font/woff", ttf: "font/ttf" } as Record<string, string>)[extension ?? ""] ?? "text/plain");
        res.end(bytes);
      });
    },
    generateBundle() {
      for (const [name, source] of assets) this.emitFile({ type: "asset", fileName: `${prefix}/${name}`, source });
    },
  };
}
