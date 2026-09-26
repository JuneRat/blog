use async_trait::async_trait;
use sqlx::{PgPool, Row};
use time::OffsetDateTime;
use uuid::Uuid;

use application::error::UseCaseError;
use application::ports::{
    CategoryDeleteOutcome, CategoryRepository, CategoryWithUsage, MediaContentKind,
    PublicCategorySummary, PublicPostSummary, PublicSeriesSummary, PublicTagSummary,
    PublicUrlEntry, PublishedCategoryQuery, PublishedSeriesQuery, PublishedTagQuery,
    ReorderOutcome, SeriesDeleteOutcome, SeriesMember, SeriesRepository, SeriesWithUsage,
    TagDeleteOutcome, TagRepository, TagWithUsage,
};

use super::media::{clear_media_refs, media_ids_for, sync_media_refs};
use super::sql::{POST_PUBLIC_PREDICATE, map_row_error, map_sqlx_error};

// ---------------------------------------------------------------------------
// 标签目录与文章关联
// ---------------------------------------------------------------------------

pub struct PostgresTagRepository {
    pool: PgPool,
}

impl PostgresTagRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

const TAG_COLUMNS: &str = "id, name, slug, version, created_at";

fn tag_from_row(row: &sqlx::postgres::PgRow) -> Result<domain::content::TagSnapshot, UseCaseError> {
    Ok(domain::content::TagSnapshot {
        id: row.try_get("id").map_err(map_row_error)?,
        name: row.try_get("name").map_err(map_row_error)?,
        slug: row.try_get("slug").map_err(map_row_error)?,
        version: row.try_get("version").map_err(map_row_error)?,
        created_at: row.try_get("created_at").map_err(map_row_error)?,
    })
}

/// 公开计数子查询：与公开文章谓词同口径（草稿/私密/回收站不计入）。
const TAG_PUBLIC_COUNT: &str = "(SELECT count(*) FROM post_tags pt JOIN posts p ON p.id = pt.post_id \
      WHERE pt.tag_id = t.id AND p.status = 'published' AND p.visibility = 'public' \
        AND p.deleted_at IS NULL)";

#[async_trait]
impl TagRepository for PostgresTagRepository {
    async fn insert(&self, aggregate: &domain::content::Tag) -> Result<(), UseCaseError> {
        let snapshot = aggregate.snapshot();
        sqlx::query(
            "INSERT INTO tags (id, name, slug, version, created_at) \
             VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(snapshot.id)
        .bind(&snapshot.name)
        .bind(&snapshot.slug)
        .bind(snapshot.version)
        .bind(snapshot.created_at)
        .execute(&self.pool)
        .await
        .map_err(map_sqlx_error)?;
        Ok(())
    }

    async fn find_by_slug(
        &self,
        slug: &str,
    ) -> Result<Option<domain::content::TagSnapshot>, UseCaseError> {
        let row = sqlx::query(&format!("SELECT {TAG_COLUMNS} FROM tags WHERE slug = $1"))
            .bind(slug)
            .fetch_optional(&self.pool)
            .await
            .map_err(map_sqlx_error)?;
        row.as_ref().map(tag_from_row).transpose()
    }

    async fn list(&self) -> Result<Vec<TagWithUsage>, UseCaseError> {
        let rows = sqlx::query(&format!(
            "SELECT {TAG_COLUMNS}, {TAG_PUBLIC_COUNT} AS public_post_count \
             FROM tags t ORDER BY t.slug"
        ))
        .fetch_all(&self.pool)
        .await
        .map_err(map_sqlx_error)?;

        rows.iter()
            .map(|row| {
                Ok(TagWithUsage {
                    snapshot: tag_from_row(row)?,
                    public_post_count: row.try_get("public_post_count").map_err(map_row_error)?,
                })
            })
            .collect()
    }

    async fn rename(
        &self,
        id: Uuid,
        new_name: &str,
        expected_version: i64,
    ) -> Result<Option<domain::content::TagSnapshot>, UseCaseError> {
        let row = sqlx::query(
            "UPDATE tags SET name = $3, version = version + 1 \
             WHERE id = $1 AND version = $2 \
             RETURNING id, name, slug, version, created_at",
        )
        .bind(id)
        .bind(expected_version)
        .bind(new_name)
        .fetch_optional(&self.pool)
        .await
        .map_err(map_sqlx_error)?;
        row.as_ref().map(tag_from_row).transpose()
    }

    async fn delete(
        &self,
        id: Uuid,
        expected_version: i64,
    ) -> Result<TagDeleteOutcome, UseCaseError> {
        let mut tx = self.pool.begin().await.map_err(map_sqlx_error)?;
        // 锁住标签行再数引用：并发「把该标签挂到文章」的写入会经
        // post_tags.tag_id 的 FK KEY SHARE 锁与本事务互斥，引用检查因此不被写穿。
        let locked: Option<(i64,)> =
            sqlx::query_as("SELECT version FROM tags WHERE id = $1 FOR UPDATE")
                .bind(id)
                .fetch_optional(&mut *tx)
                .await
                .map_err(map_sqlx_error)?;
        let Some((current_version,)) = locked else {
            tx.commit().await.map_err(map_sqlx_error)?;
            return Ok(TagDeleteOutcome::Gone);
        };
        if current_version != expected_version {
            tx.commit().await.map_err(map_sqlx_error)?;
            return Ok(TagDeleteOutcome::StaleVersion);
        }
        let (count,): (i64,) = sqlx::query_as("SELECT count(*) FROM post_tags WHERE tag_id = $1")
            .bind(id)
            .fetch_one(&mut *tx)
            .await
            .map_err(map_sqlx_error)?;
        if count > 0 {
            tx.commit().await.map_err(map_sqlx_error)?;
            return Ok(TagDeleteOutcome::Referenced { count });
        }
        sqlx::query("DELETE FROM tags WHERE id = $1")
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(map_sqlx_error)?;
        tx.commit().await.map_err(map_sqlx_error)?;
        Ok(TagDeleteOutcome::Deleted)
    }

    async fn existing_ids(&self, ids: &[Uuid]) -> Result<Vec<Uuid>, UseCaseError> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let rows: Vec<(Uuid,)> =
            sqlx::query_as("SELECT id FROM tags WHERE id = ANY($1::uuid[]) ORDER BY id")
                .bind(ids)
                .fetch_all(&self.pool)
                .await
                .map_err(map_sqlx_error)?;
        Ok(rows.into_iter().map(|(id,)| id).collect())
    }

