//! 文章用例：创建、编辑、发布、撤回。
//!
//! 权限约定（M2 RBAC 已接入）：
//! - 写通道仅限受控 CLI（`Actor::ensure_write_channel`），公开 HTTP 无写路由；
//! - 各动作按 own/any 权限对检查（post.update/post.update_any 等），
//!   any 覆盖 own；角色名称不替代动作检查。
//!
//! 所有写入携带 expected_version，冲突不自动覆盖。

use std::sync::Arc;

use uuid::Uuid;

use crate::error::UseCaseError;
use crate::identity::{Actor, authorize_own_or_any};
use crate::ports::{Clock, PostRepository, SaveOutcome};
use domain::content::post::{Post, PostPatch, PostSnapshot, Slug, Visibility};
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

    /// 创建草稿。作者即 Actor 本人（post.create 语义：创建本人文章）。
    pub async fn create(&self, actor: &Actor, cmd: CreatePostCmd) -> Result<PostDto, UseCaseError> {
        actor.ensure_write_channel()?;
        if !actor.has_permission("post.create") {
            return Err(UseCaseError::Forbidden);
        }
        let slug_raw = cmd
            .slug
            .unwrap_or_else(|| format!("draft-{}", Uuid::now_v7().simple()));
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
        actor.ensure_write_channel()?;
        let (mut post, expected) = self
            .load_authorized(&cmd.target_slug, actor, "post.update", "post.update_any")
            .await?;
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
            return self.commit(post, expected).await;
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
        actor.ensure_write_channel()?;
        let (mut post, expected) = self
            .load_authorized(slug, actor, "post.publish", "post.publish_any")
            .await?;
        let expected = expected_version.unwrap_or(expected);

        if post.publish(self.clock.now()).map_err(map_domain)? {
            return self.commit(post, expected).await;
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
        actor.ensure_write_channel()?;
        let (mut post, expected) = self
            .load_authorized(slug, actor, "post.unpublish", "post.unpublish_any")
            .await?;
        let expected = expected_version.unwrap_or(expected);

        if post.withdraw() {
            return self.commit(post, expected).await;
        }
        Ok(PostDto::from_snapshot(&post.snapshot()))
    }

    /// 文章作者元数据（不含内容）；供 CLI 解析缺省操作身份。
    /// 泄漏面只有作者 id，不构成内容读取。
    pub async fn author_of(&self, slug: &str) -> Result<UserId, UseCaseError> {
        let snapshot = self
            .posts
            .find_by_slug(slug)
            .await?
            .ok_or_else(|| UseCaseError::NotFound(format!("文章 {slug}")))?;
        if snapshot.deleted_at.is_some() {
            return Err(UseCaseError::NotFound(format!("文章 {slug}")));
        }
        Ok(UserId(snapshot.author_id))
    }

    /// CLI/后台读取（任意状态）：own 需归属，any 放行；匿名 HTTP 不走此路径。
    pub async fn find(&self, actor: &Actor, slug: &str) -> Result<PostDto, UseCaseError> {
        let snapshot = self
            .posts
            .find_by_slug(slug)
            .await?
            .ok_or_else(|| UseCaseError::NotFound(format!("文章 {slug}")))?;
        if snapshot.deleted_at.is_some() {
            return Err(UseCaseError::NotFound(format!("文章 {slug}")));
        }
        let post = Post::reconstitute(snapshot);
        authorize_own_or_any(actor, "post.read", "post.read_any", post.author_id())?;
        Ok(PostDto::from_snapshot(&post.snapshot()))
    }

    /// 列出作者的文章：本人列表或 read_any。
    pub async fn list_by_author(
        &self,
        actor: &Actor,
        author: UserId,
    ) -> Result<Vec<PostDto>, UseCaseError> {
        if author != actor.user_id && !actor.has_permission("post.read_any") {
            return Err(UseCaseError::Forbidden);
        }
        let snapshots = self.posts.list_by_author(author.0).await?;
        Ok(snapshots.iter().map(PostDto::from_snapshot).collect())
    }

    /// 提交聚合变更：三态结果映射为用例错误；
    /// 成功时直接采用数据库返回的新版本，不做二次回读。
    async fn commit(&self, post: Post, expected: i64) -> Result<PostDto, UseCaseError> {
        let now = self.clock.now();
        let mut snapshot = post.snapshot();
        match self.posts.save(&snapshot, expected, now).await? {
            SaveOutcome::Saved { new_version } => {
                snapshot.version = new_version;
                snapshot.updated_at = now;
                Ok(PostDto::from_snapshot(&snapshot))
            }
            SaveOutcome::StaleConflict => Err(UseCaseError::VersionConflict),
            SaveOutcome::Gone => Err(UseCaseError::NotFound("文章（已被删除）".into())),
        }
    }

    /// 加载聚合并执行 own/any 授权。
    async fn load_authorized(
        &self,
        slug: &str,
        actor: &Actor,
        own_key: &str,
        any_key: &str,
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
        authorize_own_or_any(actor, own_key, any_key, post.author_id())?;
        let version = post.version();
        Ok((post, version))
    }
}

fn map_domain(e: domain::content::post::PostError) -> UseCaseError {
    UseCaseError::Invalid(e.to_string())
}
