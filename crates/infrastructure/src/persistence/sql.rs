use sqlx::postgres::PgDatabaseError;

use application::error::{ConflictKind, UseCaseError};

/// 匿名公开条件（与 docs/database-design.md §5 保持一致）。
pub(super) const POST_PUBLIC_PREDICATE: &str = "p.status = 'published' AND p.visibility = 'public' AND p.deleted_at IS NULL AND p.published_at <= now()";

/// 页面与文章使用相同公开条件。
pub(super) const PAGE_PUBLIC_PREDICATE: &str = POST_PUBLIC_PREDICATE;

// ---------------------------------------------------------------------------
// 错误映射
// ---------------------------------------------------------------------------

/// 唯一约束名 → 结构化冲突原因。
///
/// 这里只做「约束名到枚举」的翻译；是否给某个原因分配独立业务码由接口层决定。
/// 未知约束回 `Unknown`：仍然拒绝写入，只是不给前端编造字段名。
fn unique_conflict_target(error: &PgDatabaseError) -> Option<ConflictKind> {
    match error.constraint()? {
        "posts_slug_key" | "pages_slug_key" => Some(ConflictKind::Slug),
        "users_username_ci_uq" => Some(ConflictKind::Username),
        "users_email_ci_uq" => Some(ConflictKind::Email),
        "oauth_accounts_pkey" => Some(ConflictKind::ExternalIdentity),
        "categories_slug_key" | "series_slug_key" | "tags_slug_key" => Some(ConflictKind::Slug),
        "roles_code_key" => Some(ConflictKind::RoleSlug),
        "permissions_pkey" => Some(ConflictKind::PermissionKey),
        _ => None,
    }
}

pub(super) fn map_sqlx_error(error: sqlx::Error) -> UseCaseError {
    if let sqlx::Error::Database(db) = &error {
        let pg = db.try_downcast_ref::<PgDatabaseError>();
        if let Some(pg) = pg {
            if pg.code() == "23505" {
                return UseCaseError::Conflict(
                    unique_conflict_target(pg).unwrap_or(ConflictKind::Unknown),
                );
            }
            // 文章关联不存在的标签：用例层已前置校验，这里是并发删除标签的兜底，
            // 翻译成可定位的参数错误而不是裸存储错误。
            if pg.code() == "23503" && pg.constraint() == Some("post_tags_tag_id_fkey") {
                return UseCaseError::Invalid("所选标签不存在或刚被删除".into());
            }
            if pg.code() == "23503" && pg.constraint() == Some("post_series_series_id_fkey") {
                return UseCaseError::Invalid("所选系列不存在或刚被删除".into());
            }
            // 同理：并发删除分类时的 FK 兜底。
            if pg.code() == "23503" && pg.constraint() == Some("posts_category_id_fkey") {
                return UseCaseError::Invalid("所选分类不存在或刚被删除".into());
            }
        }
    }
    UseCaseError::Repository(error.to_string())
}

pub(super) fn map_row_error(error: sqlx::Error) -> UseCaseError {
    UseCaseError::Repository(format!("行映射失败：{error}"))
}
