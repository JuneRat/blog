import { ApiError, withRequestId } from "./api";

/**
 * `ApiError` → 用户可读文案的唯一出处。
 *
 * 此前这段逻辑在 15 个文件里各抄了一份，而且**并不真的相同**：
 * - 管理界面（文章/页面/标签/分类/系列/媒体/设置…）遇到 403 要加「没有权限：」前缀，
 *   让用户一眼看出是权限问题而不是接口坏了；
 * - 登录、角色目录与 `auth.tsx` 不能加前缀：登录失败就是凭据问题，说成「没有权限」
 *   只会误导；
 * - 用户与角色屏、改密弹窗需要先按业务码分支，再回落到通用文案。
 *
 * 所以这里给两个入口而不是一个布尔开关：调用点读起来就是它想要的语义，
 * 也不会有人「顺手统一」成一种而改掉另一侧的文案。
 */

/** 通用文案：`error.message` + `（错误编号 req-…）`。 */
export function messageOf(error: unknown): string {
  if (error instanceof ApiError) return withRequestId(error.message, error.requestId);
  return error instanceof Error ? error.message : "未知错误";
}

/**
 * 管理界面用的文案：403 额外标出「没有权限：」。
 *
 * 只用于**本该有权限**的管理屏幕。别用在登录或公开入口上——那里的 403 不是权限问题。
 *
 * 例外 `media_not_attachable`：那是归属校验（引用了他人私有图片），不是权限配置
 * 问题，换一张自己上传的图片即可解决；服务端文案已自解释，套「没有权限」反而误导。
 */
export function permissionMessageOf(error: unknown): string {
  if (error instanceof ApiError) {
    const base =
      error.status === 403 && error.code !== "media_not_attachable"
        ? `没有权限：${error.message}`
        : error.message;
    return withRequestId(base, error.requestId);
  }
  return error instanceof Error ? error.message : "未知错误";
}
