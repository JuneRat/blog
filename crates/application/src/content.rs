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
use crate::ports::{
    Clock, PostCommitOutcome, PostRecord, PostRepository, SaveOutcome, TagRepository,
};
use crate::version::checked_version;
use domain::content::{Post, PostDraftMetadata, PostPatch, PostSnapshot, Slug, Visibility};
use domain::identity::UserId;

/// 向接口层转出的值对象（interfaces 不直接依赖 domain crate）。
pub use domain::content::Visibility as PostVisibility;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SeriesPlacement {
    pub series_id: Uuid,
    #[serde(default)]
    pub position: i32,
}
impl From<domain::content::SeriesPlacement> for SeriesPlacement {
    fn from(p: domain::content::SeriesPlacement) -> Self {
        Self {
            series_id: p.series_id,
            position: p.position,
        }
    }
}
impl From<SeriesPlacement> for domain::content::SeriesPlacement {
    fn from(p: SeriesPlacement) -> Self {
        Self {
            series_id: p.series_id,
            position: p.position,
        }
    }
}

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
    /// 初始分类（存在性由用例校验）。
    pub category_id: Option<Uuid>,
    /// 初始系列集合（空数组表示不加入系列）。
    pub series: Vec<SeriesPlacement>,
    /// 初始封面媒体资产（None = 无封面）。
    pub cover_media_id: Option<Uuid>,
}

#[derive(Debug, Clone, Default)]
pub struct EditPostCmd {
    pub id: Uuid,
    pub new_slug: Option<String>,
    pub title: Option<String>,
    pub excerpt: Option<String>,
    pub content: Option<String>,
    pub visibility: Option<Visibility>,
    /// Some(set) 表示同事务整体替换标签关系（含清空：Some(vec![])）；
    /// None 表示本次不触碰标签。
    pub tag_ids: Option<Vec<Uuid>>,
    /// 三态：None 不修改；Some(None) 清空分类；Some(Some(id)) 设置分类。
    pub category_id: Option<Option<Uuid>>,
    /// None 不修改；Some 整体替换系列集合。
    pub series: Option<Vec<SeriesPlacement>>,
    /// 封面三态：None 不修改；Some(None) 移除封面；Some(Some(id)) 设置封面。
    pub cover_media_id: Option<Option<Uuid>>,
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
    /// 所属分类 id（至多一个；None = 未分类）。
    pub category_id: Option<Uuid>,
    /// 所属系列与各系列内的排序权重。
    pub series: Vec<SeriesPlacement>,
    /// 封面媒体资产 id（None = 无封面）；URL 由接口层按 `/media/{id}` 生成。
    pub cover_media_id: Option<Uuid>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct TrashPage {
    pub items: Vec<PostDto>,
    pub total: i64,
    pub page: i64,
    pub per_page: i64,
}

impl PostDto {
    fn from_record(record: PostRecord) -> Self {
        Self::from_snapshot(&record.snapshot, record.tag_ids)
    }

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
            category_id: s.category_id,
            series: s.series.iter().copied().map(Into::into).collect(),
            cover_media_id: s.cover_media_id,
        }
    }
}

pub struct PostInteractor {
    posts: Arc<dyn PostRepository>,
    /// 标签存在性校验（文章-标签关联的前置检查；写关系仍在 PostRepository 事务内）。
    tags: Arc<dyn TagRepository>,
    /// 分类存在性校验（文章设置分类的前置检查；写关系仍在 PostRepository 事务内）。
    categories: Arc<dyn crate::ports::CategoryRepository>,
    /// 系列存在性校验（文章设置系列的前置检查；写关系仍在 PostRepository 事务内）。
    series: Arc<dyn crate::ports::SeriesRepository>,
    clock: Arc<dyn Clock>,
    /// 封面附着的可用性校验（`ensure_attachable`）。
    media_guard: Arc<dyn crate::ports::MediaRefGuard>,
}

