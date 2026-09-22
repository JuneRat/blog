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
use crate::ports::{Clock, PostRepository, SaveOutcome, TagRepository};
use crate::version::checked_version;
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
    /// 初始标签集合（整体写入；用例内去重并校验存在）。
    pub tag_ids: Vec<Uuid>,
}

#[derive(Debug, Clone, Default)]
pub struct EditPostCmd {
    pub target_slug: String,
    pub new_slug: Option<String>,
    pub title: Option<String>,
    pub excerpt: Option<String>,
    pub content: Option<String>,
    pub visibility: Option<Visibility>,
    /// Some(set) 表示同事务整体替换标签关系（含清空：Some(vec![])）；
    /// None 表示本次不触碰标签。
    pub tag_ids: Option<Vec<Uuid>>,
    /// None 表示使用读取到的当前版本（仍可检测读后并发修改）。
    pub expected_version: Option<i64>,
}

/// 面向 CLI/后台的文章视图，包含非公开状态与正文源文。
#[derive(Debug, Clone, serde::Serialize)]
pub struct PostDto {
    pub id: Uuid,
    pub slug: String,
    pub title: String,
    pub excerpt: Option<String>,
    /// Markdown 源文；后台编辑需要，公开 SSR 不走此视图。
    pub content: String,
    pub status: &'static str,
    pub visibility: &'static str,
    pub version: i64,
    pub published_at: Option<time::OffsetDateTime>,
    pub updated_at: time::OffsetDateTime,
    pub deleted: bool,
    pub author_id: Uuid,
    /// 当前关联标签 id（按 id 升序）；名称由前端结合标签目录解析。
    pub tag_ids: Vec<Uuid>,
}

impl PostDto {
    fn from_snapshot(s: &PostSnapshot, tag_ids: Vec<Uuid>) -> Self {
        Self {
            id: s.id,
            slug: s.slug.clone(),
            title: s.title.clone(),
            excerpt: s.excerpt.clone(),
            content: s.content.clone(),
            status: s.status.as_str(),
            visibility: s.visibility.as_str(),
            version: s.version,
            published_at: s.published_at,
            updated_at: s.updated_at,
            deleted: s.deleted_at.is_some(),
            author_id: s.author_id,
            tag_ids,
        }
    }
}

pub struct PostInteractor {
    posts: Arc<dyn PostRepository>,
    /// 标签存在性校验（文章-标签关联的前置检查；写关系仍在 PostRepository 事务内）。
    tags: Arc<dyn TagRepository>,
    clock: Arc<dyn Clock>,
}

impl PostInteractor {
    pub fn new(
        posts: Arc<dyn PostRepository>,
        tags: Arc<dyn TagRepository>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self { posts, tags, clock }
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
        let tag_ids = self.validate_tags(cmd.tag_ids).await?;
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
        // 正文与初始标签关系同一事务写入。
        self.posts.insert(&snapshot, &tag_ids).await?;
        Ok(PostDto::from_snapshot(&snapshot, tag_ids))
    }

    /// 编辑当前正文；保存已发布内容直接更新线上。
    /// tag_ids = Some(set) 时与正文在同一事务整体替换标签关系。
    pub async fn edit(&self, actor: &Actor, cmd: EditPostCmd) -> Result<PostDto, UseCaseError> {
        actor.ensure_write_channel()?;
        let (mut post, expected) = self
            .load_authorized(&cmd.target_slug, actor, "post.update", "post.update_any")
            .await?;
        let expected = checked_version(expected, cmd.expected_version)?;

        let new_tags = match cmd.tag_ids {
            Some(ids) => Some(self.validate_tags(ids).await?),
            None => None,
        };
        // 标签是否有实际变化：与当前集合（同样去重排序后）比较。
        let tags_changed = match new_tags.as_deref() {
            Some(new_set) => {
                let current = self.posts.tags_of(post.snapshot().id).await?;
                current != new_set
            }
            None => false,
        };

        let changed = post
            .edit(PostPatch {
                slug: cmd.new_slug,
                title: cmd.title,
                excerpt: cmd.excerpt,
                content: cmd.content,
                visibility: cmd.visibility,
            })
            .map_err(|e| UseCaseError::Invalid(e.to_string()))?;

        // 仅标签变化也要提交（version+1）；正文与标签都无变化则幂等返回。
        if changed || tags_changed {
            return self.commit(post, expected, new_tags).await;
        }
        let tag_ids = self.posts.tags_of(post.snapshot().id).await?;
        Ok(PostDto::from_snapshot(&post.snapshot(), tag_ids))
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
        let expected = checked_version(expected, expected_version)?;

        if post.publish(self.clock.now()).map_err(map_domain)? {
            return self.commit(post, expected, None).await;
        }
        let tag_ids = self.posts.tags_of(post.snapshot().id).await?;
        Ok(PostDto::from_snapshot(&post.snapshot(), tag_ids))
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
        let expected = checked_version(expected, expected_version)?;

        if post.withdraw() {
            return self.commit(post, expected, None).await;
        }
        let tag_ids = self.posts.tags_of(post.snapshot().id).await?;
        Ok(PostDto::from_snapshot(&post.snapshot(), tag_ids))
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
        let tag_ids = self.posts.tags_of(post.snapshot().id).await?;
        Ok(PostDto::from_snapshot(&post.snapshot(), tag_ids))
    }

