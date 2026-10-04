import * as s from "./schemas/audit";
import { request } from "./client";
import type { AuditFilter, AuditPage } from "../types";

export const auditApi = {
  list: (filter: AuditFilter, cursor?: string): Promise<AuditPage> => {
    const params = new URLSearchParams({ limit: "50" });
    for (const [key, value] of Object.entries(filter)) {
      if (value !== undefined && value !== "" && value !== false)
        params.set(key, String(value));
    }
    if (cursor) params.set("cursor", cursor);
    return request(s.auditPage, `/api/admin/v1/audit-logs?${params}`);
  },
};
