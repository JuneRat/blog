//! 标签用例：创建、目录读取、改名、删除。
//!
//! 权限约定（docs/identity-and-admin.md §2）：
//! - 目录管理动作（创建/改名/删除）要求 `tag.manage`（Owner 与 Editor 内置持有）；
//! - 目录**读取**不设权限：标签本身是公开数据（公开标签页对匿名可见），
//!   Author 选择标签编辑自己的文章需要读目录，但不授予管理权；
//! - 文章与标签的**关联**走文章编辑授权（post.update/post.update_any），
//!   不在本用例——跨文章修改仍核验文章授权。
//!
//! 并发约定：改名/删除都携带 expected_version，条件更新不自动覆盖；
//! slug 创建后不可修改；删除在引用保护下拒绝（含草稿/私密/回收站引用）。

use std::sync::Arc;

use uuid::Uuid;

use crate::error::UseCaseError;
use crate::identity::Actor;
use crate::ports::{Clock, TagDeleteOutcome, TagRepository, TagWithUsage};
use crate::version::checked_version;
use domain::content::Slug;
use domain::content::tag::{Tag, TagError};

pub struct CreateTagCmd {
    pub name: String,
    /// 标签目录地址 /tags/{slug} 的路径段；创建后不可修改。
    pub slug: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct TagDto {
    pub id: Uuid,
    pub name: String,
    pub slug: String,
    pub version: i64,
    /// 公开文章计数；与公开标签页同口径（不含草稿/私密/回收站）。
    pub public_post_count: i64,
}

impl TagDto {
    fn from_usage(row: &TagWithUsage) -> Self {
        Self {
            id: row.snapshot.id,
            name: row.snapshot.name.clone(),
            slug: row.snapshot.slug.clone(),
            version: row.snapshot.version,
            public_post_count: row.public_post_count,
        }
    }
}

pub struct TagInteractor {
    tags: Arc<dyn TagRepository>,
    clock: Arc<dyn Clock>,
}

impl TagInteractor {
    pub fn new(tags: Arc<dyn TagRepository>, clock: Arc<dyn Clock>) -> Self {
        Self { tags, clock }
    }

    /// 创建标签。slug 冲突由唯一约束兜底并翻译为 Conflict(Slug)。
    pub async fn create(&self, actor: &Actor, cmd: CreateTagCmd) -> Result<TagDto, UseCaseError> {
        actor.ensure_write_channel()?;
        if !actor.has_permission("tag.manage") {
            return Err(UseCaseError::Forbidden);
        }
        let slug = Slug::new(&cmd.slug).map_err(|e| UseCaseError::Invalid(e.to_string()))?;
        let tag = Tag::new(cmd.name, slug, self.clock.now()).map_err(map_domain)?;
        let snapshot = tag.snapshot();
        self.tags.insert(&tag).await?;
        Ok(TagDto {
            id: snapshot.id,
            name: snapshot.name,
            slug: snapshot.slug,
            version: snapshot.version,
            public_post_count: 0,
        })
    }

    /// 全量目录（管理屏与编辑器选择器共用；按 slug 排序）。
    /// 已认证即可读——不检查 tag.manage，见模块说明。
    pub async fn list(&self, _actor: &Actor) -> Result<Vec<TagDto>, UseCaseError> {
        Ok(self
            .tags
            .list()
            .await?
            .iter()
            .map(TagDto::from_usage)
            .collect())
    }

    /// 改名：影响全部引用文章（名称读取当前值）；slug 不变。
    pub async fn rename(
        &self,
        actor: &Actor,
        target_slug: &str,
        new_name: String,
        expected_version: Option<i64>,
    ) -> Result<TagDto, UseCaseError> {
        actor.ensure_write_channel()?;
        if !actor.has_permission("tag.manage") {
            return Err(UseCaseError::Forbidden);
        }
        let tag = self.load(target_slug).await?;
        let expected = checked_version(tag.version(), expected_version)?;

        let mut tag = tag;
        if !tag.rename(new_name).map_err(map_domain)? {
            // 幂等改名：版本前提已校验，聚合即当前状态，只补公开计数。
            let count = self.tags.public_count(tag.id()).await?;
            let s = tag.snapshot();
            return Ok(TagDto {
                id: s.id,
                name: s.name,
                slug: s.slug,
                version: s.version,
                public_post_count: count,
            });
        }
        let snapshot = tag.snapshot();
        match self
            .tags
            .rename(snapshot.id, &snapshot.name, expected)
            .await?
        {
            Some(updated) => {
                let count = self.tags.public_count(updated.id).await?;
                Ok(TagDto {
                    id: updated.id,
                    name: updated.name,
                    slug: updated.slug,
                    version: updated.version,
                    public_post_count: count,
                })
            }
            None => Err(UseCaseError::VersionConflict),
        }
    }

    /// 删除：仍被任何文章引用（含草稿/私密/回收站）时拒绝；
    /// 引用检查与删除在同一事务（见仓储实现）。
    pub async fn delete(
        &self,
        actor: &Actor,
        target_slug: &str,
        expected_version: Option<i64>,
    ) -> Result<(), UseCaseError> {
        actor.ensure_write_channel()?;
        if !actor.has_permission("tag.manage") {
            return Err(UseCaseError::Forbidden);
        }
        let tag = self.load(target_slug).await?;
        let expected = checked_version(tag.version(), expected_version)?;

        match self.tags.delete(tag.id(), expected).await? {
            TagDeleteOutcome::Deleted => Ok(()),
            TagDeleteOutcome::StaleVersion => Err(UseCaseError::VersionConflict),
            TagDeleteOutcome::Referenced { count } => Err(UseCaseError::TagInUse(count)),
            TagDeleteOutcome::Gone => Err(UseCaseError::NotFound(format!("标签 {target_slug}"))),
        }
    }

    async fn load(&self, slug: &str) -> Result<Tag, UseCaseError> {
        let snapshot = self
            .tags
            .find_by_slug(slug)
            .await?
            .ok_or_else(|| UseCaseError::NotFound(format!("标签 {slug}")))?;
        Tag::reconstitute(snapshot).map_err(|e| UseCaseError::Repository(e.to_string()))
    }
}

fn map_domain(e: TagError) -> UseCaseError {
    UseCaseError::Invalid(e.to_string())
}
