import type * as Wire from "./generated";
import * as s from "./schemas/preview";
import { json, request } from "./client";

export const contentApi = {
  previewContent: (content: string): Promise<Wire.ContentPreviewResult> =>
    request(s.contentPreviewResult, "/api/admin/v1/content-preview", {
      method: "POST",
      body: json<Wire.ContentPreviewInput>({ content }),
    }),
};