impl PostInteractor {
    pub fn new(
        posts: Arc<dyn PostRepository>,
        tags: Arc<dyn TagRepository>,
        categories: Arc<dyn crate::ports::CategoryRepository>,
        series: Arc<dyn crate::ports::SeriesRepository>,
        clock: Arc<dyn Clock>,
        media_guard: Arc<dyn crate::ports::MediaRefGuard>,
    ) -> Self {
        Self {
            posts,
            tags,
            categories,
            series,
            clock,
            media_guard,
        }
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
        if let Some(category_id) = cmd.category_id {
            self.validate_category(category_id).await?;
        }
        for placement in &cmd.series {
            self.validate_series(placement.series_id).await?;
        }
        // 新文章的封面总是首次附着，一律过可用性校验。
        if let Some(cover_media_id) = cmd.cover_media_id {
            crate::media::ensure_attachable(&*self.media_guard, cover_media_id).await?;
        }
        let post = Post::create_draft_with_metadata(
            actor.user_id,
            slug,
            cmd.title,
            cmd.excerpt,
            cmd.content,
            cmd.visibility,
            PostDraftMetadata {
                category_id: cmd.category_id,
                series: cmd.series.into_iter().map(Into::into).collect(),
                cover_media_id: cmd.cover_media_id,
            },
            self.clock.now(),
        )
        .map_err(|e| UseCaseError::Invalid(e.to_string()))?;
        let record = self
            .posts
            .insert_post(&post, &tag_ids, actor.audit_actor_id())
            .await?;
        Ok(PostDto::from_record(record))
    }

    /// 编辑当前正文；保存已发布内容直接更新线上。
    /// tag_ids = Some(set) 时与正文在同一事务整体替换标签关系。
    pub async fn edit(&self, actor: &Actor, cmd: EditPostCmd) -> Result<PostDto, UseCaseError> {
        actor.ensure_write_channel()?;
        let (mut post, record) = self
            .load_authorized(cmd.id, actor, "post.update", "post.update_any")
            .await?;
        let expected = checked_version(record.snapshot.version, cmd.expected_version)?;

        if let Some(Some(category_id)) = cmd.category_id {
            self.validate_category(category_id).await?;
        }
        if let Some(placements) = &cmd.series {
            for placement in placements {
                self.validate_series(placement.series_id).await?;
            }
        }
        // 封面只有**换成新资产**时才过可用性校验：编辑者重复提交当前封面
        // （含编辑他人文章）不重新授权，历史引用不卡正常保存。
        if let Some(Some(cover_media_id)) = cmd.cover_media_id
            && post.snapshot().cover_media_id != Some(cover_media_id)
        {
            crate::media::ensure_attachable(&*self.media_guard, cover_media_id).await?;
        }
        let new_tags = match cmd.tag_ids {
            Some(ids) => Some(self.validate_tags(ids).await?),
            None => None,
        };
        // 标签是否有实际变化：与当前集合（同样去重排序后）比较。
        let tags_changed = match new_tags.as_deref() {
            Some(new_set) => record.tag_ids != new_set,
            None => false,
        };

        // 聚合的 edit() 报告分类变化（仅分类变化也 changed=true → version+1）。
        let changed = post
            .edit(PostPatch {
                slug: cmd.new_slug,
                title: cmd.title,
                excerpt: cmd.excerpt,
                content: cmd.content,
                visibility: cmd.visibility,
                category_id: cmd.category_id,
                series: cmd
                    .series
                    .map(|series| series.into_iter().map(Into::into).collect()),
                cover_media_id: cmd.cover_media_id,
            })
            .map_err(|e| UseCaseError::Invalid(e.to_string()))?;

        // 仅标签变化也要提交（version+1）；正文与标签都无变化则幂等返回。
        if changed || tags_changed {
            return self.commit(actor, post, expected, new_tags).await;
        }
        Ok(PostDto::from_record(record))
    }

    /// 立即发布草稿或预约内容；保留过去的发布时间，未来预约改为当前时间。
    pub async fn publish(
        &self,
        actor: &Actor,
        id: Uuid,
        expected_version: Option<i64>,
    ) -> Result<PostDto, UseCaseError> {
        actor.ensure_write_channel()?;
        let (mut post, record) = self
            .load_authorized(id, actor, "post.publish", "post.publish_any")
            .await?;
        let expected = checked_version(record.snapshot.version, expected_version)?;

        if post.publish(self.clock.now()).map_err(map_domain)? {
            return self.commit(actor, post, expected, None).await;
        }
        Ok(PostDto::from_record(record))
    }

