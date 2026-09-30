import { createContext, useContext } from "react";

/** Kept separate so authentication does not eagerly load the scheduling polyfill. */
export const TimeZoneContext = createContext("UTC");
export const useTimeZone = () => useContext(TimeZoneContext);
