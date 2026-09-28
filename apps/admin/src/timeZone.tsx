import { Temporal } from "@js-temporal/polyfill";
import { createContext, useContext } from "react";

export const TimeZoneContext = createContext("UTC");
export const useTimeZone = () => useContext(TimeZoneContext);

/** API values are instants, never browser-local wall times. */
export function formatDateTime(value: string, timeZone: string): string {
  try {
    const local = Temporal.Instant.from(value).toZonedDateTimeISO(timeZone);
    return `${local.toPlainDateTime().toString({ smallestUnit: "second" }).replace("T", " ")} ${local.offset} (${timeZone})`;
  } catch { return value; }
}

export function dateTimeInput(value: string | null, timeZone: string): string {
  if (!value) return "";
  try {
    return Temporal.Instant.from(value).toZonedDateTimeISO(timeZone)
      .toPlainDateTime().toString({ smallestUnit: "minute" });
  } catch { return ""; }
}

/** Reject DST gaps/folds instead of silently scheduling a different instant. */
export function inputToInstant(value: string, timeZone: string): string | null {
  if (!/^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}(?::\d{2}(?:\.\d+)?)?$/.test(value)) return null;
  try {
    return Temporal.PlainDateTime.from(value, { overflow: "reject" })
      .toZonedDateTime(timeZone, { disambiguation: "reject" })
      .toInstant().toString({ smallestUnit: "millisecond" });
  } catch { return null; }
}

export const invalidLocalTime = "时间无效，或处于夏令时切换的重复/不存在时段，请选择其它时间。";
