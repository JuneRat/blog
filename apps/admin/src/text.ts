/**
 * 按 Unicode 码点计数（`Array.from` 逐码点迭代）。
 *
 * 后端的字符上限用 Rust `str::chars().count()` 判定，计的是码点；而
 * `String.prototype.length` 计的是 UTF-16 代码单元，会把 emoji 等增补平面
 * 字符算作 2（`"😀".length === 2`），导致「后端允许、前端报超长」。
 *
 * 凡取值受后端字符上限约束的输入，长度判定都必须用本函数；
 * HTML `maxLength` 同样按代码单元计数，不能用来表达码点上限。
 */
export function codePointLength(value: string): number {
  return Array.from(value).length;
}
