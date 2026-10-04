// Compatibility facade: resource adapters own endpoints; client owns transport.
export {
  ApiError,
  ApiProtocolError,
  withRequestId,
  setCsrfToken,
  setUnauthorizedHandler,
} from "./client";
export type {
  CreatePostInput,
  EditPostInput,
  CreatePageInput,
  EditPageInput,
  PasswordLoginInput,
  CreateUserInput,
  CreateTagInput,
  RenameTagInput,
  CreateCategoryInput,
  UpdateCategoryInput,
  CreateSeriesInput,
  UpdateSeriesInput,
  SaveSiteSettingsInput,
  RetentionSettings,
} from "./generated";
export type { CommentItem, CommentPage, CommentPolicy } from "./responseTypes";
import { postsApi } from "./posts";
import { pagesApi } from "./pages";
import { identityApi } from "./identity";
import { contentApi } from "./content";
import { tagsApi } from "./taxonomy";
export const api = {
  ...postsApi,
  ...pagesApi,
  ...identityApi,
  ...contentApi,
  ...tagsApi,
};
export { loginUrl } from "./identity";
export { categoryApi, seriesApi } from "./taxonomy";
export { settingsApi, themeSettingsApi, retentionApi } from "./settings";
export { mediaApi } from "./media";
export { commentsApi } from "./comments";
export { auditApi } from "./audit";