    /// 撤回、取消预约或解除归档：回到 draft，slug 保持锁定；草稿幂等。
    pub async fn withdraw(
        &self,
        actor: &Actor,
        id: Uuid,
        expected_version: Option<i64>,
    ) -> Result<PostDto, UseCaseError> {
        actor.ensure_write_channel()?;
        let (mut post, record) = self
            .load_authorized(id, actor, "post.unpublish", "post.unpublish_any")
            .await?;
        let expected = checked_version(record.snapshot.version, expected_version)?;

        if post.withdraw() {
            return self.commit(actor, post, expected, None).await;
        }
        Ok(PostDto::from_record(record))
    }

    pub async fn schedule(
        &self,
        actor: &Actor,
        id: Uuid,
        at: time::OffsetDateTime,
        version: Option<i64>,
    ) -> Result<PostDto, UseCaseError> {
        actor.ensure_write_channel()?;
        let (mut post, record) = self
            .load_authorized(id, actor, "post.publish", "post.publish_any")
            .await?;
        let expected = checked_version(record.snapshot.version, version)?;
        if post.schedule(at, self.clock.now()).map_err(map_domain)? {
            return self.commit(actor, post, expected, None).await;
        }
        Ok(PostDto::from_record(record))
    }

    pub async fn archive(
        &self,
        actor: &Actor,
        id: Uuid,
        version: Option<i64>,
    ) -> Result<PostDto, UseCaseError> {
        actor.ensure_write_channel()?;
        let (mut post, record) = self
            .load_authorized(id, actor, "post.unpublish", "post.unpublish_any")
            .await?;
        let expected = checked_version(record.snapshot.version, version)?;
        if post.archive().map_err(map_domain)? {
            return self.commit(actor, post, expected, None).await;
        }
        Ok(PostDto::from_record(record))
    }

    /// 文章作者元数据（不含内容）；供 CLI 解析缺省操作身份。
    /// 泄漏面只有作者 id，不构成内容读取。
    pub async fn author_of(&self, id: Uuid) -> Result<UserId, UseCaseError> {
        let snapshot = self
            .posts
            .find_by_id(id)
            .await?
            .ok_or_else(|| UseCaseError::NotFound(format!("文章 {id}")))?;
        if snapshot.deleted_at.is_some() {
            return Err(UseCaseError::NotFound(format!("文章 {id}")));
        }
        Ok(UserId(snapshot.author_id))
    }

