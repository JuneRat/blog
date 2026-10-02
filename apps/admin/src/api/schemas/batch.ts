import { z } from "zod";
import type * as Wire from "../generated";
import { responseObject as object } from "../contract";
import { string, count } from "./primitives";

export const batchItemResult = object<Wire.BatchItemResult>()({
  id: string,
  version: count.nullable(),
  changed: z.boolean(),
});

export const batchResult = object<Wire.BatchResult>()({
  items: z.array(batchItemResult),
  affected: count,
});
