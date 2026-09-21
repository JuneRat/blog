//! 文章用例：创建、编辑、发布、撤回。
//!
//! M1 权限约定：作者可编辑/发布/撤回自己的文章（own 语义的雏形），
//! RBAC 表与 any 权限随 M2 接入后替换这里的归属检查。
//! 所有写入携带 expected_version，冲突不自动覆盖。

use std::sync::Arc;

use uuid::Uuid;

use crate::error::UseCaseError;
use crate::identity::Actor;
use crate::ports::{Clock, PostRepository};
use domain::content::post::{
    Post, PostPatch, PostSnapshot, Slug, Visibility,
};
use domain::identity::UserId;

/// 向接口层转出的值对象（interfaces 不直接依赖 domain crate）。
pub use domain::content::post::Visibility as PostVisibility;

#[derive(Debug, Clone)]
pub struct CreatePostCmd {
    /// None 时由应用生成临时唯一 slug（草稿创建即占用 slug）。
    pub slug: Option<String>,
    pub title: String,
    pub excerpt: Option<String>,
    pub content: String,
    pub visibility: Visibility,
}

#[derive(Debug, Clone, Default)]
pub struct EditPostCmd {
    pub target_slug: String,
    pub new_slug: Option<String>,
    pub title: Option<String>,
    pub excerpt: Option<String>,
    pub content: Option<String>,
    pub visibility: Option<Visibility>,
    /// None 表示使用读取到的当前版本（仍可检测读后并发修改）。
    pub expected_version: Option<i64>,
}

/// 面向 CLI/后台的文章视图，包含非公开状态。
#[derive(Debug, Clone, serde::Serialize)]
pub struct PostDto {
    pub id: Uuid,
    pub slug: String,
    pub title: String,
    pub status: &'static str,
    pub visibility: &'static str,
    pub version: i64,
    pub published_at: Option<time::OffsetDateTime>,
    pub updated_at: time::OffsetDateTime,
    pub deleted: bool,
    pub author_id: Uuid,
}

impl PostDto {
    fn from_snapshot(s: &PostSnapshot) -> Self {
        Self {
            id: s.id,
            slug: s.slug.clone(),
            title: s.title.clone(),
            status: s.status.as_str(),
            visibility: s.visibility.as_str(),
            version: s.version,
            published_at: s.published_at,
            updated_at: s.updated_at,
            deleted: s.deleted_at.is_some(),
            author_id: s.author_id,
        }
    }
}

pub struct PostInteractor {
    posts: Arc<dyn PostRepository>,
    clock: Arc<dyn Clock>,
}

impl PostInteractor {
    pub fn new(posts: Arc<dyn PostRepository>, clock: Arc<dyn Clock>) -> Self {
        Self { posts, clock }
    }

    /// 创建草稿。作者来自受信 Actor。
    pub async fn create(&self, actor: &Actor, cmd: CreatePostCmd) -> Result<PostDto, UseCaseError> {
        let slug_raw = cmd.slug.unwrap_or_else(|| format!("draft-{}", Uuid::now_v7().simple()));
        let slug = Slug::new(&slug_raw).map_err(|e| UseCaseError::Invalid(e.to_string()))?;
        let post = Post::create_draft(
            actor.user_id,
            slug,
            cmd.title,
            cmd.excerpt,
            cmd.content,
            cmd.visibility,
            self.clock.now(),
        )
        .map_err(|e| UseCaseError::Invalid(e.to_string()))?;
        let snapshot = post.snapshot();
        self.posts.insert(&snapshot).await?;
        Ok(PostDto::from_snapshot(&snapshot))
    }

