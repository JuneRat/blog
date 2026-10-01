import type * as Wire from "../generated";
import { responseObject as object } from "../contract";
import { string } from "./primitives";

export const previewResult = object<Wire.PreviewResult>()({
  content_html: string,
});

export const contentPreviewResult = object<Wire.ContentPreviewResult>()({
  content_html: string,
  head_html: string,
});
