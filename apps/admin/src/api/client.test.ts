import { afterEach, describe, expect, it, vi } from "vitest";
import {
  ApiError,
  ApiProtocolError,
  api,
  commentsApi,
  mediaApi,
  setCsrfToken,
  setUnauthorizedHandler,
} from "./index";
import { jsonResponse, postResponse } from "../../tests/httpFixtures";

function respond(response: Response) {
  const fetcher = vi.fn().mockResolvedValue(response);
  vi.stubGlobal("fetch", fetcher);
  return fetcher;
}
afterEach(() => {
  vi.unstubAllGlobals();
  setCsrfToken(null);
  setUnauthorizedHandler(null);
});

describe("HTTP response boundary", () => {
  it.each([
    [
      "missing required nullable field",
      () => {
        const value: Record<string, unknown> = postResponse();
        delete value.cover_media_id;
        return jsonResponse(value);
      },
    ],
    [
      "wrong field type",
      () => jsonResponse({ ...postResponse(), version: "2" }),
    ],
    [
      "unknown visibility",
      () => jsonResponse({ ...postResponse(), visibility: "hidden" }),
    ],
    [
      "unsafe integer",
      () =>
        jsonResponse({
          ...postResponse(),
          version: Number.MAX_SAFE_INTEGER + 1,
        }),
    ],
    [
      "malformed JSON",
      () =>
        new Response("{", {
          headers: {
            "Content-Type": "application/json",
            "x-request-id": "request-1",
          },
        }),
    ],
    [
      "HTML success",
      () =>
        new Response("<html>proxy error</html>", {
          headers: { "Content-Type": "text/html", "x-request-id": "request-1" },
        }),
    ],
    [
      "unexpected empty body",
      () =>
        new Response(null, {
          status: 204,
          headers: { "x-request-id": "request-1" },
        }),
    ],
  ] as const)("rejects %s and retains request ID", async (_name, response) => {
    respond(response());
    await expect(api.getPost("id")).rejects.toMatchObject({
      name: "ApiProtocolError",
      code: "invalid_response",
      requestId: "request-1",
    });
  });

  it("accepts explicit null and additive response fields", async () => {
    respond(jsonResponse({ ...postResponse(), future_field: "compatible" }));
    await expect(api.getPost("id")).resolves.toEqual(postResponse());
  });

  it("requires 204 for commands with no body", async () => {
    respond(new Response(null, { status: 204 }));
    await expect(api.purgePost("id", 1)).resolves.toBeUndefined();
    respond(jsonResponse({}));
    await expect(api.purgePost("id", 1)).rejects.toBeInstanceOf(
      ApiProtocolError,
    );
  });

  it("preserves error codes before trying the success schema", async () => {
    respond(jsonResponse({ error: "并发修改", code: "version_conflict" }, 409));
    await expect(
      api.updatePost("id", { expected_version: 1 }),
    ).rejects.toMatchObject({
      status: 409,
      code: "version_conflict",
      message: "并发修改",
      requestId: "request-1",
    });
    respond(
      new Response("<html>failure</html>", {
        status: 500,
        statusText: "Failed",
        headers: { "x-request-id": "proxy-id" },
      }),
    );
    await expect(api.getPost("id")).rejects.toMatchObject({
      status: 500,
      message: "Failed",
      requestId: "proxy-id",
    });
  });

  it("401 clears the token and notifies auth exactly once", async () => {
    setCsrfToken("secret-csrf");
    const unauthorized = vi.fn();
    setUnauthorizedHandler(unauthorized);
    const fetcher = respond(
      jsonResponse({ error: "登录失效", code: "unauthenticated" }, 401),
    );
    await expect(api.updatePost("id", {})).rejects.toBeInstanceOf(ApiError);
    expect(unauthorized).toHaveBeenCalledTimes(1);
    expect(fetcher.mock.calls[0][1].headers.get("X-CSRF-Token")).toBe(
      "secret-csrf",
    );
    fetcher.mockResolvedValue(jsonResponse(postResponse()));
    await api.updatePost("id", {});
    expect(fetcher.mock.calls[1][1].headers.has("X-CSRF-Token")).toBe(false);
  });

  it("keeps binary upload headers and validates its response", async () => {
    const fetcher = respond(jsonResponse({ id: "missing-fields" }));
    setCsrfToken("csrf");
    const file = new File(["bytes"], "image.png", { type: "image/png" });
    await expect(mediaApi.upload(file)).rejects.toBeInstanceOf(
      ApiProtocolError,
    );
    const init = fetcher.mock.calls[0][1];
    expect(init.body).toBe(file);
    expect(init.headers.get("content-type")).toBe("image/png");
    expect(init.headers.get("x-csrf-token")).toBe("csrf");
    expect(init.credentials).toBe("same-origin");
  });

  it("keeps omitted, cleared and set PATCH values distinct", async () => {
    const fetcher = vi
      .fn()
      .mockImplementation(() => Promise.resolve(jsonResponse(postResponse())));
    vi.stubGlobal("fetch", fetcher);
    await api.updatePost("id", {});
    await api.updatePost("id", {
      category_id: null,
      cover_media_id: null,
      series: [],
    });
    await api.updatePost("id", {
      category_id: "category",
      cover_media_id: "media",
    });
    expect(fetcher.mock.calls.map((call) => JSON.parse(call[1].body))).toEqual([
      {},
      { category_id: null, cover_media_id: null, series: [] },
      { category_id: "category", cover_media_id: "media" },
    ]);
  });

  it("logout accepts the followed HTML redirect", async () => {
    respond(
      new Response("<html>public site</html>", {
        headers: { "content-type": "text/html" },
      }),
    );
    await expect(api.logout()).resolves.toBeUndefined();
  });

  it("reply omits the nickname placeholder", async () => {
    const fetcher = respond(jsonResponse({ message: "已提交，等待审核", status: "pending" }, 202));
    await commentsApi.reply(
      { id: "comment", post_slug: "first" } as Parameters<
        typeof commentsApi.reply
      >[0],
      "回复",
    );
    expect(JSON.parse(fetcher.mock.calls[0][1].body)).toEqual({
      body: "回复",
      parent_id: "comment",
    });
  });
});
