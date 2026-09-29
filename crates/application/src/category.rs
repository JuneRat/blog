//! 分类用例：创建、更新（改名/描述/移动父节点）、删除与目录读取。
//!
//! 权限约定（docs/identity-and-admin.md §2）：
//! - 目录管理动作要求 `category.manage`（Admin 与 Editor 内置持有）；
//! - 目录读取对已认证会话开放（文章编辑器选择分类需要）；
//! - 文章与分类的**关联**走文章编辑授权（post.update/post.update_any）。
//!
//! 防环与并发：移动父节点的祖先链校验在仓储的分类树事务锁内完成
//! （应用层不预检——预检结果在锁外可被并发移动作废）；删除同样在树锁内
//! 检查文章引用与子分类。slug 创建后不可修改。

use std::sync::Arc;

use uuid::Uuid;

use crate::error::UseCaseError;
use crate::identity::Actor;
use crate::ports::{CategoryDeleteOutcome, CategoryRepository, CategoryWithUsage, Clock};
use crate::version::checked_version;
use domain::content::Slug;
use domain::content::category::Category;

pub struct CreateCategoryCmd {
    pub name: String,
    pub slug: String,
    /// 父分类 slug；None 表示根分类。
    pub parent: Option<String>,
    pub description: Option<String>,
}

/// 更新命令：`parent` 三态——`None` 保持现状，`Some(None)` 移到根，`Some(Some(slug))` 移到指定父。
#[derive(Debug, Clone, Default)]
pub struct UpdateCategoryCmd {
    pub name: String,
    pub description: Option<String>,
    pub parent: Option<Option<String>>,
    pub expected_version: Option<i64>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct CategoryDto {
    pub id: Uuid,
    pub name: String,
    pub slug: String,
    pub parent_id: Option<Uuid>,
    pub description: Option<String>,
    pub version: i64,
    /// 直接归属的公开文章计数（子树聚合需显式查询，首版不提供）。
    pub public_post_count: i64,
}

impl CategoryDto {
    fn from_usage(row: &CategoryWithUsage) -> Self {
        Self {
            id: row.snapshot.id,
            name: row.snapshot.name.clone(),
            slug: row.snapshot.slug.clone(),
            parent_id: row.snapshot.parent_id,
            description: row.snapshot.description.clone(),
            version: row.snapshot.version,
            public_post_count: row.public_post_count,
        }
    }
}

pub struct CategoryInteractor {
    categories: Arc<dyn CategoryRepository>,
    clock: Arc<dyn Clock>,
}

impl CategoryInteractor {
    pub fn new(categories: Arc<dyn CategoryRepository>, clock: Arc<dyn Clock>) -> Self {
        Self { categories, clock }
    }

    /// 创建分类。新节点不可能是自己的祖先，创建本身无环风险；
    /// 父分类按 slug 解析（不存在即 NotFound）。
    pub async fn create(
        &self,
        actor: &Actor,
        cmd: CreateCategoryCmd,
    ) -> Result<CategoryDto, UseCaseError> {
        actor.ensure_write_channel()?;
        if !actor.has_permission("category.manage") {
            return Err(UseCaseError::Forbidden);
        }
        let slug = Slug::new(&cmd.slug).map_err(|e| UseCaseError::Invalid(e.to_string()))?;
        let parent_id = match cmd.parent.as_deref() {
            Some(parent_slug) if !parent_slug.is_empty() => {
                Some(self.resolve_parent(parent_slug).await?.id())
            }
            _ => None,
        };
        let category = Category::new(cmd.name, slug, parent_id, cmd.description, self.clock.now())
            .map_err(map_domain)?;
        let snapshot = category.snapshot();
        self.categories
            .insert(&category, actor.audit_context())
            .await?;
        Ok(CategoryDto {
            id: snapshot.id,
            name: snapshot.name,
            slug: snapshot.slug,
            parent_id: snapshot.parent_id,
            description: snapshot.description,
            version: snapshot.version,
            public_post_count: 0,
        })
    }

