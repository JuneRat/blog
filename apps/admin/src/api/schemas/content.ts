import { z } from "zod";
import type * as Wire from "../generated";
import { responseObject as object } from "../contract";
import { string, nullable, count, visibility } from "./primitives";

export const pageSummary = object<Wire.PageSummary>()({
  id: string,
  slug: string,
  title: string,
  status: string,
  visibility,
  version: count,
  published_at: nullable,
  updated_at: string,
});
export const pageDetail = object<Wire.PageDetail>()({
  has_pending_changes: z.boolean().optional(),
  ...pageSummary.shape,
  content: string,
});
export const postSummary = object<Wire.PostSummary>()({
  author_username: string,
  ...pageSummary.shape,
  author_id: string,
});
export const seriesPlacement = object<Wire.SeriesPlacement>()({
  series_id: string,
  position: count.max(2147483647),
});
export const postDetail = object<Wire.PostDetail>()({
  has_pending_changes: z.boolean().optional(),
  ...pageSummary.shape,
  author_id: string,
  tag_ids: z.array(string),
  category_id: nullable,
  series: z.array(seriesPlacement),
  cover_media_id: nullable,
  cover_url: nullable,
  excerpt: nullable,
  content: string,
});
export const postPage = object<Wire.ContentPage<Wire.PostSummary>>()({
  items: z.array(postSummary),
  total: count,
  page: count,
  per_page: count,
});
export const pagePage = object<Wire.ContentPage<Wire.PageSummary>>()({
  items: z.array(pageSummary),
  total: count,
  page: count,
  per_page: count,
});

export const revisionSummary = object<Wire.ContentRevisionSummary>()({ id: string, version: count, title: string, created_at: string, actor_id: nullable });
export const revisionList = z.array(revisionSummary);
export const revisionDetail = object<Wire.ContentRevisionDetail>()({ slug: string, title: string, content: string, visibility: string, excerpt: nullable, tag_ids: z.array(string), category_id: nullable, series: z.array(seriesPlacement), cover_media_id: nullable });