    async fn public_count(&self, id: Uuid) -> Result<i64, UseCaseError> {
        // count(*) 恒有一行（可能为 0）。
        let (count,): (i64,) = sqlx::query_as(
            "SELECT count(*) FROM post_tags pt JOIN posts p ON p.id = pt.post_id \
             WHERE pt.tag_id = $1 AND p.status = 'published' AND p.visibility = 'public' \
               AND p.deleted_at IS NULL",
        )
        .bind(id)
        .fetch_one(&self.pool)
        .await
        .map_err(map_sqlx_error)?;
        Ok(count)
    }
}

// ---------------------------------------------------------------------------
// 公开标签页查询
// ---------------------------------------------------------------------------

pub struct PostgresPublishedTagQuery {
    pool: PgPool,
}

impl PostgresPublishedTagQuery {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl PublishedTagQuery for PostgresPublishedTagQuery {
    async fn list_public_tags(&self, limit: i64) -> Result<Vec<PublicTagSummary>, UseCaseError> {
        let rows: Vec<(String, String)> =
            sqlx::query_as("SELECT slug, name FROM tags ORDER BY slug LIMIT $1")
                .bind(limit.clamp(1, 50))
                .fetch_all(&self.pool)
                .await
                .map_err(map_sqlx_error)?;
        Ok(rows
            .into_iter()
            .map(|(slug, name)| PublicTagSummary { slug, name })
            .collect())
    }
    async fn find_public_by_slug(
        &self,
        slug: &str,
    ) -> Result<Option<PublicTagSummary>, UseCaseError> {
        // 标签目录本身无可见性；未知 slug 与存在与否不区分差异。
        let row: Option<(String, String)> =
            sqlx::query_as("SELECT slug, name FROM tags WHERE slug = $1")
                .bind(slug)
                .fetch_optional(&self.pool)
                .await
                .map_err(map_sqlx_error)?;
        Ok(row.map(|(slug, name)| PublicTagSummary { slug, name }))
    }

