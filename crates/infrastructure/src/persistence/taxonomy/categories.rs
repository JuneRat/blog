use super::super::content::audit_content;
use super::super::sql::{POST_PUBLIC_PREDICATE, map_row_error, map_sqlx_error};
use application::error::UseCaseError;
use application::ports::{
    CategoryDeleteOutcome, CategoryRepository, CategoryWithUsage, PublicCategorySummary,
    PublicPostSummary, PublicUrlEntry, PublishedCategoryQuery,
};
use async_trait::async_trait;
use sqlx::{PgPool, Row};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// 分类目录：树锁、防环与引用保护
// ---------------------------------------------------------------------------

/// 分类树事务锁：创建/移动/删除统一取得，串行化祖先链检查与写入。
///
/// 不锁读路径（目录读取无锁）；只约束写写并发——两条并发移动若各自
/// 通过了环检查再先后提交，可能拼出环（检查结果在锁外失效）。
use crate::locks::CATEGORY_TREE as CATEGORY_TREE_LOCK;

pub struct PostgresCategoryRepository {
    pool: PgPool,
}

impl PostgresCategoryRepository {
    pub fn new(database: crate::Database) -> Self {
        let pool = database.pool;
        Self { pool }
    }
}

const CATEGORY_COLUMNS: &str =
    "id, name, slug, parent_id, description, version, created_at, updated_at";

fn category_from_row(
    row: &sqlx::postgres::PgRow,
) -> Result<domain::content::CategorySnapshot, UseCaseError> {
    Ok(domain::content::CategorySnapshot {
        id: row.try_get("id").map_err(map_row_error)?,
        name: row.try_get("name").map_err(map_row_error)?,
        slug: row.try_get("slug").map_err(map_row_error)?,
        parent_id: row.try_get("parent_id").map_err(map_row_error)?,
        description: row.try_get("description").map_err(map_row_error)?,
        version: row.try_get("version").map_err(map_row_error)?,
        created_at: row.try_get("created_at").map_err(map_row_error)?,
        updated_at: row.try_get("updated_at").map_err(map_row_error)?,
    })
}

fn category_usage_from_row(row: &sqlx::postgres::PgRow) -> Result<CategoryWithUsage, UseCaseError> {
    Ok(CategoryWithUsage {
        snapshot: category_from_row(row)?,
        public_post_count: row.try_get("public_post_count").map_err(map_row_error)?,
    })
}

/// 公开文章计数子查询（直接归属；与公开分类页同口径）。
const CATEGORY_PUBLIC_COUNT: &str = "(SELECT count(*) FROM posts p WHERE p.category_id = t.id AND p.status = 'published' \
      AND p.visibility = 'public' AND p.deleted_at IS NULL AND p.published_at <= now())";

/// 树锁内完整遍历祖先链。UNION 按节点去重，旧环也能终止；
/// 必须到达根，不能把截断或接入已有环当成校验成功。
async fn validate_parent_chain(
    tx: &mut sqlx::PgConnection,
    parent_id: Uuid,
    self_id: Uuid,
) -> Result<(), UseCaseError> {
    let (contains_self, reaches_root): (Option<bool>, Option<bool>) = sqlx::query_as(
        r#"
        WITH RECURSIVE up(id, parent_id) AS (
            SELECT c.id, c.parent_id FROM categories c WHERE c.id = $1
            UNION
            SELECT c.id, c.parent_id
            FROM categories c JOIN up ON up.parent_id = c.id
        )
        SELECT bool_or(id = $2), bool_or(parent_id IS NULL) FROM up
        "#,
    )
    .bind(parent_id)
    .bind(self_id)
    .fetch_one(&mut *tx)
    .await
    .map_err(map_sqlx_error)?;
    match contains_self {
        None => Err(UseCaseError::Invalid("父分类不存在".into())),
        Some(true) => Err(UseCaseError::Invalid(
            "目标父分类的祖先链包含自身，会形成环".into(),
        )),
        Some(false) if reaches_root != Some(true) => {
            Err(UseCaseError::DataCorrupt("父分类的祖先链已存在环".into()))
        }
        Some(false) => Ok(()),
    }
}

