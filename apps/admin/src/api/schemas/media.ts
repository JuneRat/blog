import { z } from "zod";
import type * as Wire from "../generated";
import { responseObject as object } from "../contract";
import { string, nullable, count, visibility } from "./primitives";

export const mediaAsset = object<Wire.MediaAsset>()({
  id: string,
  original_name: string,
  mime: string,
  byte_size: count,
  width: count,
  height: count,
  deleted_at: nullable,
  version: count,
  created_at: string,
  owner_id: nullable,
  owner_display: string,
  url: string,
  reference_count: count,
});
export const mediaPage = object<Wire.MediaPage>()({
  items: z.array(mediaAsset),
  total: count,
  page: count,
  per_page: count,
});
export const mediaReference = object<Wire.MediaReference>()({
  kind: z.enum(["post", "page", "series", "user", "site"]),
  content_id: string,
  slug: string,
  title: string,
  status: string,
  visibility,
  deleted: z.boolean(),
  public: z.boolean(),
});
export const mediaUsageView = object<Wire.MediaUsageView>()({
  media: mediaAsset,
  references: z.array(mediaReference),
  hidden_references: count,
});