    /// 列出作者的文章：本人列表需 `post.read`，他人列表需 `post.read_any`。
    ///
    /// 与单篇 [`PostInteractor::find`] 使用同一套 own/any 授权：否则同一用户会出现
    /// 「单篇 `GET` 403、列表 `GET` 200」的自相矛盾（例如被移除 `author` 角色后仍能列草稿）。
    pub async fn list_by_author(
        &self,
        actor: &Actor,
        author: UserId,
    ) -> Result<Vec<PostDto>, UseCaseError> {
        authorize_own_or_any(actor, "post.read", "post.read_any", author)?;
        let snapshots = self.posts.list_by_author(author.0).await?;
        // 列表是摘要形态：不逐篇补标签（编辑器打开详情时才读取）。
        Ok(snapshots
            .iter()
            .map(|s| PostDto::from_snapshot(s, Vec::new()))
            .collect())
    }

    /// 提交聚合变更：三态结果映射为用例错误；
    /// 成功时直接采用数据库返回的新版本，不做二次回读。
    /// `new_tags = Some(set)` 时标签关系与正文在同一事务整体替换。
    async fn commit(
        &self,
        post: Post,
        expected: i64,
        new_tags: Option<Vec<Uuid>>,
    ) -> Result<PostDto, UseCaseError> {
        let now = self.clock.now();
        let mut snapshot = post.snapshot();
        match self
            .posts
            .save(&snapshot, expected, now, new_tags.as_deref())
            .await?
        {
            SaveOutcome::Saved { new_version } => {
                snapshot.version = new_version;
                snapshot.updated_at = now;
                // 未经替换（发布/撤回）时回读当前集合，保证响应与存储一致。
                let tag_ids = match new_tags {
                    Some(set) => set,
                    None => self.posts.tags_of(snapshot.id).await?,
                };
                Ok(PostDto::from_snapshot(&snapshot, tag_ids))
            }
            SaveOutcome::StaleConflict => Err(UseCaseError::VersionConflict),
            SaveOutcome::Gone => Err(UseCaseError::NotFound("文章（已被删除）".into())),
        }
    }

    /// 标签集合规范化与存在性校验：去重、按 id 排序（与 tags_of 读取顺序一致），
    /// 缺失的 id 报为可定位的参数错误。重复 id 是幂等语义（post_tags 主键去重），
    /// 不作为错误——一次提交里勾选同一标签两次不应让整次保存失败。
    async fn validate_tags(&self, ids: Vec<Uuid>) -> Result<Vec<Uuid>, UseCaseError> {
        let unique: Vec<Uuid> = std::collections::BTreeSet::from_iter(ids)
            .into_iter()
            .collect();
        if unique.is_empty() {
            return Ok(unique);
        }
        let existing = self.tags.existing_ids(&unique).await?;
        if existing.len() != unique.len() {
            let missing: Vec<Uuid> = unique
                .iter()
                .filter(|id| !existing.contains(id))
                .copied()
                .collect();
            return Err(UseCaseError::Invalid(format!(
                "所选标签不存在：{missing:?}"
            )));
        }
        Ok(unique)
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
