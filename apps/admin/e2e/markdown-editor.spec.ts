import { test, expect } from "@playwright/test";
import { createServer, type Server } from "node:http";
import { readFile } from "node:fs/promises";
import { resolve } from "node:path";
import { meResponse, postResponse } from "../tests/httpFixtures";

// Real production UI and Vditor bundles, backed by an isolated API fixture.
const root = resolve(import.meta.dirname, "../../..");
const source = "## 双栏写作\n\n左侧编辑 **Markdown**，右侧查看正文。\n\n$E=mc^2$\n\n```mermaid\ngraph LR\nA[写作] --> B[预览] --> C[保存]\n```";
const post = postResponse({ content: source, title: "Markdown 双栏编辑器", slug: "markdown-editor" });
let server: Server;
let origin: string;

test.beforeAll(async () => {
  server = createServer(async (req, res) => {
    const path = new URL(req.url!, "http://fixture").pathname;
    let file = "apps/admin/dist/index.html";
    if (path.startsWith("/admin/assets/")) file = `apps/admin/dist/assets/${path.slice("/admin/assets/".length)}`;
    if (!/^[\w./-]+$/.test(file) || file.includes("..")) { res.writeHead(404).end(); return; }
    try {
      const bytes = await readFile(resolve(root, file));
      const type = file.endsWith(".html") ? "text/html; charset=utf-8" : file.endsWith(".js") ? "text/javascript"
        : file.endsWith(".css") ? "text/css" : file.endsWith(".woff2") ? "font/woff2" : "application/octet-stream";
      res.writeHead(200, {
        "Content-Type": type, "Access-Control-Allow-Origin": "*", "X-Content-Type-Options": "nosniff",
        ...(file.endsWith(".html") ? { "Content-Security-Policy": "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data: blob: https: http:; font-src 'self' data:; connect-src 'self'; object-src 'none'; base-uri 'none'; form-action 'self'; frame-ancestors 'none'" } : {}),
      }).end(bytes);
    } catch { res.writeHead(404).end(); }
  });
  await new Promise<void>(done => server.listen(0, "127.0.0.1", done));
  const address = server.address();
  if (!address || typeof address === "string") throw new Error("Missing fixture address");
  origin = `http://127.0.0.1:${address.port}`;
});
test.afterAll(async () => { await new Promise<void>(done => server.close(() => done())); });

