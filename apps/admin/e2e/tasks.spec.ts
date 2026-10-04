import { expect, test } from "@playwright/test";
import type { Page, Response } from "@playwright/test";
import type { TaskRun, TaskView } from "../src/api/generated";

const endpoint = "/api/admin/v1/tasks";

function mutation(page: Page, path: string) {
  return page.waitForResponse(response => new URL(response.url()).pathname === path && response.request().method() === "POST");
}

async function accepted(response: Response): Promise<TaskRun> {
  expect(response.status()).toBe(202);
  expect(response.headers()["cache-control"]).toBe("no-store");
  return response.json();
}

async function current(page: Page): Promise<TaskView> {
  const response = await page.request.get(endpoint);
  expect(response.status()).toBe(200);
  return response.json();
}

test("real future plan cancellation, failed rebuild, refresh and a new retry identity", async ({ page }) => {
  test.setTimeout(60_000);
  const fixtureId = process.env.BLOG_BROWSER_TASK_POST_ID;
  expect(fixtureId, "acceptance.py owns and prepares the invalid old HTML fixture").toBeTruthy();
  await page.goto("/admin/");
  await page.getByLabel("用户名或邮箱").fill("acceptance-owner");
  await page.getByLabel("密码", { exact: true }).fill(process.env.BLOG_BROWSER_PASSWORD!);
  await page.getByRole("button", { name: "登录", exact: true }).click();
  await page.getByRole("menuitem", { name: "任务管理", exact: true }).click();
  await expect(page).toHaveURL(/\/admin\/tasks$/);
  const html = page.getByRole("region", { name: "内容重建", exact: true });
  await html.getByRole("radio", { name: "一次性计划", exact: true }).check();
  const plannedResponse = mutation(page, endpoint);
  await html.getByRole("button", { name: "创建重建计划", exact: true }).click();
  const planned = await accepted(await plannedResponse);
  expect(planned.status).toBe("queued");
  expect(planned.trigger).toBe("once");
  expect(Date.parse(planned.run_at)).toBeGreaterThan(Date.now());
  await page.reload();
  expect((await current(page)).latest.find(run => run.kind === "html_rebuild")?.id).toBe(planned.id);
  const cancelledResponse = mutation(page, endpoint + "/" + planned.id + "/cancel");
  await html.getByRole("button", { name: "取消重建计划", exact: true }).click();
  const cancelled = await cancelledResponse;
  expect(cancelled.status()).toBe(200);
  expect((await cancelled.json()).status).toBe("cancelled");
  await html.getByRole("radio", { name: "立即执行", exact: true }).check();
  const startedResponse = mutation(page, endpoint);
  await html.getByRole("button", { name: "开始重建", exact: true }).click();
  const started = await accepted(await startedResponse);
  await page.reload();
  await expect(html.getByText("内容重建未全部完成", { exact: true })).toBeVisible({ timeout: 20_000 });
  expect((await current(page)).latest.find(run => run.kind === "html_rebuild")?.id).toBe(started.id);
  await expect(html.getByText(new RegExp(fixtureId!))).toBeVisible();

  // Repair source through the real authorized content API; no mocked task
  // response or browser-side state fabricates the failure/retry lifecycle.
  const me = await (await page.request.get("/api/admin/v1/me")).json();
  const post = await (await page.request.get("/api/admin/v1/posts/" + fixtureId)).json();
  const repaired = await page.request.patch("/api/admin/v1/posts/" + fixtureId, {
    data: { content: "**Browser repaired body**", expected_version: post.version },
    headers: { "X-CSRF-Token": me.csrf_token, Origin: new URL(page.url()).origin },
  });
  expect(repaired.status()).toBe(200);
  const retriedResponse = mutation(page, endpoint + "/" + started.id + "/retry");
  await html.getByRole("button", { name: "重新执行", exact: true }).click();
  const retried = await accepted(await retriedResponse);
  expect(retried.id).not.toBe(started.id);
  expect(retried.retry_of).toBe(started.id);
  await expect(html.getByText("内容重建完成。", { exact: true })).toBeVisible({ timeout: 20_000 });
  await page.reload();
  expect((await current(page)).latest.find(run => run.kind === "html_rebuild")?.id).toBe(retried.id);
  await page.getByRole("tab", { name: "任务记录", exact: true }).click();
  await page.getByRole("button", { name: "查看任务 " + retried.id, exact: true }).click();
  const detail = page.getByRole("dialog", { name: "任务详情", exact: true });
  await expect(detail.getByText(retried.id, { exact: true })).toBeVisible();
  await expect(detail.getByText(started.id, { exact: true })).toBeVisible();
});
