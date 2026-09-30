import type * as Wire from "../generated";
import { responseObject as object } from "../contract";
import { string } from "./primitives";

export const previewResult = object<Wire.PreviewResult>()({
  content_html: string,
});