    /// 更新：改名/描述/移动父节点。移动的防环校验在仓储树锁内执行；
    /// 未变化的幂等更新同样校验版本前提（docs/content-lifecycle.md §1）。
    pub async fn update(
        &self,
        actor: &Actor,
        target_slug: &str,
        cmd: UpdateCategoryCmd,
    ) -> Result<CategoryDto, UseCaseError> {
        actor.ensure_write_channel()?;
        if !actor.has_permission("category.manage") {
            return Err(UseCaseError::Forbidden);
        }
        let category = self.load(target_slug).await?;
        let expected = checked_version(category.version(), cmd.expected_version)?;

        // 解析目标父节点（三态：保持 / 移到根 / 移到指定父）。
        let parent_id = match cmd.parent {
            None => category.parent_id(),
            Some(None) => None,
            Some(Some(ref parent_slug)) => Some(self.resolve_parent(parent_slug).await?.id()),
        };

        // 幂等：字段与父节点都无变化时只校验版本前提，不写不递增。
        let mut category = category;
        let fields_changed = category
            .update(cmd.name, cmd.description)
            .map_err(map_domain)?;
        let parent_changed = parent_id != category.parent_id();
        if !fields_changed && !parent_changed {
            return self.dto_of(category.id()).await;
        }

        let snapshot = category.snapshot();
        match self
            .categories
            .update(
                snapshot.id,
                &snapshot.name,
                snapshot.description.as_deref(),
                parent_id,
                expected,
                actor.audit_context(),
            )
            .await?
        {
            Some(updated) => {
                let count = self.categories.public_count(updated.id).await?;
                Ok(CategoryDto {
                    id: updated.id,
                    name: updated.name,
                    slug: updated.slug,
                    parent_id: updated.parent_id,
                    description: updated.description,
                    version: updated.version,
                    public_post_count: count,
                })
            }
            None => Err(UseCaseError::VersionConflict),
        }
    }

    /// 删除：被文章引用（含草稿/私密/回收站）或仍有子分类时拒绝。
    pub async fn delete(
        &self,
        actor: &Actor,
        target_slug: &str,
        expected_version: Option<i64>,
    ) -> Result<(), UseCaseError> {
        actor.ensure_write_channel()?;
        if !actor.has_permission("category.manage") {
            return Err(UseCaseError::Forbidden);
        }
        let category = self.load(target_slug).await?;
        let expected = checked_version(category.version(), expected_version)?;

        match self
            .categories
            .delete(category.id(), expected, actor.audit_context())
            .await?
        {
            CategoryDeleteOutcome::Deleted => Ok(()),
            CategoryDeleteOutcome::StaleVersion => Err(UseCaseError::VersionConflict),
            CategoryDeleteOutcome::Referenced { posts, children } => {
                Err(UseCaseError::CategoryInUse { posts, children })
            }
            CategoryDeleteOutcome::Gone => {
                Err(UseCaseError::NotFound(format!("分类 {target_slug}")))
            }
        }
    }

    /// 全量目录（管理屏与编辑器选择器共用；按 slug 排序，前端自组树形）。
    pub async fn list(&self, _actor: &Actor) -> Result<Vec<CategoryDto>, UseCaseError> {
        Ok(self
            .categories
            .list()
            .await?
            .iter()
            .map(CategoryDto::from_usage)
            .collect())
    }

    async fn load(&self, slug: &str) -> Result<Category, UseCaseError> {
        let snapshot = self
            .categories
            .find_by_slug(slug)
            .await?
            .ok_or_else(|| UseCaseError::NotFound(format!("分类 {slug}")))?;
        Category::reconstitute(snapshot).map_err(|e| UseCaseError::DataCorrupt(e.to_string()))
    }

    async fn resolve_parent(&self, slug: &str) -> Result<Category, UseCaseError> {
        // 父分类与目标相同：聚合层不允许自父（数据库 CHECK 兜底）。
        self.load(slug).await
    }

    async fn dto_of(&self, id: Uuid) -> Result<CategoryDto, UseCaseError> {
        let row = self
            .categories
            .list()
            .await?
            .into_iter()
            .find(|row| row.snapshot.id == id)
            .ok_or_else(|| UseCaseError::NotFound("分类".into()))?;
        Ok(CategoryDto::from_usage(&row))
    }
}

fn map_domain(e: domain::content::category::CategoryError) -> UseCaseError {
    UseCaseError::Invalid(e.to_string())
}
