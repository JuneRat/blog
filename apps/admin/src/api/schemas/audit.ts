import { z } from "zod";
import type * as Wire from "../generated";
import { responseObject as object } from "../contract";
import { string, nullable } from "./primitives";

export const auditField = object<Wire.AuditField>()({
  key: string,
  value: string,
});
export const auditRecord = object<Wire.AuditRecord>()({
  id: string,
  created_at: string,
  actor_id: nullable,
  actor_display: nullable,
  ip_address: nullable,
  action: string,
  target_type: string,
  target_id: string,
  summary: z.array(auditField),
});
export const auditPage = object<Wire.AuditPage>()({
  items: z.array(auditRecord),
  next_cursor: nullable,
});
