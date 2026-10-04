import type { PageDetail, Visibility } from "../../types";

export interface FormState {
  slug: string;
  title: string;
  content: string;
  visibility: Visibility;
}

export const EMPTY_FORM: FormState = {
  slug: "",
  title: "",
  content: "",
  visibility: "public",
};

export function toForm(page: PageDetail): FormState {
  return {
    slug: page.slug,
    title: page.title,
    content: page.content,
    visibility: page.visibility,
  };
}

export function formEquals(left: FormState, right: FormState): boolean {
  return (
    left.slug === right.slug &&
    left.title === right.title &&
    left.content === right.content &&
    left.visibility === right.visibility
  );
}

/** 只取用户在请求飞行期间没有改过的字段：改过的保留本地值，其余采用服务器值。 */
function pickServer<K extends keyof FormState>(
  key: K,
  current: FormState,
  sent: FormState,
  server: FormState,
): FormState[K] {
  return current[key] === sent[key] ? server[key] : current[key];
}

/** 见 PostEditScreen 的同名注释：避免请求返回时覆盖飞行途中的新输入。 */
export function mergeServer(current: FormState, sent: FormState, server: FormState): FormState {
  return {
    slug: pickServer("slug", current, sent, server),
    title: pickServer("title", current, sent, server),
    content: pickServer("content", current, sent, server),
    visibility: pickServer("visibility", current, sent, server),
  };
}
