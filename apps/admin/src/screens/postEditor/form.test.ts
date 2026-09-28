import { describe, expect, it } from "vitest";
import {
  EMPTY_FORM,
  formEquals,
  mergeServer,
  normalizeForm,
  validSeries,
  type FormState,
} from "./form";

function form(value: Partial<FormState>): FormState {
  return normalizeForm({ ...EMPTY_FORM, ...value });
}

describe("保存响应与请求期间的编辑合并", () => {
  it("仅更新未改字段；保留标签、分类、封面及系列的后续修改", () => {
    const sent = form({
      title: "提交标题",
      content: "提交正文",
      tagIds: ["a"],
      categoryId: "a",
      coverMediaId: "a",
      seriesIds: ["a"],
      seriesPositions: { a: "1" },
    });
    const current = form({
      ...sent,
      content: "继续写",
      tagIds: ["b"],
      categoryId: null,
      coverMediaId: null,
      seriesIds: ["a", "b"],
      seriesPositions: { a: "2", b: "3" },
    });
    const server = form({
      ...sent,
      title: "规范化标题",
      content: "服务器正文",
      tagIds: ["server"],
      categoryId: "server",
      coverMediaId: "server",
      seriesIds: ["server"],
    });
    const merged = mergeServer(current, sent, server);
    expect(merged).toEqual({ ...current, title: "规范化标题" });
    expect(formEquals(merged, server)).toBe(false);
  });
  it("标签顺序不构成修改，系列顺序和序号作为一组比较", () => {
    const sent = form({
      tagIds: ["a", "b"],
      seriesIds: ["a", "b"],
      seriesPositions: { a: "0", b: "1" },
    });
    const current = form({
      ...sent,
      tagIds: ["b", "a"],
      seriesIds: ["b", "a"],
    });
    const server = form({
      ...sent,
      tagIds: ["c"],
      seriesPositions: { a: "3", b: "4" },
    });
    expect(mergeServer(current, sent, server)).toEqual(server);
    const edited = form({ ...current, seriesPositions: { a: "8", b: "1" } });
    expect(mergeServer(edited, sent, server).seriesPositions).toEqual({
      a: "8",
      b: "1",
    });
  });
  it("恢复和保存归一化清空值，同时拒绝非法系列权重", () => {
    const cleared = form({
      categoryId: "",
      coverMediaId: "",
      seriesIds: [],
      seriesPositions: { removed: "1" },
    });
    expect(cleared).toEqual(EMPTY_FORM);
    for (const value of ["", "-1", "0.1", "2147483648"])
      expect(
        validSeries(form({ seriesIds: ["s"], seriesPositions: { s: value } })),
      ).toBe(false);
    expect(
      validSeries(form({ seriesIds: ["s"], seriesPositions: { s: "0" } })),
    ).toBe(true);
  });
});