    /// 编辑当前正文；保存已发布内容直接更新线上。
    pub async fn edit(&self, actor: &Actor, cmd: EditPostCmd) -> Result<PostDto, UseCaseError> {
        let (mut post, expected) = self.load_for_actor(&cmd.target_slug, actor).await?;
        let expected = cmd.expected_version.unwrap_or(expected);

        let changed = post
            .edit(PostPatch {
                slug: cmd.new_slug,
                title: cmd.title,
                excerpt: cmd.excerpt,
                content: cmd.content,
                visibility: cmd.visibility,
            })
            .map_err(|e| UseCaseError::Invalid(e.to_string()))?;

        if changed {
            let now = self.clock.now();
            let mut snapshot = post.snapshot();
            if !self.posts.save(&snapshot, expected, now).await? {
                return Err(UseCaseError::VersionConflict);
            }
            // 回读真实 version，避免调用方拿到过期计数。
            snapshot = self
                .posts
                .find_by_id(snapshot.id)
                .await?
                .ok_or_else(|| UseCaseError::NotFound("文章".into()))?;
            return Ok(PostDto::from_snapshot(&snapshot));
        }
        Ok(PostDto::from_snapshot(&post.snapshot()))
    }

    /// 发布：draft → published，首次发布写入 published_at；已发布幂等。
    pub async fn publish(
        &self,
        actor: &Actor,
        slug: &str,
        expected_version: Option<i64>,
    ) -> Result<PostDto, UseCaseError> {
        let (mut post, expected) = self.load_for_actor(slug, actor).await?;
        let expected = expected_version.unwrap_or(expected);

        if post.publish(self.clock.now()).map_err(map_domain)? {
            let mut snapshot = post.snapshot();
            if !self.posts.save(&snapshot, expected, self.clock.now()).await? {
                return Err(UseCaseError::VersionConflict);
            }
            snapshot = self
                .posts
                .find_by_id(snapshot.id)
                .await?
                .ok_or_else(|| UseCaseError::NotFound("文章".into()))?;
            return Ok(PostDto::from_snapshot(&snapshot));
        }
        Ok(PostDto::from_snapshot(&post.snapshot()))
    }

    /// 撤回：published → draft，slug 保持锁定；非发布状态幂等。
    pub async fn withdraw(
        &self,
        actor: &Actor,
        slug: &str,
        expected_version: Option<i64>,
    ) -> Result<PostDto, UseCaseError> {
        let (mut post, expected) = self.load_for_actor(slug, actor).await?;
        let expected = expected_version.unwrap_or(expected);

        if post.withdraw() {
            let mut snapshot = post.snapshot();
            if !self.posts.save(&snapshot, expected, self.clock.now()).await? {
                return Err(UseCaseError::VersionConflict);
            }
            snapshot = self
                .posts
                .find_by_id(snapshot.id)
                .await?
                .ok_or_else(|| UseCaseError::NotFound("文章".into()))?;
            return Ok(PostDto::from_snapshot(&snapshot));
        }
        Ok(PostDto::from_snapshot(&post.snapshot()))
    }

    /// CLI/后台读取（任意状态）；写通道专属，不进入匿名 HTTP。
    pub async fn find(&self, slug: &str) -> Result<PostDto, UseCaseError> {
        let snapshot = self
            .posts
            .find_by_slug(slug)
            .await?
            .ok_or_else(|| UseCaseError::NotFound(format!("文章 {slug}")))?;
        Ok(PostDto::from_snapshot(&snapshot))
    }

    pub async fn list_by_author(&self, author: UserId) -> Result<Vec<PostDto>, UseCaseError> {
        let snapshots = self.posts.list_by_author(author.0).await?;
        Ok(snapshots.iter().map(PostDto::from_snapshot).collect())
    }

    /// 加载聚合并执行 M1 归属检查（own）。
    async fn load_for_actor(
        &self,
        slug: &str,
        actor: &Actor,
    ) -> Result<(Post, i64), UseCaseError> {
        let snapshot = self
            .posts
            .find_by_slug(slug)
            .await?
            .ok_or_else(|| UseCaseError::NotFound(format!("文章 {slug}")))?;
        if snapshot.deleted_at.is_some() {
            return Err(UseCaseError::NotFound(format!("文章 {slug}")));
        }
        let post = Post::reconstitute(snapshot);
        if post.author_id() != actor.user_id {
            return Err(UseCaseError::Forbidden);
        }
        let version = post.version();
        Ok((post, version))
    }
}

fn map_domain(e: domain::content::post::PostError) -> UseCaseError {
    UseCaseError::Invalid(e.to_string())
}

#[cfg(test)]
mod tests {
    //! 用例测试见 crates/application/tests/，配合内存 fake 仓储。
}
