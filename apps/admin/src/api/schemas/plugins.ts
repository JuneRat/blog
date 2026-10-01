import { z } from "zod";
import type * as Wire from "../generated";
import { responseObject as object } from "../contract";
import { count, string } from "./primitives";

const value = z.union([z.boolean(), z.number().int().min(-2147483648).max(2147483647), string]);
const field = object<Wire.PluginConfigField>()({ key: string, label: string, description: string, default: value });
const plugin = object<Wire.PluginView>()({
  id: string, name: string, description: string, version: string,
  hooks: z.array(z.enum(["content", "page_head"])), config_fields: z.array(field),
  available: z.boolean(), enabled: z.boolean(), config: z.record(string, value),
});
export const plugins = object<Wire.PluginsView>()({ version: count, plugins: z.array(plugin) });
