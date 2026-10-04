import { z } from "zod";
import type * as Wire from "../generated";
import { responseObject as object } from "../contract";
import { string, nullable, integer, count, visibility } from "./primitives";

export const tagSummary = object<Wire.TagSummary>()({
  id: string,
  slug: string,
  name: string,
  version: count,
  public_post_count: count,
});
export const categorySummary = object<Wire.CategorySummary>()({
  id: string,
  slug: string,
  name: string,
  parent_id: nullable,
  description: nullable,
  version: count,
  pub_post_count: count,
});
export const seriesSummary = object<Wire.SeriesSummary>()({
  id: string,
  slug: string,
  name: string,
  description: nullable,
  version: count,
  post_count: count.nullable(),
  pub_post_count: count,
  cover_media_id: nullable,
  cover_url: nullable,
});
export const seriesMemberRow = object<Wire.SeriesMemberRow>()({
  id: string,
  slug: string,
  title: string,
  status: string,
  deleted: z.boolean(),
  visibility,
  author_id: string,
  position: integer,
});
export const reorderSeriesResult = object<Wire.ReorderSeriesResult>()({
  series_version: count,
  ordered_post_ids: z.array(string),
});
