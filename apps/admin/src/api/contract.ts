import { z } from "zod";

/** Every wire field must have a validator; new/removed Rust fields fail typecheck.
 * Some string fields are deliberately narrowed to the values accepted by the UI.
 * Unknown response fields are tolerated for rolling deployments.
 */
// Permit enum refinement only when Rust exposes an unrestricted string. Closed Rust
// enums, nullability, optional fields and nested collection shapes must still agree.
type WireShape<T, Wire> = T extends string
  ? string extends Wire
    ? string
    : T
  : T extends Array<infer E>
    ? WireShape<E, NonNullable<Wire> extends Array<infer W> ? W : never>[]
    : T extends object
      ? {
          [K in keyof T]: WireShape<
            T[K],
            K extends keyof NonNullable<Wire> ? NonNullable<Wire>[K] : never
          >;
        }
      : T;
// `any` is assignable in both directions and defeats ordinary shape equality.
// Check recursively: a single permissive leaf also bypasses runtime validation.
type ContainsAny<T> = 0 extends 1 & T
  ? true
  : T extends ReadonlyArray<infer E>
    ? ContainsAny<E>
    : T extends object
      ? { [K in keyof T]-?: ContainsAny<T[K]> }[keyof T]
      : false;
export function responseObject<T>() {
  return <S extends z.ZodRawShape>(
    shape: S & { [K in keyof T]-?: z.ZodType<T[K]> } & Record<
        Exclude<keyof S, keyof T>,
        never
      > &
      (true extends ContainsAny<z.output<z.ZodObject<S>>>
        ? { __unsafe_any: never }
        : unknown) &
      (WireShape<T, T> extends WireShape<z.output<z.ZodObject<S>>, T>
        ? unknown
        : { __incompatible_wire_shape: never }),
  ) => z.object<S>(shape);
}
