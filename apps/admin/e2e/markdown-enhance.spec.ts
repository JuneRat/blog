import { test, expect } from "@playwright/test";
import { createServer, type Server } from "node:http";
import { readFile } from "node:fs/promises";
import { resolve } from "node:path";
import { previewDocument } from "../src/components/previewDocument";

// Exercise the exact committed Rust-served bundles in a real browser, including
// CSP and fonts in the opaque-origin preview. No database or login is required.
const root = resolve(import.meta.dirname, "../../..");
const prefix = "/assets/plugins/markdown-enhance/test/";
const head = `<link rel="stylesheet" href="${prefix}display.css"><link rel="stylesheet" href="${prefix}katex.css"><script src="${prefix}math.js" defer></script><script src="${prefix}mermaid.js" defer></script>`;
const html = `<h1>公式与图表</h1><p>质量与能量：<span class="math math-inline">E=mc^2</span>。普通正文保持可读。</p>
<span class="math math-display">\\int_0^1 x^2 \\, dx = \\frac{1}{3}</span>
<pre><code class="language-mermaid">graph LR
A[开始] --&gt; B{检查配置}
B --&gt; C[完成]</code></pre>
<pre><code class="language-mermaid">this is invalid</code></pre>
<pre><code class="language-mermaid">sequenceDiagram
Alice-&gt;&gt;Bob: Hello</code></pre>
<p>错误公式：<span class="math math-inline">\\notARealCommand{bad}</span></p>
<p>之后仍可显示 <span class="math math-inline">a+b</span>。</p>
<pre><code>$literal$</code></pre>`;
let server: Server;
let origin: string;

test.beforeAll(async () => {
  server = createServer(async (req, res) => {
    const url = new URL(req.url!, "http://fixture");
    if (url.pathname.startsWith(prefix)) {
      const file = url.pathname.slice(prefix.length);
      if (!/^[\w./-]+$/.test(file) || file.includes("..")) { res.writeHead(404).end(); return; }
      try {
        const bytes = await readFile(resolve(root, "crates/infrastructure/assets/markdown-enhance", file));
        const type = file.endsWith(".js") ? "text/javascript" : file.endsWith(".css") ? "text/css" : file.endsWith(".woff2") ? "font/woff2" : "application/octet-stream";
        res.writeHead(200, { "Content-Type": type, "Access-Control-Allow-Origin": "*", "X-Content-Type-Options": "nosniff" }).end(bytes);
      } catch { res.writeHead(404).end(); }
      return;
    }
    const theme = url.searchParams.get("theme") === "paper" ? "paper" : "default";
    if (url.pathname === "/theme.css") {
      res.writeHead(200, { "Content-Type": "text/css" }).end(await readFile(resolve(root, theme === "paper" ? "theme-packages/paper/assets/paper.css" : "themes/default/assets/style.css")));
      return;
    }
    if (url.pathname === "/preview") {
      res.writeHead(200, { "Content-Type": "text/html; charset=utf-8", "Content-Security-Policy": "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data: blob: https: http:; font-src 'self' data:; connect-src 'self'; object-src 'none'; base-uri 'none'; form-action 'self'; frame-ancestors 'none'" }).end("<!doctype html><html><body><h1>后台预览</h1></body></html>");
      return;
    }
    res.writeHead(200, { "Content-Type": "text/html; charset=utf-8" }).end(`<!doctype html><html lang="zh-CN"><head><meta name="viewport" content="width=device-width, initial-scale=1"><link rel="stylesheet" href="/theme.css?theme=${theme}">${head}</head><body><main><article class="${theme === "paper" ? "prose" : "content"}" data-content-root>${html}</article></main><aside><span class="math math-inline">$outside$</span></aside></body></html>`);
  });
  await new Promise<void>(done => server.listen(0, "127.0.0.1", done));
  const address = server.address();
  if (!address || typeof address === "string") throw new Error("Missing fixture address");
  origin = `http://127.0.0.1:${address.port}`;
});
test.afterAll(async () => { await new Promise<void>(done => server.close(() => done())); });

for (const theme of ["default", "paper"]) {
  test(`${theme}: renders locally, isolates errors, and preserves ordinary code`, async ({ page }, testInfo) => {
    const failures: string[] = [];
    page.on("pageerror", error => failures.push(error.message));
    page.on("requestfailed", request => failures.push(request.url()));
    await page.route("**/*", route => new URL(route.request().url()).origin === origin ? route.continue() : route.abort());
    await page.goto(`${origin}/?theme=${theme}`);
    await expect(page.getByRole("heading", { name: "公式与图表" })).toBeVisible();
    await expect(page.locator("[data-content-root] .katex")).toHaveCount(3);
    await expect(page.locator(".md-enhance-diagram svg")).toHaveCount(2);
    await expect(page.locator(".md-enhance-error")).toHaveCount(2);
    await expect(page.locator("code").filter({ hasText: "$literal$" })).toHaveText("$literal$");
    await expect(page.locator("aside .katex")).toHaveCount(0);
    await expect(page.locator("code").filter({ hasText: "this is invalid" })).toBeVisible();
    await page.evaluate(() => document.fonts.ready);
    expect(failures).toEqual([]);
    await page.setViewportSize({ width: 390, height: 844 });
    await expect(page.locator(".md-enhance-diagram").first()).toBeVisible();
    await page.screenshot({ path: testInfo.outputPath(`${theme}-mobile.png`), fullPage: true });
  });
}

test("admin preview renders the same assets with fonts and without access to admin", async ({ page }, testInfo) => {
  const failures: string[] = [];
  page.on("pageerror", error => failures.push(error.message));
  page.on("requestfailed", request => failures.push(request.url()));
  await page.goto(`${origin}/preview`);
  const srcdoc = previewDocument({ content_html: html, head_html: head }, origin);
  await page.evaluate(srcdoc => {
    const iframe = document.createElement("iframe");
    iframe.setAttribute("sandbox", "allow-scripts");
    iframe.title = "正文预览";
    iframe.style.cssText = "width:95%;height:680px";
    iframe.srcdoc = srcdoc;
    document.body.append(iframe);
  }, srcdoc);
  const frame = page.frameLocator("iframe");
  await expect(frame.locator(".katex")).toHaveCount(3);
  await expect(frame.locator(".md-enhance-diagram svg")).toHaveCount(2);
  await expect(frame.locator(".md-enhance-error")).toHaveCount(2);
  const inner = page.frames().find(candidate => candidate.parentFrame());
  expect(await inner!.evaluate(async () => {
    await document.fonts.ready;
    return [...document.fonts].some(font => font.family.startsWith("KaTeX") && font.status === "loaded");
  })).toBe(true);
  expect(await inner!.evaluate(() => {
    try { void parent.document.body; return false; } catch { return true; }
  })).toBe(true);
  expect(failures).toEqual([]);
  await page.screenshot({ path: testInfo.outputPath("preview.png"), fullPage: true });
});
