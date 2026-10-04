import type { HtmlRebuildJob, HtmlRebuildView } from "./generated";
import { request } from "./client";
import { htmlRebuildJob, htmlRebuildView } from "./schemas/maintenance";

const endpoint = "/api/admin/v1/maintenance/html-rebuild";

export const maintenanceApi = {
  get: (signal?: AbortSignal): Promise<HtmlRebuildView> =>
    request(htmlRebuildView, endpoint, { signal }),
  start: (): Promise<HtmlRebuildJob> =>
    request(htmlRebuildJob, endpoint, { method: "POST" }),
};