#[async_trait]
impl CategoryRepository for PostgresCategoryRepository {
    async fn insert(
        &self,
        aggregate: &domain::content::Category,
        audit_actor: application::audit::AuditContext,
    ) -> Result<(), UseCaseError> {
        let snapshot = aggregate.snapshot();
        // 树锁内确认父节点仍存在且祖先链完好，避免新节点接入历史环。
        let mut tx = self.pool.begin().await.map_err(map_sqlx_error)?;
        sqlx::query("SELECT pg_advisory_xact_lock($1::int, $2::int)")
            .bind(CATEGORY_TREE_LOCK.0)
            .bind(CATEGORY_TREE_LOCK.1)
            .execute(&mut *tx)
            .await
            .map_err(map_sqlx_error)?;
        if let Some(parent_id) = snapshot.parent_id {
            validate_parent_chain(&mut tx, parent_id, snapshot.id).await?;
        }
        sqlx::query(
            "INSERT INTO categories (id, name, slug, parent_id, description, version, \
             created_at, updated_at) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        )
        .bind(snapshot.id)
        .bind(&snapshot.name)
        .bind(&snapshot.slug)
        .bind(snapshot.parent_id)
        .bind(&snapshot.description)
        .bind(snapshot.version)
        .bind(snapshot.created_at)
        .bind(snapshot.updated_at)
        .execute(&mut *tx)
        .await
        .map_err(map_sqlx_error)?;
        audit_content(
            &mut tx,
            audit_actor,
            "category.create",
            "category",
            snapshot.id,
            serde_json::json!({"version":snapshot.version,"parent_id":snapshot.parent_id}),
        )
        .await?;
        tx.commit().await.map_err(map_sqlx_error)?;
        Ok(())
    }

    async fn find_by_slug(
        &self,
        slug: &str,
    ) -> Result<Option<domain::content::CategorySnapshot>, UseCaseError> {
        let row = sqlx::query(&format!(
            "SELECT {CATEGORY_COLUMNS} FROM categories WHERE slug = $1"
        ))
        .bind(slug)
        .fetch_optional(&self.pool)
        .await
        .map_err(map_sqlx_error)?;
        row.as_ref().map(category_from_row).transpose()
    }

    async fn list(&self) -> Result<Vec<CategoryWithUsage>, UseCaseError> {
        let rows = sqlx::query(&format!(
            "SELECT {CATEGORY_COLUMNS}, {CATEGORY_PUBLIC_COUNT} AS public_post_count \
             FROM categories t ORDER BY t.slug"
        ))
        .fetch_all(&self.pool)
        .await
        .map_err(map_sqlx_error)?;
        rows.iter().map(category_usage_from_row).collect()
    }

    async fn update(
        &self,
        id: Uuid,
        name: &str,
        description: Option<&str>,
        parent_id: Option<Uuid>,
        expected_version: i64,
        audit_actor: application::audit::AuditContext,
    ) -> Result<Option<CategoryWithUsage>, UseCaseError> {
        let mut tx = self.pool.begin().await.map_err(map_sqlx_error)?;
        // 树锁内完成「环检查 + 写入」：锁外的检查结果可能被并发移动作废。
        sqlx::query("SELECT pg_advisory_xact_lock($1::int, $2::int)")
            .bind(CATEGORY_TREE_LOCK.0)
            .bind(CATEGORY_TREE_LOCK.1)
            .execute(&mut *tx)
            .await
            .map_err(map_sqlx_error)?;

        let current: Option<(Option<Uuid>, i64)> =
            sqlx::query_as("SELECT parent_id, version FROM categories WHERE id = $1")
                .bind(id)
                .fetch_optional(&mut *tx)
                .await
                .map_err(map_sqlx_error)?;
        let Some((current_parent, current_version)) = current else {
            tx.commit().await.map_err(map_sqlx_error)?;
            return Ok(None);
        };
        if current_version != expected_version {
            tx.commit().await.map_err(map_sqlx_error)?;
            return Ok(None);
        }

        if parent_id != current_parent
            && let Some(new_parent) = parent_id
        {
            validate_parent_chain(&mut tx, new_parent, id).await?;
        }

        let row = sqlx::query(&format!(
            "UPDATE categories t SET name = $3, description = $4, parent_id = $5, \
             version = CASE WHEN (name, description, parent_id) IS DISTINCT FROM ($3::text, $4::text, $5::uuid) \
                            THEN version + 1 ELSE version END, \
             updated_at = CASE WHEN (name, description, parent_id) IS DISTINCT FROM ($3::text, $4::text, $5::uuid) \
                               THEN now() ELSE updated_at END \
             WHERE id = $1 AND version = $2 \
             RETURNING {CATEGORY_COLUMNS}, {CATEGORY_PUBLIC_COUNT} AS public_post_count"
        ))
        .bind(id)
        .bind(expected_version)
        .bind(name)
        .bind(description)
        .bind(parent_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(map_sqlx_error)?;
        let result = row.as_ref().map(category_usage_from_row).transpose()?;
        if let Some(row) = &result
            && row.snapshot.version != expected_version
        {
            let snapshot = &row.snapshot;
            audit_content(
                &mut tx,
                audit_actor,
                "category.update",
                "category",
                id,
                serde_json::json!({"version":snapshot.version,"parent_id":snapshot.parent_id}),
            )
            .await?;
        }
        tx.commit().await.map_err(map_sqlx_error)?;
        Ok(result)
    }

    async fn delete(
        &self,
        id: Uuid,
        expected_version: i64,
        audit_actor: application::audit::AuditContext,
    ) -> Result<CategoryDeleteOutcome, UseCaseError> {
        let mut tx = self.pool.begin().await.map_err(map_sqlx_error)?;
        sqlx::query("SELECT pg_advisory_xact_lock($1::int, $2::int)")
            .bind(CATEGORY_TREE_LOCK.0)
            .bind(CATEGORY_TREE_LOCK.1)
            .execute(&mut *tx)
            .await
            .map_err(map_sqlx_error)?;

        let current: Option<(i64,)> =
            sqlx::query_as("SELECT version FROM categories WHERE id = $1")
                .bind(id)
                .fetch_optional(&mut *tx)
                .await
                .map_err(map_sqlx_error)?;
        let Some((current_version,)) = current else {
            tx.commit().await.map_err(map_sqlx_error)?;
            return Ok(CategoryDeleteOutcome::Gone);
        };
        if current_version != expected_version {
            tx.commit().await.map_err(map_sqlx_error)?;
            return Ok(CategoryDeleteOutcome::StaleVersion);
        }

        // 引用计数不过滤可见性：草稿/私密/回收站同样占用。
        let (posts,): (i64,) = sqlx::query_as("SELECT count(*) FROM posts WHERE category_id = $1")
            .bind(id)
            .fetch_one(&mut *tx)
            .await
            .map_err(map_sqlx_error)?;
        let (children,): (i64,) =
            sqlx::query_as("SELECT count(*) FROM categories WHERE parent_id = $1")
                .bind(id)
                .fetch_one(&mut *tx)
                .await
                .map_err(map_sqlx_error)?;
        if posts > 0 || children > 0 {
            tx.commit().await.map_err(map_sqlx_error)?;
            return Ok(CategoryDeleteOutcome::Referenced { posts, children });
        }

        sqlx::query("DELETE FROM categories WHERE id = $1")
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(map_sqlx_error)?;
        audit_content(
            &mut tx,
            audit_actor,
            "category.purge",
            "category",
            id,
            serde_json::json!({"version":expected_version}),
        )
        .await?;
        tx.commit().await.map_err(map_sqlx_error)?;
        Ok(CategoryDeleteOutcome::Deleted)
    }
}

// ---------------------------------------------------------------------------
// 公开分类页查询
// ---------------------------------------------------------------------------

pub struct PostgresPublishedCategoryQuery {
    pool: PgPool,
}

impl PostgresPublishedCategoryQuery {
    pub fn new(database: crate::Database) -> Self {
        let pool = database.pool;
        Self { pool }
    }
}

#[async_trait]
impl PublishedCategoryQuery for PostgresPublishedCategoryQuery {
    async fn list_public_categories(
        &self,
        limit: i64,
    ) -> Result<Vec<PublicCategorySummary>, UseCaseError> {
        let rows: Vec<(String, String)> =
            sqlx::query_as("SELECT slug, name FROM categories ORDER BY slug LIMIT $1")
                .bind(limit.clamp(1, 50))
                .fetch_all(&self.pool)
                .await
                .map_err(map_sqlx_error)?;
        Ok(rows
            .into_iter()
            .map(|(slug, name)| PublicCategorySummary { slug, name })
            .collect())
    }
    async fn find_public_by_slug(
        &self,
        slug: &str,
    ) -> Result<Option<PublicCategorySummary>, UseCaseError> {
        let row: Option<(String, String)> =
            sqlx::query_as("SELECT slug, name FROM categories WHERE slug = $1")
                .bind(slug)
                .fetch_optional(&self.pool)
                .await
                .map_err(map_sqlx_error)?;
        Ok(row.map(|(slug, name)| PublicCategorySummary { slug, name }))
    }

    async fn list_public_posts_by_category(
        &self,
        category_slug: &str,
        limit: i64,
        offset: i64,
    ) -> Result<(Vec<PublicPostSummary>, i64), UseCaseError> {
        let limit = limit.clamp(1, 100);
        let offset = offset.max(0);
        // 直接归属（不含子树）：父分类不自动成为文章的另一条直接分类关系。
        let rows = sqlx::query(&format!(
            r#"
            WITH matching AS (
            SELECT p.id, p.title, p.slug, p.excerpt, p.published_at,
                   COALESCE(NULLIF(u.display_name, ''), u.username) AS author_display,
                   u.avatar_media_id AS author_avatar_media_id
            FROM categories c
            JOIN posts p ON p.category_id = c.id
            JOIN users u ON u.id = p.author_id
            WHERE c.slug = $1 AND {POST_PUBLIC_PREDICATE}
            )
            SELECT page.*, totals.total
            FROM (SELECT count(*) AS total FROM matching) totals
            LEFT JOIN (
                SELECT * FROM matching
                ORDER BY published_at DESC, id DESC
                LIMIT $2 OFFSET $3
            ) page ON true
            ORDER BY page.published_at DESC, page.id DESC
            "#
        ))
        .bind(category_slug)
        .bind(limit)
        .bind(offset)
        .fetch_all(&self.pool)
        .await
        .map_err(map_sqlx_error)?;

        let total = rows
            .first()
            .map(|row| row.try_get::<i64, _>("total").map_err(map_row_error))
            .transpose()?
            .unwrap_or(0);
        let posts = rows
            .iter()
            .map(|row| {
                if row
                    .try_get::<Option<String>, _>("slug")
                    .map_err(map_row_error)?
                    .is_none()
                {
                    return Ok(None);
                }
                Ok(Some(PublicPostSummary {
                    title: row.try_get("title").map_err(map_row_error)?,
                    slug: row.try_get("slug").map_err(map_row_error)?,
                    excerpt: row.try_get("excerpt").map_err(map_row_error)?,
                    published_at: row.try_get("published_at").map_err(map_row_error)?,
                    author_display: row.try_get("author_display").map_err(map_row_error)?,
                    author_avatar_media_id: row
                        .try_get("author_avatar_media_id")
                        .map_err(map_row_error)?,
                }))
            })
            .collect::<Result<Vec<_>, UseCaseError>>()?
            .into_iter()
            .flatten()
            .collect();
        Ok((posts, total))
    }

    async fn list_public_directories(
        &self,
        limit: i64,
    ) -> Result<Vec<PublicUrlEntry>, UseCaseError> {
        // 与标签页同口径：INNER JOIN 只保留有直接归属公开文章的分类，空分类不收。
        // lastmod 取「分类自身改名时间」与「成员文章最近更新时间」的较晚者：
        // 改分类名也会改变公开页展示内容。
        let rows = sqlx::query(&format!(
            r#"
            SELECT c.slug, GREATEST(c.updated_at, max(p.updated_at)) AS updated_at
            FROM categories c
            JOIN posts p ON p.category_id = c.id
            WHERE {POST_PUBLIC_PREDICATE}
            GROUP BY c.id, c.slug, c.updated_at
            ORDER BY c.slug
            LIMIT $1
            "#
        ))
        .bind(limit.max(0))
        .fetch_all(&self.pool)
        .await
        .map_err(map_sqlx_error)?;
        rows.iter()
            .map(|row| {
                Ok(PublicUrlEntry {
                    slug: row.try_get("slug").map_err(map_row_error)?,
                    updated_at: row.try_get("updated_at").map_err(map_row_error)?,
                })
            })
            .collect()
    }
}

#[async_trait::async_trait]
impl application::ports::CategoryLookup for PostgresCategoryRepository {
    async fn existing_id(&self, id: Uuid) -> Result<bool, UseCaseError> {
        let hit: Option<(Uuid,)> = sqlx::query_as("SELECT id FROM categories WHERE id = $1")
            .bind(id)
            .fetch_optional(&self.pool)
            .await
            .map_err(map_sqlx_error)?;
        Ok(hit.is_some())
    }
}