    async fn list_public_posts_by_tag(
        &self,
        tag_slug: &str,
        limit: i64,
        offset: i64,
    ) -> Result<(Vec<PublicPostSummary>, i64), UseCaseError> {
        let limit = limit.clamp(1, 100);
        let offset = offset.max(0);
        // count(*) OVER() 让总数与页面来自同一快照：分页导航不会显示
        // 「共 N 篇」却翻出第 N+1 篇（或反之）。
        let rows = sqlx::query(&format!(
            r#"
            SELECT p.title, p.slug, p.excerpt, p.published_at,
                   COALESCE(NULLIF(u.display_name, ''), u.username) AS author_display,
                   u.avatar_media_id AS author_avatar_media_id,
                   count(*) OVER() AS total
            FROM tags t
            JOIN post_tags pt ON pt.tag_id = t.id
            JOIN posts p ON p.id = pt.post_id
            JOIN users u ON u.id = p.author_id
            WHERE t.slug = $1 AND {POST_PUBLIC_PREDICATE}
            ORDER BY p.published_at DESC, p.id DESC
            LIMIT $2 OFFSET $3
            "#
        ))
        .bind(tag_slug)
        .bind(limit)
        .bind(offset)
        .fetch_all(&self.pool)
        .await
        .map_err(map_sqlx_error)?;

        // 空页时窗口函数无行可聚合，总数即 0。
        let total = rows
            .first()
            .map(|row| row.try_get::<i64, _>("total").map_err(map_row_error))
            .transpose()?
            .unwrap_or(0);
        let posts = rows
            .iter()
            .map(|row| {
                Ok(PublicPostSummary {
                    title: row.try_get("title").map_err(map_row_error)?,
                    slug: row.try_get("slug").map_err(map_row_error)?,
                    excerpt: row.try_get("excerpt").map_err(map_row_error)?,
                    published_at: row.try_get("published_at").map_err(map_row_error)?,
                    author_display: row.try_get("author_display").map_err(map_row_error)?,
                    author_avatar_media_id: row
                        .try_get("author_avatar_media_id")
                        .map_err(map_row_error)?,
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok((posts, total))
    }

    async fn list_public_directories(&self) -> Result<Vec<PublicUrlEntry>, UseCaseError> {
        // INNER JOIN 直接实现「非空才收录」：没有公开文章的标签根本不出现在结果里。
        // lastmod 取该标签下公开文章的最近更新时间（tags 表没有 updated_at）。
        let rows = sqlx::query(&format!(
            r#"
            SELECT t.slug, max(p.updated_at) AS updated_at
            FROM tags t
            JOIN post_tags pt ON pt.tag_id = t.id
            JOIN posts p ON p.id = pt.post_id
            WHERE {POST_PUBLIC_PREDICATE}
            GROUP BY t.slug
            ORDER BY t.slug
            "#
        ))
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

// ---------------------------------------------------------------------------
// 分类目录：树锁、防环与引用保护
// ---------------------------------------------------------------------------

/// 分类树事务锁：创建/移动/删除统一取得，串行化祖先链检查与写入。
///
/// 不锁读路径（目录读取无锁）；只约束写写并发——两条并发移动若各自
/// 通过了环检查再先后提交，可能拼出环（检查结果在锁外失效）。
const CATEGORY_TREE_LOCK: (i32, i32) = (2048002, 1);

pub struct PostgresCategoryRepository {
    pool: PgPool,
}

impl PostgresCategoryRepository {
    pub fn new(pool: PgPool) -> Self {
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

/// 公开文章计数子查询（直接归属；与公开分类页同口径）。
const CATEGORY_PUBLIC_COUNT: &str = "(SELECT count(*) FROM posts p WHERE p.category_id = t.id AND p.status = 'published' \
      AND p.visibility = 'public' AND p.deleted_at IS NULL)";

/// 深度受限的祖先链检查：自 parent 向上走，链上出现 self 即成环。
///
/// depth 上限防的是**已损坏数据**（自引用 CHECK 只排除直接自父，历史环会让
/// 无界递归 CTE 永不终止）；正常数据下树锁保证环不会并发产生，链长天然有限。
async fn parent_chain_contains(
    tx: &mut sqlx::PgConnection,
    parent_id: Uuid,
    self_id: Uuid,
) -> Result<bool, UseCaseError> {
    let hit: Option<(i32,)> = sqlx::query_as(
        r#"
        WITH RECURSIVE up(id, parent_id, depth) AS (
            SELECT c.id, c.parent_id, 0 FROM categories c WHERE c.id = $1
            UNION ALL
            -- 向上走祖先链：c 是当前节点的父（up.parent_id = c.id）。
            SELECT c.id, c.parent_id, up.depth + 1
            FROM categories c JOIN up ON up.parent_id = c.id
            WHERE up.depth < 100
        )
        SELECT 1 FROM up WHERE id = $2 LIMIT 1
        "#,
    )
    .bind(parent_id)
    .bind(self_id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(map_sqlx_error)?;
    Ok(hit.is_some())
}

#[async_trait]
impl CategoryRepository for PostgresCategoryRepository {
    async fn insert(&self, aggregate: &domain::content::Category) -> Result<(), UseCaseError> {
        let snapshot = aggregate.snapshot();
        // 新节点不可能是自己的祖先，创建本身无环；树锁仍统一取得，
        // 与并发删除父分类互斥（否则插入成功后父已消失，靠 FK 报裸错误）。
        let mut tx = self.pool.begin().await.map_err(map_sqlx_error)?;
        sqlx::query("SELECT pg_advisory_xact_lock($1::int, $2::int)")
            .bind(CATEGORY_TREE_LOCK.0)
            .bind(CATEGORY_TREE_LOCK.1)
            .execute(&mut *tx)
            .await
            .map_err(map_sqlx_error)?;
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
        rows.iter()
            .map(|row| {
                Ok(CategoryWithUsage {
                    snapshot: category_from_row(row)?,
                    public_post_count: row.try_get("public_post_count").map_err(map_row_error)?,
                })
            })
            .collect()
    }

    async fn update(
        &self,
        id: Uuid,
        name: &str,
        description: Option<&str>,
        parent_id: Option<Uuid>,
        expected_version: i64,
    ) -> Result<Option<domain::content::CategorySnapshot>, UseCaseError> {
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
            {
                if new_parent == id {
                    tx.commit().await.map_err(map_sqlx_error)?;
                    return Err(UseCaseError::Invalid("父分类不能是自身".into()));
                }
                // 新父必须存在（给出可定位错误，而非 FK 裸错误）。
                let exists: Option<(Uuid,)> =
                    sqlx::query_as("SELECT id FROM categories WHERE id = $1")
                        .bind(new_parent)
                        .fetch_optional(&mut *tx)
                        .await
                        .map_err(map_sqlx_error)?;
                if exists.is_none() {
                    tx.commit().await.map_err(map_sqlx_error)?;
                    return Err(UseCaseError::Invalid("父分类不存在".into()));
                }
                if parent_chain_contains(&mut tx, new_parent, id).await? {
                    tx.commit().await.map_err(map_sqlx_error)?;
                    return Err(UseCaseError::Invalid(
                        "目标父分类的祖先链包含自身，会形成环".into(),
                    ));
                }
            }
        }

        let row = sqlx::query(
            "UPDATE categories SET name = $3, description = $4, parent_id = $5, \
             version = version + 1, updated_at = now() \
             WHERE id = $1 AND version = $2 \
             RETURNING id, name, slug, parent_id, description, version, created_at, updated_at",
        )
        .bind(id)
        .bind(expected_version)
        .bind(name)
        .bind(description)
        .bind(parent_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(map_sqlx_error)?;
        tx.commit().await.map_err(map_sqlx_error)?;
        row.as_ref().map(category_from_row).transpose()
    }

    async fn delete(
        &self,
        id: Uuid,
        expected_version: i64,
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
        tx.commit().await.map_err(map_sqlx_error)?;
        Ok(CategoryDeleteOutcome::Deleted)
    }

    async fn existing_id(&self, id: Uuid) -> Result<bool, UseCaseError> {
        let hit: Option<(Uuid,)> = sqlx::query_as("SELECT id FROM categories WHERE id = $1")
            .bind(id)
            .fetch_optional(&self.pool)
            .await
            .map_err(map_sqlx_error)?;
        Ok(hit.is_some())
    }

    async fn public_count(&self, id: Uuid) -> Result<i64, UseCaseError> {
        let (count,): (i64,) = sqlx::query_as(
            "SELECT count(*) FROM posts WHERE category_id = $1 AND status = 'published' \
             AND visibility = 'public' AND deleted_at IS NULL",
        )
        .bind(id)
        .fetch_one(&self.pool)
        .await
        .map_err(map_sqlx_error)?;
        Ok(count)
    }
}

// ---------------------------------------------------------------------------
// 公开分类页查询
// ---------------------------------------------------------------------------

pub struct PostgresPublishedCategoryQuery {
    pool: PgPool,
}

impl PostgresPublishedCategoryQuery {
    pub fn new(pool: PgPool) -> Self {
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
            SELECT p.title, p.slug, p.excerpt, p.published_at,
                   COALESCE(NULLIF(u.display_name, ''), u.username) AS author_display,
                   u.avatar_media_id AS author_avatar_media_id,
                   count(*) OVER() AS total
            FROM categories c
            JOIN posts p ON p.category_id = c.id
            JOIN users u ON u.id = p.author_id
            WHERE c.slug = $1 AND {POST_PUBLIC_PREDICATE}
            ORDER BY p.published_at DESC, p.id DESC
            LIMIT $2 OFFSET $3
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
                Ok(PublicPostSummary {
                    title: row.try_get("title").map_err(map_row_error)?,
                    slug: row.try_get("slug").map_err(map_row_error)?,
                    excerpt: row.try_get("excerpt").map_err(map_row_error)?,
                    published_at: row.try_get("published_at").map_err(map_row_error)?,
                    author_display: row.try_get("author_display").map_err(map_row_error)?,
                    author_avatar_media_id: row
                        .try_get("author_avatar_media_id")
                        .map_err(map_row_error)?,
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok((posts, total))
    }

    async fn list_public_directories(&self) -> Result<Vec<PublicUrlEntry>, UseCaseError> {
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
            "#
        ))
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

// ---------------------------------------------------------------------------
// 系列目录与并发重排
// ---------------------------------------------------------------------------

pub struct PostgresSeriesRepository {
    pool: PgPool,
}

impl PostgresSeriesRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

const SERIES_COLUMNS: &str =
    "id, name, slug, description, cover_media_id, version, created_at, updated_at";

fn series_from_row(
    row: &sqlx::postgres::PgRow,
) -> Result<domain::content::SeriesSnapshot, UseCaseError> {
    Ok(domain::content::SeriesSnapshot {
        id: row.try_get("id").map_err(map_row_error)?,
        name: row.try_get("name").map_err(map_row_error)?,
        slug: row.try_get("slug").map_err(map_row_error)?,
        description: row.try_get("description").map_err(map_row_error)?,
        cover_media_id: row.try_get("cover_media_id").map_err(map_row_error)?,
        version: row.try_get("version").map_err(map_row_error)?,
        created_at: row.try_get("created_at").map_err(map_row_error)?,
        updated_at: row.try_get("updated_at").map_err(map_row_error)?,
    })
}

#[async_trait]
impl SeriesRepository for PostgresSeriesRepository {
    async fn insert(&self, aggregate: &domain::content::Series) -> Result<(), UseCaseError> {
        let snapshot = aggregate.snapshot();
        let mut tx = self.pool.begin().await.map_err(map_sqlx_error)?;
        sqlx::query(
            "INSERT INTO series (id, name, slug, description, cover_media_id, version, created_at, \
             updated_at) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        )
        .bind(snapshot.id)
        .bind(&snapshot.name)
        .bind(&snapshot.slug)
        .bind(&snapshot.description)
        .bind(snapshot.cover_media_id)
        .bind(snapshot.version)
        .bind(snapshot.created_at)
        .bind(snapshot.updated_at)
        .execute(&mut *tx)
        .await
        .map_err(map_sqlx_error)?;
        sync_media_refs(
            &mut tx,
            MediaContentKind::Series,
            snapshot.id,
            &media_ids_for(&[], snapshot.cover_media_id),
        )
        .await?;
        tx.commit().await.map_err(map_sqlx_error)
    }

    async fn find_by_slug(
        &self,
        slug: &str,
    ) -> Result<Option<domain::content::SeriesSnapshot>, UseCaseError> {
        let row = sqlx::query(&format!(
            "SELECT {SERIES_COLUMNS} FROM series WHERE slug = $1"
        ))
        .bind(slug)
        .fetch_optional(&self.pool)
        .await
        .map_err(map_sqlx_error)?;
        row.as_ref().map(series_from_row).transpose()
    }

    async fn list(&self) -> Result<Vec<SeriesWithUsage>, UseCaseError> {
        let rows = sqlx::query(
            "SELECT s.id, s.name, s.slug, s.description, s.cover_media_id, s.version, s.created_at, \
                    s.updated_at, \
                    (SELECT count(*) FROM posts p WHERE p.series_id = s.id) AS post_count, \
                    (SELECT count(*) FROM posts p WHERE p.series_id = s.id \
                       AND p.status = 'published' AND p.visibility = 'public' \
                       AND p.deleted_at IS NULL) AS public_post_count \
             FROM series s ORDER BY s.slug",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(map_sqlx_error)?;
        rows.iter()
            .map(|row| {
                Ok(SeriesWithUsage {
                    snapshot: series_from_row(row)?,
                    post_count: row.try_get("post_count").map_err(map_row_error)?,
                    public_post_count: row.try_get("public_post_count").map_err(map_row_error)?,
                })
            })
            .collect()
    }

    async fn update(
        &self,
        id: Uuid,
        name: &str,
        description: Option<&str>,
        cover_media_id: Option<Uuid>,
        expected_version: i64,
    ) -> Result<Option<domain::content::SeriesSnapshot>, UseCaseError> {
        // name/描述/封面与引用行在同一事务：封面替换时旧图必须同时被释放，
        // 否则会出现「列里已换新图、引用表还占着旧图」的幽灵占用。
        let mut tx = self.pool.begin().await.map_err(map_sqlx_error)?;
        let row = sqlx::query(
            "UPDATE series SET name = $3, description = $4, cover_media_id = $5, \
             version = version + 1, updated_at = now() WHERE id = $1 AND version = $2 \
             RETURNING id, name, slug, description, cover_media_id, version, created_at, updated_at",
        )
        .bind(id)
        .bind(expected_version)
        .bind(name)
        .bind(description)
        .bind(cover_media_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(map_sqlx_error)?;
        let Some(row) = row else {
            tx.commit().await.map_err(map_sqlx_error)?;
            return Ok(None);
        };
        let snapshot = series_from_row(&row)?;
        // 系列封面与引用同事务固化，保留站内使用统计与物理清理保护。
        sync_media_refs(
            &mut tx,
            MediaContentKind::Series,
            snapshot.id,
            &media_ids_for(&[], snapshot.cover_media_id),
        )
        .await?;
        tx.commit().await.map_err(map_sqlx_error)?;
        Ok(Some(snapshot))
    }

    async fn delete(
        &self,
        id: Uuid,
        expected_version: i64,
    ) -> Result<SeriesDeleteOutcome, UseCaseError> {
        let mut tx = self.pool.begin().await.map_err(map_sqlx_error)?;
        // 锁系列行：与「文章加入该系列」的写入（posts 行上的系列引用）互斥，
        // 引用检查与删除之间不会有并发加入。
        let locked: Option<(i64,)> =
            sqlx::query_as("SELECT version FROM series WHERE id = $1 FOR UPDATE")
                .bind(id)
                .fetch_optional(&mut *tx)
                .await
                .map_err(map_sqlx_error)?;
        let Some((current_version,)) = locked else {
            tx.commit().await.map_err(map_sqlx_error)?;
            return Ok(SeriesDeleteOutcome::Gone);
        };
        if current_version != expected_version {
            tx.commit().await.map_err(map_sqlx_error)?;
            return Ok(SeriesDeleteOutcome::StaleVersion);
        }
        let (count,): (i64,) = sqlx::query_as("SELECT count(*) FROM posts WHERE series_id = $1")
            .bind(id)
            .fetch_one(&mut *tx)
            .await
            .map_err(map_sqlx_error)?;
        if count > 0 {
            tx.commit().await.map_err(map_sqlx_error)?;
            return Ok(SeriesDeleteOutcome::Referenced { count });
        }
        // 内容物理删除：其媒体引用必须在同一事务清理（content_id 是多态引用，
        // 没有外键级联兜底）。系列只有封面一种引用来源。
        clear_media_refs(&mut tx, MediaContentKind::Series, id).await?;
        sqlx::query("DELETE FROM series WHERE id = $1")
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(map_sqlx_error)?;
        tx.commit().await.map_err(map_sqlx_error)?;
        Ok(SeriesDeleteOutcome::Deleted)
    }

    async fn existing_id(&self, id: Uuid) -> Result<bool, UseCaseError> {
        let hit: Option<(Uuid,)> = sqlx::query_as("SELECT id FROM series WHERE id = $1")
            .bind(id)
            .fetch_optional(&self.pool)
            .await
            .map_err(map_sqlx_error)?;
        Ok(hit.is_some())
    }

    async fn members_of(&self, series_id: Uuid) -> Result<Vec<SeriesMember>, UseCaseError> {
        let rows = sqlx::query(
            "SELECT id, author_id, slug, title, status, visibility, series_order, deleted_at \
             FROM posts WHERE series_id = $1 \
             ORDER BY series_order, id",
        )
        .bind(series_id)
        .fetch_all(&self.pool)
        .await
        .map_err(map_sqlx_error)?;
        rows.iter()
            .map(|row| {
                Ok(SeriesMember {
                    post_id: row.try_get("id").map_err(map_row_error)?,
                    author_id: row.try_get("author_id").map_err(map_row_error)?,
                    slug: row.try_get("slug").map_err(map_row_error)?,
                    title: row.try_get("title").map_err(map_row_error)?,
                    status: row.try_get::<String, _>("status").map_err(map_row_error)?,
                    deleted: row
                        .try_get::<Option<OffsetDateTime>, _>("deleted_at")
                        .map_err(map_row_error)?
                        .is_some(),
                    visibility: row
                        .try_get::<String, _>("visibility")
                        .map_err(map_row_error)?,
                    series_order: row.try_get("series_order").map_err(map_row_error)?,
                })
            })
            .collect()
    }

    /// 重排锁协议（docs/database-design.md §4）：
    /// 1. 锁系列行并校验 series.version（旧目录不得重排）；
    /// 2. 校验成员集合与提交的完整排列一致；
    /// 3. 按固定 id 序锁涉及文章（跨系列移动时两个系列都按 id 序）；
    /// 4. `SET CONSTRAINTS posts_series_position_unique DEFERRED`，
    ///    交换期间允许临时重复，提交时恢复唯一检查；
    /// 5. 更新每篇文章 series_order 并递增 posts.version；递增 series.version。
    async fn reorder(
        &self,
        series_id: Uuid,
        expected_series_version: i64,
        ordered_post_ids: &[Uuid],
    ) -> Result<ReorderOutcome, UseCaseError> {
        let mut tx = self.pool.begin().await.map_err(map_sqlx_error)?;
        let locked: Option<(i64,)> =
            sqlx::query_as("SELECT version FROM series WHERE id = $1 FOR UPDATE")
                .bind(series_id)
                .fetch_optional(&mut *tx)
                .await
                .map_err(map_sqlx_error)?;
        let Some((current_version,)) = locked else {
            tx.commit().await.map_err(map_sqlx_error)?;
            return Ok(ReorderOutcome::SeriesGone);
        };
        if current_version != expected_series_version {
            tx.commit().await.map_err(map_sqlx_error)?;
            return Ok(ReorderOutcome::StaleSeriesVersion);
        }

        // 固定 id 序锁定成员行，并读取当前成员集合。
        let members: Vec<(Uuid,)> =
            sqlx::query_as("SELECT id FROM posts WHERE series_id = $1 ORDER BY id")
                .bind(series_id)
                .fetch_all(&mut *tx)
                .await
                .map_err(map_sqlx_error)?;
        let mut current: Vec<Uuid> = members.into_iter().map(|(id,)| id).collect();
        let mut given: Vec<Uuid> = ordered_post_ids.to_vec();
        current.sort_unstable();
        given.sort_unstable();
        if current != given {
            tx.commit().await.map_err(map_sqlx_error)?;
            return Ok(ReorderOutcome::MembershipMismatch);
        }

        // 锁成员行（id 序）。VALUES 列表带序号参数在 sqlx 里用 unnest 更稳。
        let locked_posts: Vec<(Uuid,)> = sqlx::query_as(
            "SELECT id FROM posts WHERE id = ANY($1::uuid[]) ORDER BY id \
             FOR UPDATE",
        )
        .bind(&given)
        .fetch_all(&mut *tx)
        .await
        .map_err(map_sqlx_error)?;
        if locked_posts.len() != given.len() {
            tx.commit().await.map_err(map_sqlx_error)?;
            return Ok(ReorderOutcome::MembershipMismatch);
        }

        // 延后位置唯一约束：交换期间允许临时重复。
        sqlx::query("SET CONSTRAINTS posts_series_position_unique DEFERRED")
            .execute(&mut *tx)
            .await
            .map_err(map_sqlx_error)?;

        // 按提交顺序写入序号（1 起）；每篇文章 version+1（顺序是文章内容的一部分）。
        for (index, post_id) in ordered_post_ids.iter().enumerate() {
            let order: i32 = (index + 1) as i32;
            sqlx::query(
                "UPDATE posts SET series_order = $2, version = version + 1, updated_at = now() \
                 WHERE id = $1 AND series_id = $3",
            )
            .bind(post_id)
            .bind(order)
            .bind(series_id)
            .execute(&mut *tx)
            .await
            .map_err(map_sqlx_error)?;
        }

        let bumped: Option<(i64,)> = sqlx::query_as(
            "UPDATE series SET version = version + 1, updated_at = now() \
             WHERE id = $1 RETURNING version",
        )
        .bind(series_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(map_sqlx_error)?;
        tx.commit().await.map_err(map_sqlx_error)?;
        Ok(ReorderOutcome::Reordered {
            new_version: bumped.map(|(v,)| v).unwrap_or(current_version + 1),
        })
    }
}

// ---------------------------------------------------------------------------
// 公开系列页查询
// ---------------------------------------------------------------------------

pub struct PostgresPublishedSeriesQuery {
    pool: PgPool,
}

impl PostgresPublishedSeriesQuery {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl PublishedSeriesQuery for PostgresPublishedSeriesQuery {
    async fn find_public_by_slug(
        &self,
        slug: &str,
    ) -> Result<Option<PublicSeriesSummary>, UseCaseError> {
        let row: Option<(String, String, Option<Uuid>)> =
            sqlx::query_as("SELECT slug, name, cover_media_id FROM series WHERE slug = $1")
                .bind(slug)
                .fetch_optional(&self.pool)
                .await
                .map_err(map_sqlx_error)?;
        Ok(row.map(|(slug, name, cover_media_id)| PublicSeriesSummary {
            slug,
            name,
            cover_media_id,
        }))
    }

    async fn list_public_posts_by_series(
        &self,
        series_slug: &str,
        limit: i64,
        offset: i64,
    ) -> Result<(Vec<PublicPostSummary>, i64), UseCaseError> {
        let limit = limit.clamp(1, 100);
        let offset = offset.max(0);
        // 阅读顺序：series_order 升序；草稿/私密/回收站保留位置但不出现，
        // 因此公开页的序号可能留空档（不强制重新编号）。
        let rows = sqlx::query(&format!(
            r#"
            SELECT p.title, p.slug, p.excerpt, p.published_at,
                   COALESCE(NULLIF(u.display_name, ''), u.username) AS author_display,
                   u.avatar_media_id AS author_avatar_media_id,
                   count(*) OVER() AS total
            FROM series s
            JOIN posts p ON p.series_id = s.id
            JOIN users u ON u.id = p.author_id
            WHERE s.slug = $1 AND {POST_PUBLIC_PREDICATE}
            ORDER BY p.series_order ASC, p.id ASC
            LIMIT $2 OFFSET $3
            "#
        ))
        .bind(series_slug)
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
                Ok(PublicPostSummary {
                    title: row.try_get("title").map_err(map_row_error)?,
                    slug: row.try_get("slug").map_err(map_row_error)?,
                    excerpt: row.try_get("excerpt").map_err(map_row_error)?,
                    published_at: row.try_get("published_at").map_err(map_row_error)?,
                    author_display: row.try_get("author_display").map_err(map_row_error)?,
                    author_avatar_media_id: row
                        .try_get("author_avatar_media_id")
                        .map_err(map_row_error)?,
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok((posts, total))
    }

    async fn list_public_directories(&self) -> Result<Vec<PublicUrlEntry>, UseCaseError> {
        // 与分类页同口径：只收录有公开文章的系列；lastmod 取系列改名与成员更新的较晚者。
        let rows = sqlx::query(&format!(
            r#"
            SELECT s.slug, GREATEST(s.updated_at, max(p.updated_at)) AS updated_at
            FROM series s
            JOIN posts p ON p.series_id = s.id
            WHERE {POST_PUBLIC_PREDICATE}
            GROUP BY s.id, s.slug, s.updated_at
            ORDER BY s.slug
            "#
        ))
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
