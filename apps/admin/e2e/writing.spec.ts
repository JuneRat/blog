import { expect, test } from "@playwright/test";
import type { Page } from "@playwright/test";

// The acceptance runner owns this disposable site and its credentials.
test.beforeEach(async ({ page }) => {
  await page.goto("/admin/");
  await page.getByLabel("用户名或邮箱").fill("acceptance-owner");
  await page.getByLabel("密码", { exact: true }).fill(process.env.BLOG_BROWSER_PASSWORD!);
  await page.getByRole("button", { name: "登录", exact: true }).click();
  await expect(page.getByRole("button", { name: "新建草稿" })).toBeVisible();
});

async function createDraft(page: Page, title: string) {
  await page.getByRole("button", { name: "新建草稿" }).click();
  await page.getByLabel("标题", { exact: true }).fill(title);
  await page.getByLabel("正文（Markdown）").fill("**浏览器正文**");
  await page.getByRole("button", { name: "保存草稿", exact: true }).click();
  await expect(page).toHaveURL(/\/admin\/posts\/[^/]+\/edit$/);
  await expect(page.getByRole("button", { name: "保存草稿", exact: true })).toBeEnabled();
}

test("production login, preview, publication and editor deep link", async ({ page, context }) => {
  await createDraft(page, "浏览器发布验收");
  await expect(page.locator(".vditor-preview strong")).toHaveText("浏览器正文");
  await page.getByRole("button", { name: "发布", exact: true }).click();
  const publicLink = page.getByRole("link", { name: "查看公开页面" });
  await expect(publicLink).toBeVisible();
  const publicPage = await context.newPage();
  await publicPage.goto((await publicLink.getAttribute("href"))!);
  await expect(publicPage.getByRole("heading", { name: "浏览器发布验收" })).toBeVisible();
  await expect(publicPage.locator("strong").filter({ hasText: "浏览器正文" })).toBeVisible();
  await publicPage.close();
  await page.reload();
  await expect(page.getByLabel("标题", { exact: true })).toHaveValue("浏览器发布验收");
});

test("navigation cancellation and local draft survive a real reload", async ({ page }) => {
  await createDraft(page, "浏览器恢复验收");
  const editorUrl = page.url();
  await page.getByLabel("正文（Markdown）").fill("本机尚未保存的正文");
  await page.getByRole("menuitem", { name: "标签", exact: true }).click();
  const confirmation = page.getByRole("dialog", { name: "有未保存的修改" });
  await confirmation.getByRole("button", { name: "留在此页" }).click();
  await expect(page).toHaveURL(editorUrl);
  page.once("dialog", dialog => dialog.accept());
  await page.reload();
  await page.getByRole("button", { name: "恢复本机编辑" }).click();
  await expect(page.getByLabel("正文（Markdown）")).toHaveValue("本机尚未保存的正文");
  await page.getByRole("button", { name: "保存草稿", exact: true }).click();
  await expect(page.getByText("已保存。", { exact: true })).toBeVisible();
});

test("typing while a real save response is pending preserves the newer input", async ({ page }) => {
  await createDraft(page, "浏览器并发输入验收");
  let release!: () => void;
  const gate = new Promise<void>(resolve => { release = resolve; });
  let saved!: () => void;
  const committed = new Promise<void>(resolve => { saved = resolve; });
  await page.route("**/api/admin/v1/posts/*", async route => {
    if (route.request().method() !== "PATCH") return route.continue();
    const response = await route.fetch();
    saved();
    await gate;
    await route.fulfill({ response });
  });
  const body = page.getByLabel("正文（Markdown）");
  await body.fill("本次提交的正文");
  await page.getByRole("button", { name: "保存草稿", exact: true }).click();
  await committed;
  await body.fill("等待响应期间继续输入");
  release();
  await expect(page.getByText("已保存；等待期间的新改动尚未保存。", { exact: true })).toBeVisible();
  await expect(body).toHaveValue("等待响应期间继续输入");
});