    /// CLI/后台读取（任意状态）：own 需归属，any 放行；匿名 HTTP 不走此路径。
    pub async fn find(&self, actor: &Actor, id: Uuid) -> Result<PostDto, UseCaseError> {
        let (_, record) = self
            .load_authorized(id, actor, "post.read", "post.read_any")
            .await?;
        Ok(PostDto::from_record(record))
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
            .filter(|s| s.deleted_at.is_none())
            .map(|s| PostDto::from_snapshot(s, Vec::new()))
            .collect())
    }

    pub async fn list_trash(
        &self,
        actor: &Actor,
        author: UserId,
        page: i64,
    ) -> Result<TrashPage, UseCaseError> {
        authorize_own_or_any(actor, "post.read", "post.read_any", author)?;
        if !(1..=i64::MAX / 20).contains(&page) {
            return Err(UseCaseError::Invalid("页码超出范围".into()));
        }
        let (snapshots, total) = self
            .posts
            .list_trash_by_author(author.0, 20, (page - 1) * 20)
            .await?;
        Ok(TrashPage {
            items: snapshots
                .iter()
                .map(|s| PostDto::from_snapshot(s, Vec::new()))
                .collect(),
            total,
            page,
            per_page: 20,
        })
    }

    pub async fn trash(
        &self,
        actor: &Actor,
        id: Uuid,
        version: Option<i64>,
    ) -> Result<PostDto, UseCaseError> {
        actor.ensure_write_channel()?;
        let (mut post, record) = self
            .load_authorized(id, actor, "post.delete", "post.delete_any")
            .await?;
        let expected = checked_version(record.snapshot.version, version)?;
        let now = self.clock.now();
        post.trash(now);
        Self::committed(
            self.posts
                .commit_lifecycle(&post, expected, now, actor.audit_actor_id())
                .await?,
        )
    }

    pub async fn restore(
        &self,
        actor: &Actor,
        id: Uuid,
        version: Option<i64>,
    ) -> Result<PostDto, UseCaseError> {
        actor.ensure_write_channel()?;
        let record = self.load_record(id).await?;
        authorize_own_or_any(
            actor,
            "post.delete",
            "post.delete_any",
            UserId(record.snapshot.author_id),
        )?;
        if record.snapshot.deleted_at.is_none() {
            return Err(UseCaseError::NotFound(format!("回收站文章 {id}")));
        }
        let expected = checked_version(record.snapshot.version, version)?;
        let mut post = Post::reconstitute(record.snapshot)
            .map_err(|e| UseCaseError::Repository(e.to_string()))?;
        post.restore();
        Self::committed(
            self.posts
                .commit_lifecycle(&post, expected, self.clock.now(), actor.audit_actor_id())
                .await?,
        )
    }

    pub async fn purge(
        &self,
        actor: &Actor,
        id: Uuid,
        version: Option<i64>,
    ) -> Result<(), UseCaseError> {
        actor.ensure_write_channel()?;
        if !actor.has_permission("post.purge") {
            return Err(UseCaseError::Forbidden);
        }
        let record = self.load_record(id).await?;
        if record.snapshot.deleted_at.is_none() {
            return Err(UseCaseError::Invalid("只能永久删除回收站文章".into()));
        }
        let expected = checked_version(record.snapshot.version, version)?;
        match self
            .posts
            .purge(record.snapshot.id, expected, actor.audit_actor_id())
            .await?
        {
            SaveOutcome::Saved { .. } => Ok(()),
            SaveOutcome::StaleConflict => Err(UseCaseError::VersionConflict),
            SaveOutcome::Gone => Err(UseCaseError::NotFound(format!("文章 {id}"))),
        }
    }

    /// All relations and the returned editor record belong to the same conditional commit.
    async fn commit(
        &self,
        actor: &Actor,
        post: Post,
        expected: i64,
        new_tags: Option<Vec<Uuid>>,
    ) -> Result<PostDto, UseCaseError> {
        Self::committed(
            self.posts
                .commit_post(
                    &post,
                    expected,
                    self.clock.now(),
                    new_tags.as_deref(),
                    actor.audit_actor_id(),
                )
                .await?,
        )
    }

    fn committed(result: PostCommitOutcome) -> Result<PostDto, UseCaseError> {
        match result {
            PostCommitOutcome::Saved(record) => Ok(PostDto::from_record(*record)),
            PostCommitOutcome::StaleConflict => Err(UseCaseError::VersionConflict),
            PostCommitOutcome::Gone => Err(UseCaseError::NotFound("文章（已被删除）".into())),
        }
    }

    /// 系列存在性校验：未知 id 报为可定位的参数错误。
    async fn validate_series(&self, series_id: Uuid) -> Result<(), UseCaseError> {
        if !self.series.existing_id(series_id).await? {
            return Err(UseCaseError::Invalid(format!(
                "所选系列不存在：{series_id}"
            )));
        }
        Ok(())
    }

    /// 分类存在性校验：未知 id 报为可定位的参数错误。
    async fn validate_category(&self, category_id: Uuid) -> Result<(), UseCaseError> {
        if !self.categories.existing_id(category_id).await? {
            return Err(UseCaseError::Invalid(format!(
                "所选分类不存在：{category_id}"
            )));
        }
        Ok(())
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

    async fn load_record(&self, id: Uuid) -> Result<PostRecord, UseCaseError> {
        self.posts
            .find_record_by_id(id)
            .await?
            .ok_or_else(|| UseCaseError::NotFound(format!("文章 {id}")))
    }

    /// Authorize the same complete snapshot used by the editor and write preconditions.
    async fn load_authorized(
        &self,
        id: Uuid,
        actor: &Actor,
        own_key: &str,
        any_key: &str,
    ) -> Result<(Post, PostRecord), UseCaseError> {
        let record = self.load_record(id).await?;
        if record.snapshot.deleted_at.is_some() {
            return Err(UseCaseError::NotFound(format!("文章 {id}")));
        }
        authorize_own_or_any(actor, own_key, any_key, UserId(record.snapshot.author_id))?;
        Ok((
            Post::reconstitute(record.snapshot.clone())
                .map_err(|e| UseCaseError::Repository(e.to_string()))?,
            record,
        ))
    }
}

fn map_domain(e: domain::content::PostError) -> UseCaseError {
    UseCaseError::Invalid(e.to_string())
}
