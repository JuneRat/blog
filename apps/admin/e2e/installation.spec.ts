import { test, expect } from "@playwright/test";

test("checks a supplied database, then installs and opens the login page", async ({ page }) => {
  const token = process.env.BLOG_BROWSER_INSTALL_TOKEN;
  test.skip(!token, "Runs against the disposable Docker installation deployment");
  await page.goto("/install");
  await page.getByLabel("安装码", { exact: true }).fill(token!);
  await page.getByRole("button", { name: "验证安装码", exact: true }).click();
  await expect(page.getByLabel("数据库地址", { exact: true })).toBeVisible();
  await expect(page.getByLabel("用户名", { exact: true })).toBeHidden();
  await page.getByLabel("数据库地址", { exact: true }).fill(process.env.BLOG_BROWSER_DATABASE_URL!);
  await page.getByRole("button", { name: "验证数据库连接", exact: true }).click();
  await expect(page.getByRole("status")).toContainText("验证通过");
  await page.getByLabel("用户名", { exact: true }).fill("acceptance-owner");
  await page.getByLabel("密码", { exact: true }).fill(process.env.BLOG_BROWSER_PASSWORD!);
  await page.getByLabel("确认密码", { exact: true }).fill(process.env.BLOG_BROWSER_PASSWORD!);
  await page.screenshot({ path: "/tmp/blog-install-verified.png", fullPage: true });
  await page.getByRole("button", { name: "安装博客", exact: true }).click();
  await expect(page).toHaveURL(/\/admin\/(?:login)?$/, { timeout: 30_000 });
});