for (const kind of ["posts", "pages"]) {
  test(`${kind}: split view, local rendering, mobile switching and source retention`, async ({ page }, testInfo) => {
    const failures: string[] = [];
    const writes: string[] = [];
    const previews: string[] = [];
    const requests: string[] = [];
    const saves: { content: string }[] = [];
    const uploads: { filename: string | null; contentType: string; bytes: Buffer | null }[] = [];
    const png = Buffer.from("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVQIHWP4z8DwHwAFgAI/ScLbtAAAAABJRU5ErkJggg==", "base64");
    let finishSave!: () => void;
    const saveGate = new Promise<void>(resolve => { finishSave = resolve; });
    const asset = {
      id: "0195c98a-6430-7000-8000-000000000002", original_name: "editor.png", mime: "image/png",
      byte_size: 68, width: 1, height: 1, deleted_at: null, version: 1,
      created_at: post.updated_at, owner_id: post.author_id, owner_display: "测试作者",
      url: "/media/editor.png", reference_count: 0,
    };
    page.on("request", request => requests.push(request.url()));
    page.on("pageerror", error => failures.push(error.message));
    page.on("console", message => {
      if (message.type() === "error" && message.text().includes("Content Security Policy")) failures.push(message.text());
    });
    await page.route("**/media/editor.png", route => route.fulfill({ contentType: "image/png", body: png }));
    await page.route("**/auth/providers", route => route.fulfill({ json: [] }));
    await page.route("**/api/admin/v1/**", async route => {
      const path = new URL(route.request().url()).pathname;
      if (path.endsWith("/me")) return route.fulfill({ json: meResponse({ display_name: "测试作者", permissions: ["post.create", "post.update", "page.create", "page.update", "media.read", "media.upload"] }) });
      if (path.endsWith("/content-preview")) {
        const input = route.request().postDataJSON() as { content: string };
        previews.push(input.content);
        return route.fulfill({ json: { content_html: '<ul><li><input type="checkbox" disabled checked>任务</li></ul>', head_html: "" } });
      }
      if (path.endsWith("/media")) {
        if (route.request().method() === "POST") {
          const filename = new URL(route.request().url()).searchParams.get("filename");
          uploads.push({ filename, contentType: route.request().headers()["content-type"], bytes: route.request().postDataBuffer() });
          return route.fulfill({ json: { ...asset, original_name: filename } });
        }
        return route.fulfill({ json: { items: [asset], total: 1, page: 1, per_page: 24 } });
      }
      if (path.endsWith(`/${post.id}`) && route.request().method() === "PATCH") {
        const input = route.request().postDataJSON() as { content: string };
        saves.push(input); await saveGate;
        return route.fulfill({ json: { ...post, content: input.content, version: 2 } });
      }
      if (route.request().method() !== "GET") writes.push(path);
      if (path.endsWith(`/${post.id}`)) return route.fulfill({ json: post });
      if (path.endsWith("/comment-settings")) return route.fulfill({ json: { enabled: true, version: 1 } });
      return route.fulfill({ json: [] });
    });
    await page.setViewportSize({ width: 1440, height: 1000 });
    await page.goto(`${origin}/admin/${kind}/${post.id}/edit`);
    const body = page.getByLabel("正文（Markdown）");
    await expect(page.locator(".markdown-editor-vditor")).toBeVisible();
    await expect(page.getByRole("button", { name: "发布效果预览", exact: true })).toHaveCount(1);
    await expect(page.locator('iframe[title="正文预览"]')).toHaveCount(0);
    await expect(body).toHaveValue(source);
    const toolbar = page.locator(".vditor-toolbar");
    const imageButton = toolbar.getByRole("button", { name: "插入图片", exact: true });
    const imageDialog = page.getByRole("dialog", { name: "插入图片", exact: true });
    await expect(imageButton).toHaveCount(1);
    await expect(toolbar.locator('[data-type="upload"], [data-type="media-library"]')).toHaveCount(0);
    // Opening from the initial viewport must show the picker without scrolling past the editor.
    await imageButton.click();
    await expect(imageDialog).toBeInViewport({ ratio: 0.95 });
    await expect(imageDialog).toHaveCSS("opacity", "1");
    await expect(imageDialog.getByRole("button", { name: "上传图片", exact: true })).toBeVisible();
    await expect(imageDialog.getByLabel("搜索图片")).toBeVisible();
    await page.screenshot({ path: testInfo.outputPath(`${kind}-image-dialog.png`) });
    await imageDialog.getByRole("button", { name: "关闭插图窗口" }).click();
    const local = page.locator(".vditor-preview");
    await expect(local.locator(".katex")).toHaveCount(1);
    await expect(local.locator(".language-mermaid svg")).toHaveCount(1);
    expect(previews).toEqual([]);
    await page.screenshot({ path: testInfo.outputPath(`${kind}-desktop.png`), fullPage: true });
    // Switching through IR must not silently normalize the Form's canonical Markdown.
    await page.getByLabel("编辑器视图").getByText("即时渲染", { exact: true }).click();
    await expect(body).toHaveAttribute("contenteditable", "true");
    await expect(body.locator(".katex")).toHaveCount(1);
    await page.getByLabel("编辑器视图").getByText("双栏", { exact: true }).click();
    await expect(body).toHaveValue(source);

    const long = Array.from({ length: 70 }, (_, i) => `## 章节 ${i + 1}\n\n${"中英文 Markdown content，段落换行测试。".repeat(i % 5 + 1)}\n\n- 第一项\n- 第二项`).join("\n\n");
    await body.fill(long);
    await expect(local.locator("h2")).toHaveCount(70);
    const ratio = (element: Element) => element.scrollTop / (element.scrollHeight - element.clientHeight);
    for (const progress of [0.35, 1, 0]) {
      await body.evaluate((element, position) => { element.scrollTop = (element.scrollHeight - element.clientHeight) * position; }, progress);
      await expect.poll(() => local.evaluate(ratio)).toBeCloseTo(progress, 2);
    }
    for (const progress of [0.65, 1, 0]) {
      await local.evaluate((element, position) => { element.scrollTop = (element.scrollHeight - element.clientHeight) * position; }, progress);
      await expect.poll(() => body.evaluate(ratio)).toBeCloseTo(progress, 2);
    }
    expect(previews).toEqual([]);

    // Both upload and library selection use the same dialog and preserve the editor's caret.
    for (const mode of ["双栏", "即时渲染"]) {
      await body.fill("左段\n\n右段");
      await page.getByLabel("编辑器视图").getByText(mode, { exact: true }).click();
      if (mode === "双栏") {
        await body.evaluate(element => { (element as HTMLTextAreaElement).setSelectionRange(2, 2); });
      } else {
        await body.locator("p").first().click();
        await body.press("End");
      }
      await imageButton.click();
      await expect(imageDialog).toBeInViewport({ ratio: 0.95 });
      await expect(imageDialog).toHaveCSS("opacity", "1");
      const chooser = page.waitForEvent("filechooser");
      await imageDialog.getByRole("button", { name: "上传图片", exact: true }).click();
      await (await chooser).setFiles({ name: `${mode}.png`, mimeType: "image/png", buffer: png });
      await expect(imageDialog).toHaveCount(0);
      await expect(page.getByText("已插入 1 张图片（尚未保存，点保存后生效）。", { exact: true })).toBeVisible();
      await page.getByLabel("编辑器视图").getByText("源码", { exact: true }).click();
      await expect(body).toHaveValue(new RegExp(`左段\\s*!\\[${mode}\\]\\(/media/editor.png\\)\\s+右段`));
    }
    expect(uploads).toEqual(["双栏", "即时渲染"].map(mode => ({ filename: `${mode}.png`, contentType: "image/png", bytes: png })));
    expect(saves).toEqual([]);

    // IR caret must survive opening the media panel and focusing its alt-text field.
    await body.fill("左段\n\n右段");
    await page.getByLabel("编辑器视图").getByText("即时渲染", { exact: true }).click();
    await body.locator("p").first().click();
    await body.press("End");
    await imageButton.click();
    await expect(imageDialog).toBeInViewport({ ratio: 0.95 });
    await expect(imageDialog).toHaveCSS("opacity", "1");
    await imageDialog.getByLabel("替代文字").fill("插图说明");
    await imageDialog.getByRole("button", { name: "插入", exact: true }).click();
    await expect(imageDialog).toHaveCount(0);
    await page.getByLabel("编辑器视图").getByText("源码", { exact: true }).click();
    await expect(body).toHaveValue(/左段\s*!\[插图说明\]\(\/media\/editor.png\)\s+右段/);

    await page.getByLabel("编辑器视图").getByText("即时渲染", { exact: true }).click();
    await body.fill("即时渲染的新正文");
    await body.press("End");
    await body.press("Enter");
    await body.pressSequentially("**bold**");
    await expect(body.locator('strong')).toContainText("bold");
    await page.getByLabel("编辑器视图").getByText("源码", { exact: true }).click();
    await expect(body).toHaveValue(/即时渲染的新正文[\s\S]*\*\*bold\*\*/);
    await body.fill(source);

    await page.setViewportSize({ width: 390, height: 844 });
    await expect(page.getByRole("radio", { name: "双栏", exact: true })).toHaveCount(0);
    await page.getByLabel("编辑器视图").getByText("即时渲染", { exact: true }).click();
    await expect(body).toBeVisible();
    expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBeLessThanOrEqual(390);
    await page.screenshot({ path: testInfo.outputPath(`${kind}-mobile.png`), fullPage: true });
    await imageButton.click();
    await expect(imageDialog).toBeInViewport({ ratio: 0.95 });
    await expect(imageDialog).toHaveCSS("opacity", "1");
    expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBeLessThanOrEqual(390);
    await page.screenshot({ path: testInfo.outputPath(`${kind}-image-dialog-mobile.png`) });
    await imageDialog.getByRole("button", { name: "关闭插图窗口" }).click();
    await page.getByLabel("编辑器视图").getByText("源码", { exact: true }).click();
    await expect(body).toHaveValue(source);
    expect(requests.filter(url => !url.startsWith(origin))).toEqual([]);
    expect(writes).toEqual([]);
    expect(previews).toEqual([]);
    expect(failures).toEqual([]);

    // Save immediately after IR input, then keep typing while the response is pending.
    await page.getByLabel("编辑器视图").getByText("即时渲染", { exact: true }).click();
    await body.fill("本次即时渲染提交");
    await body.press("ControlOrMeta+A");
    await page.locator('.vditor-toolbar [data-type="bold"]').click();
    await page.getByRole("button", { name: "保存草稿", exact: true }).click();
    await expect.poll(() => saves.length).toBe(1);
    expect(saves[0].content.trim()).toBe("**本次即时渲染提交**");
    await body.fill("保存期间继续输入");
    finishSave();
    await expect(page.getByText("已保存；等待期间的新改动尚未保存。", { exact: true })).toBeVisible();
    await page.getByLabel("编辑器视图").getByText("源码", { exact: true }).click();
    await expect(body).toHaveValue(/保存期间继续输入/);
    await body.fill("- [x] 任务");
    await page.getByRole("button", { name: "发布效果预览", exact: true }).click();
    const publication = page.frameLocator('iframe[title="发布正文预览"]');
    await expect(publication.getByRole("checkbox")).toBeChecked();
    await expect(publication.getByRole("checkbox")).toBeDisabled();
    expect(previews).toEqual(["- [x] 任务"]);
    expect(saves).toHaveLength(1);
    await page.getByRole("dialog").getByRole("button", { name: "关闭", exact: true }).last().click();
    await expect(body).toHaveValue("- [x] 任务");
    expect(failures).toEqual([]);
  });
}

