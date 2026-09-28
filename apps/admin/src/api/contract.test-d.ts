// Compiled by tsc, not executed by Vitest: these failures are the contract's CI gate.
import { z } from "zod";
import { responseObject } from "./contract";
import type { AccountStatus, Profile } from "./generated";

const profileField = responseObject<Pick<Profile, "display_name">>();
profileField({ display_name: z.string().nullable() });
// @ts-expect-error a response validator must accept the wire's explicit null
profileField({ display_name: z.string() });
// @ts-expect-error missing wire fields must not disappear from the validator
profileField({});
// @ts-expect-error schema fields without a matching wire DTO field are drift
profileField({ display_name: z.string().nullable(), extra: z.string() });
// @ts-expect-error an incompatible primitive cannot pass the boundary
profileField({ display_name: z.number().nullable() });

const status = responseObject<{ status: AccountStatus }>();
status({ status: z.enum(["active", "disabled"]) });
// @ts-expect-error all variants of a closed Rust enum must be handled
status({ status: z.literal("active") });
// @ts-expect-error unrelated enum values are incompatible
status({ status: z.enum(["active", "disabled", "unknown"]) });

const optional = responseObject<{ optional?: string }>();
optional({ optional: z.string().optional() });
// @ts-expect-error optional wire fields cannot become required in the validator
optional({ optional: z.string() });
