import script from "../../../crates/interfaces/src/install/app.js?raw";
import html from "../../../crates/interfaces/src/install/index.html?raw";
import { fireEvent, waitFor } from "@testing-library/dom";
import { afterEach, beforeEach, expect, it, vi } from "vitest";

const response = (data: unknown, status = 200) => ({ ok: status < 400, status, json: async () => data });
const input = (id: string) => document.querySelector<HTMLInputElement>(`#${id}`)!;
const fields = () => document.querySelector<HTMLFieldSetElement>("#fields")!;
const submit = () => fireEvent.submit(document.querySelector("form")!);
let fetcher: ReturnType<typeof vi.fn>;
beforeEach(() => {
  document.body.innerHTML = new DOMParser().parseFromString(html, "text/html").body.innerHTML;
  fetcher = vi.fn();
  vi.stubGlobal("fetch", fetcher);
  window.eval(script);
});
afterEach(() => { vi.unstubAllGlobals(); document.body.replaceChildren(); });

it("先验证安装码，再显示状态；错误安装码不能展开表单", async () => {
  expect(fetcher).not.toHaveBeenCalled();
  expect(fields().disabled).toBe(true);
  expect(fields().hidden).toBe(true);
  expect(input("token").disabled).toBe(false);
  input("token").value = "bad";
  fetcher.mockResolvedValueOnce(response({}, 403));
  submit();
  await waitFor(() => expect(document.querySelector("#message")?.textContent).toContain("安装码无效"));
  expect(fields().hidden).toBe(true);
  expect(fields().disabled).toBe(true);
  expect(fetcher).toHaveBeenCalledWith("/api/install", { cache: "no-store", headers: { "X-Install-Token": "bad" } });
});

it("验证后填入配置地址；改动安装码必须重新验证，不能直接安装", async () => {
  input("token").value = "valid";
  fetcher.mockResolvedValueOnce(response({ database_configured: true, public_base_url: "https://blog.example.test" }));
  submit();
  await waitFor(() => expect(fields().disabled).toBe(false));
  expect(fields().hidden).toBe(false);
  expect(input("public-url").value).toBe("https://blog.example.test");
  expect(input("public-url").readOnly).toBe(true);
  expect(input("database").required).toBe(false);
  expect(document.querySelector<HTMLElement>("#database-fields")!.hidden).toBe(true);
  expect(fetcher).toHaveBeenCalledTimes(1);
  fireEvent.input(input("token"), { target: { value: "changed" } });
  expect(fields().disabled).toBe(true);
  expect(fields().hidden).toBe(true);
  fetcher.mockResolvedValueOnce(response({ database_configured: false, public_base_url: null }));
  submit();
  await waitFor(() => expect(fields().disabled).toBe(false));
  expect(fetcher).toHaveBeenLastCalledWith("/api/install", { cache: "no-store", headers: { "X-Install-Token": "changed" } });
  expect(input("database").required).toBe(true);
});

it("安装失败后带安装码重读续装状态，保留填写内容", async () => {
  input("token").value = "valid";
  fetcher.mockResolvedValueOnce(response({ database_configured: false, public_base_url: null }));
  submit();
  await waitFor(() => expect(fields().disabled).toBe(false));
  input("database").value = "postgres://user:password@db/blog";
  fetcher.mockResolvedValueOnce(response({ ready: true }));
  submit();
  await waitFor(() => expect(document.querySelector<HTMLFieldSetElement>("#admin-fields")!.disabled).toBe(false));
  input("username").value = "owner";
  input("password").value = input("confirm").value = "long test password";
  fetcher.mockResolvedValueOnce(response({ error: "主题尚未就绪" }, 400));
  fetcher.mockResolvedValueOnce(response({ database_configured: true, public_base_url: "https://blog.example.test" }));
  submit();
  await waitFor(() => expect(fields().disabled).toBe(false));
  expect(fetcher.mock.calls[2][1]).toMatchObject({ method: "POST", headers: { "X-Install-Token": "valid" } });
  expect(fetcher.mock.calls[3][1]).toEqual({ cache: "no-store", headers: { "X-Install-Token": "valid" } });
  expect(input("username").value).toBe("owner");
  expect(input("password").value).toBe("long test password");
  expect(input("database").value).toBe("");
  expect(document.querySelector("#message")?.textContent).toBe("主题尚未就绪");
});

it("连接通过才显示管理员表单；更换连接必须重新检查且不提交安装", async () => {
  input("token").value = "valid";
  fetcher.mockResolvedValueOnce(response({ database_configured: false, public_base_url: null }));
  submit();
  await waitFor(() => expect(fields().disabled).toBe(false));
  const admin = document.querySelector<HTMLFieldSetElement>("#admin-fields")!;
  expect(admin.hidden).toBe(true);
  expect(admin.disabled).toBe(true);
  input("database").value = "postgres://user:password@db/blog";
  fetcher.mockResolvedValueOnce(response({ ready: true }));
  submit();
  await waitFor(() => expect(admin.hidden).toBe(false));
  expect(fetcher).toHaveBeenLastCalledWith("/api/install/check", expect.objectContaining({
    method: "POST", body: JSON.stringify({ database_url: "postgres://user:password@db/blog" }),
  }));
  expect(document.querySelector("#submit")?.textContent).toBe("安装博客");
  fireEvent.input(input("database"), { target: { value: "postgres://user:password@db/other" } });
  expect(admin.hidden).toBe(true);
  expect(admin.disabled).toBe(true);
  expect(document.querySelector("#submit")?.textContent).toBe("验证数据库连接");
  fetcher.mockResolvedValueOnce(response({ error: "安装仅支持空数据库" }, 400));
  submit();
  await waitFor(() => expect(document.querySelector("#message")?.textContent).toContain("空数据库"));
  expect(fields().hidden).toBe(false);
  expect(admin.hidden).toBe(true);
  expect(fetcher).toHaveBeenCalledTimes(3);
});