async function readOnlyFixture(page: import("@playwright/test").Page, content: string) {
  await page.route("**/auth/providers", route => route.fulfill({ json: [] }));
  await page.route("**/api/admin/v1/**", route => {
    const path = new URL(route.request().url()).pathname;
    if (path.endsWith("/me")) return route.fulfill({ json: meResponse({ permissions: ["post.update"] }) });
    if (path.endsWith(`/${post.id}`)) return route.fulfill({ json: { ...post, content } });
    if (path.endsWith("/comment-settings")) return route.fulfill({ json: { enabled: true, version: 1 } });
    return route.fulfill({ json: [] });
  });
  await page.setViewportSize({ width: 1440, height: 1000 });
  await page.goto(`${origin}/admin/posts/${post.id}/edit`);
}

test("local editor keeps strict diagrams, isolates malformed blocks and preserves unsupported fences", async ({ page }) => {
  const errors: string[] = [];
  const remote: string[] = [];
  page.on("pageerror", error => errors.push(error.message));
  page.on("request", request => { if (!request.url().startsWith(origin)) remote.push(request.url()); });
  const unsafe = [
    '```mermaid\n%%{init: {"securityLevel":"loose","flowchart":{"htmlLabels":true}}}%%\ngraph LR\nA[Safe] --> B[Node]\nclick A "javascript:alert(1)"\n```',
    '```mermaid\ngraph LR\nA["<img src=x onerror=alert(1)>"] --> B[\n```',
    '$\\unknown{<img src=x onerror=alert(1)>}$',
    '```plantuml\nAlice -> Bob: keep as code\n```',
  ].join("\n\n");
  await readOnlyFixture(page, unsafe);
  const local = page.locator(".vditor-preview");
  await expect(local.locator('.language-mermaid[data-processed="true"]')).toHaveCount(2);
  await expect(local.locator(".language-mermaid").first().locator("svg")).toHaveCount(1);
  await expect(local.locator(".language-mermaid").first().locator("foreignObject")).toHaveCount(0);
  expect(await local.locator("a").evaluateAll(links => links.map(link => link.getAttribute("href") ?? link.getAttribute("xlink:href")))).not.toContain("javascript:alert(1)");
  await expect(local.locator(".language-mermaid").nth(1)).toContainText(/Parse error/);
  await expect(local.locator(".language-plantuml")).toHaveText("Alice -> Bob: keep as code");
  await expect(local.locator(".markdown-editor-math-error")).toHaveCount(1);
  await expect(local.locator("img, script, [onerror]")).toHaveCount(0);
  await page.getByLabel("编辑器视图").getByText("即时渲染", { exact: true }).click();
  const body = page.getByLabel("正文（Markdown）");
  await expect(body.locator('.language-mermaid[data-processed="true"]')).toHaveCount(2);
  await expect(body.locator(".vditor-ir__preview foreignObject, img[onerror]")).toHaveCount(0);
  await page.getByLabel("编辑器视图").getByText("源码", { exact: true }).click();
  await expect(body).toHaveValue(unsafe);
  expect(remote).toEqual([]);
  expect(errors).toEqual([]);
});

