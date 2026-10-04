import type { PluginsView, SavePluginInput } from "./generated";
import { json, request } from "./client";
import { plugins } from "./schemas/plugins";

export const pluginsApi = {
  get: (signal?: AbortSignal): Promise<PluginsView> => request(plugins, "/api/admin/v1/plugins", { signal }),
  save: (id: string, input: SavePluginInput): Promise<PluginsView> => request(plugins, `/api/admin/v1/plugins/${encodeURIComponent(id)}`, {
    method: "PUT", body: json<SavePluginInput>(input),
  }),
};