test("runtime loading failure keeps source editable and retries without losing input", async ({ page }) => {
  await page.route("**/dist/js/lute/lute.min.js", route => route.abort());
  await readOnlyFixture(page, source);
  await expect(page.getByText("编辑器资源加载失败，可重试或继续编辑 Markdown 源码。", { exact: true })).toBeVisible();
  await page.getByLabel("正文（Markdown）").fill("加载失败期间继续写作");
  await page.unroute("**/dist/js/lute/lute.min.js");
  await page.getByRole("button", { name: "重试", exact: true }).click();
  await expect(page.locator(".markdown-editor-vditor")).toBeVisible();
  await expect(page.getByLabel("正文（Markdown）")).toHaveValue("加载失败期间继续写作");
});

test("invalid math keeps its source and a friendly hint in split and IR, and recovers after correction", async ({ page }, testInfo) => {
  const block = String.raw`\notARealCommand{bad}`;
  const inline = String.raw`\notARealCommand{inline}`;
  const markdown = `# 公式错误回退\n\n$$\n${block}\n$$\n\n行内 $${inline}$\n\n正常公式 $E=mc^2$`;
  await readOnlyFixture(page, markdown);
  const preview = page.locator(".vditor-preview");
  const body = page.getByLabel("正文（Markdown）");
  await expect(preview.locator(".markdown-editor-render-error")).toHaveText([
    "公式渲染失败，请检查语法。", "公式渲染失败，请检查语法。",
  ]);
  await expect(preview.locator(".markdown-editor-math-source")).toHaveText([block, inline]);
  await expect(preview.locator(".katex")).toHaveCount(1);
  await expect(preview).not.toContainText("KaTeX parse error");
  await expect(body).toHaveValue(markdown);
  const copied = await preview.locator(".markdown-editor-math-error").first().evaluate(element => {
    const clipboard = new DataTransfer();
    element.dispatchEvent(new ClipboardEvent("copy", { bubbles: true, cancelable: true, clipboardData: clipboard }));
    return { text: clipboard.getData("text/plain"), html: clipboard.getData("text/html") };
  });
  expect(copied.text.trim()).toBe(block);
  expect(copied.html).not.toContain("公式渲染失败");
  await page.locator(".markdown-editor").screenshot({ path: testInfo.outputPath("math-error-fallback.png") });

  await page.getByLabel("编辑器视图").getByText("即时渲染", { exact: true }).click();
  await expect(body.locator(".markdown-editor-render-error")).toHaveCount(2);
  await expect(body.locator(".markdown-editor-math-source")).toHaveText([block, inline]);
  // Actual IR input must not serialize visible error messages into the article.
  await body.locator("p").last().click();
  await body.press("End");
  await body.press("Enter");
  await body.pressSequentially("后续编辑");
  await page.getByLabel("编辑器视图").getByText("源码", { exact: true }).click();
  const edited = await body.inputValue();
  expect(edited).toContain(block);
  expect(edited).toContain(inline);
  expect(edited).toContain("后续编辑");
  expect(edited).not.toContain("公式渲染失败");
  expect(edited).not.toContain("KaTeX parse error");

  await body.fill(markdown.replaceAll(String.raw`\notARealCommand`, String.raw`\mathrm`));
  await page.getByLabel("编辑器视图").getByText("双栏", { exact: true }).click();
  await expect(preview.locator(".katex")).toHaveCount(3);
  await expect(preview.locator(".markdown-editor-render-error")).toHaveCount(0);
  await page.getByLabel("编辑器视图").getByText("即时渲染", { exact: true }).click();
  await expect(body.locator(".katex")).toHaveCount(3);
  await expect(body.locator(".markdown-editor-render-error")).toHaveCount(0);
});
