//! Post 聚合：一份当前正文的状态、转换与不变量。
//!
//! 规则来源 docs/content-lifecycle.md：
//! - 每条记录只有一份当前正文；保存已发布内容直接更新线上。
//! - slug 草稿创建时即唯一；首次预约或发布（published_at 产生）后锁定，撤回也不解锁。
//! - 首次发布校验标题与正文；重新发布保留过去的 published_at。
//! - 撤回、取消预约或解除归档 → draft。
//! - version 由仓储按“有实际变化才 +1”递增；聚合只报告是否发生变化。

use time::OffsetDateTime;
use uuid::Uuid;

use super::{Slug, SlugError, Visibility};
use crate::identity::UserId;

pub const TITLE_MAX_CHARS: usize = 300;
pub const EXCERPT_MAX_CHARS: usize = 1000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PostId(pub Uuid);

impl PostId {
    pub fn generate() -> Self {
        Self(Uuid::now_v7())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PostStatus {
    Draft,
    Scheduled,
    Published,
    Archived,
}

impl PostStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            PostStatus::Draft => "draft",
            PostStatus::Scheduled => "scheduled",
            PostStatus::Published => "published",
            PostStatus::Archived => "archived",
        }
    }

    /// 从数据库字符串恢复；未知值视为损坏数据。
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "draft" => Some(PostStatus::Draft),
            "scheduled" => Some(PostStatus::Scheduled),
            "published" => Some(PostStatus::Published),
            "archived" => Some(PostStatus::Archived),
            _ => None,
        }
    }
}

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum PostError {
    #[error("回收站内的文章须先恢复才能编辑或发布")]
    InTrash,
    #[error(transparent)]
    ContentBudget(#[from] super::budget::ContentBudgetError),
    #[error("快照结构无效：{0}")]
    InvalidSnapshot(&'static str),
    #[error(transparent)]
    InvalidSlug(#[from] SlugError),
    #[error("标题长度不能超过 {TITLE_MAX_CHARS} 字符")]
    TitleTooLong,
    #[error("摘要长度不能超过 {EXCERPT_MAX_CHARS} 字符")]
    ExcerptTooLong,
    #[error("系列排序权重不能为负数，收到 {0}")]
    InvalidSeriesPosition(i32),
    #[error("首次预约或发布后 slug 已锁定，退回草稿也不允许改名")]
    SlugLocked,
    #[error("发布前标题不能为空")]
    EmptyTitleOnPublish,
    #[error("发布前正文不能为空")]
    EmptyContentOnPublish,
    #[error("已发布文章的标题与正文不能清空")]
    EmptyContentWhenPublished,
    #[error("归档内容须先退回草稿")]
    ArchivedRequiresDraft,
    #[error("预约时间必须晚于当前时间")]
    ScheduleMustBeFuture,
    #[error("已发布内容须先撤回再预约")]
    AlreadyPublished,
    #[error("系列不能重复")]
    DuplicateSeries,
    #[error("归档内容须先退回草稿再编辑")]
    ArchivedNotEditable,
}

/// 一篇文章在一个系列中的排序权重；允许零及与其他文章重复的权重。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SeriesPlacement {
    pub series_id: Uuid,
    pub position: i32,
}

impl SeriesPlacement {
    pub fn new(series_id: Uuid, position: i32) -> Result<Self, PostError> {
        if position < 0 {
            return Err(PostError::InvalidSeriesPosition(position));
        }
        Ok(Self {
            series_id,
            position,
        })
    }
}

fn normalize_series(mut series: Vec<SeriesPlacement>) -> Result<Vec<SeriesPlacement>, PostError> {
    for placement in &series {
        SeriesPlacement::new(placement.series_id, placement.position)?;
    }
    series.sort_by_key(|placement| placement.series_id);
    if series
        .windows(2)
        .any(|pair| pair[0].series_id == pair[1].series_id)
    {
        return Err(PostError::DuplicateSeries);
    }
    Ok(series)
}

/// 创建草稿时一并设置的关联信息；存在性由应用和提交事务校验。
/// 系列序号由聚合使用与编辑相同的规则验证。
#[derive(Debug, Clone, Default)]
pub struct PostDraftMetadata {
    pub category_id: Option<Uuid>,
    pub series: Vec<SeriesPlacement>,
    pub cover_media_id: Option<Uuid>,
}

/// 仓储重建聚合的受控快照载体。
#[derive(Debug, Clone, PartialEq)]
pub struct PostSnapshot {
    pub id: Uuid,
    pub author_id: Uuid,
    pub category_id: Option<Uuid>,
    pub series: Vec<SeriesPlacement>,
    pub title: String,
    pub slug: String,
    pub excerpt: Option<String>,
    pub content: String,
    /// 封面所引用的媒体资产（None = 无封面）。
    ///
    /// 与正文引用同源：保存时把 `{封面} ∪ 正文图片` 写进 `media_refs`，
    /// 为使用统计与独立物理清理保留依据；图片 URL 不受内容公开状态限制。
    pub cover_media_id: Option<Uuid>,
    pub status: PostStatus,
    pub visibility: Visibility,
    pub published_at: Option<OffsetDateTime>,
    pub version: i64,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
    pub deleted_at: Option<OffsetDateTime>,
}

/// 编辑补丁：None 表示不修改该字段。
///
/// `category_id` 是三态：`None` 不修改；`Some(None)` 清空分类；
/// `Some(Some(id))` 设置分类（存在性由用例校验）。
#[derive(Debug, Clone, Default)]
pub struct PostPatch {
    pub slug: Option<String>,
    pub title: Option<String>,
    pub excerpt: Option<String>,
    pub content: Option<String>,
    pub visibility: Option<Visibility>,
    pub category_id: Option<Option<Uuid>>,
    /// None 保留；Some 整体替换系列集合（空数组表示清空）。
    pub series: Option<Vec<SeriesPlacement>>,
    /// 封面三态：None 不修改；Some(None) 移除封面；Some(Some(id)) 设置封面。
    /// 资产存在性与 `ready` 状态由保存事务内的引用校验兜底。
    pub cover_media_id: Option<Option<Uuid>>,
}

/// Post 聚合。字段私有，状态转换只经由行为方法。
#[derive(Debug, Clone)]
pub struct Post {
    snapshot: PostSnapshot,
}

impl Post {
    /// 创建草稿：标题与正文可为空（自动保存场景），slug 必须已验证。
    #[allow(clippy::too_many_arguments)]
    pub fn create_draft(
        author: UserId,
        slug: Slug,
        title: String,
        excerpt: Option<String>,
        content: String,
        visibility: Visibility,
        now: OffsetDateTime,
    ) -> Result<Self, PostError> {
        Self::create_draft_with_metadata(
            author,
            slug,
            title,
            excerpt,
            content,
            visibility,
            PostDraftMetadata::default(),
            now,
        )
    }

    /// 创建草稿并一次设置关联信息；不得先导出快照再绕过领域规则修改。
    #[allow(clippy::too_many_arguments)]
    pub fn create_draft_with_metadata(
        author: UserId,
        slug: Slug,
        title: String,
        excerpt: Option<String>,
        content: String,
        visibility: Visibility,
        metadata: PostDraftMetadata,
        now: OffsetDateTime,
    ) -> Result<Self, PostError> {
        super::budget::validate_source(&content)?;
        Self::validate_mutation_fields(&title, excerpt.as_deref())?;
        let series = normalize_series(metadata.series)?;
        Ok(Self {
            snapshot: PostSnapshot {
                id: PostId::generate().0,
                author_id: author.0,
                category_id: metadata.category_id,
                series,
                title,
                slug: slug.into_string(),
                excerpt,
                content,
                cover_media_id: metadata.cover_media_id,
                status: PostStatus::Draft,
                visibility,
                published_at: None,
                version: 1,
                created_at: now,
                updated_at: now,
                deleted_at: None,
            },
        })
    }

    /// 受控重建入口：仅供持久化适配器从数据库恢复聚合。
    pub fn reconstitute(mut snapshot: PostSnapshot) -> Result<Self, PostError> {
        Slug::new(&snapshot.slug)?;
        Self::validate_mutation_fields(&snapshot.title, snapshot.excerpt.as_deref())?;
        snapshot.series = normalize_series(snapshot.series)?;
        if snapshot.version < 1 {
            return Err(PostError::InvalidSnapshot("版本必须为正整数"));
        }
        if matches!(
            snapshot.status,
            PostStatus::Published | PostStatus::Scheduled
        ) {
            if snapshot.published_at.is_none() {
                return Err(PostError::InvalidSnapshot("已发布文章缺少发布时间"));
            }
            if snapshot.title.trim().is_empty() || snapshot.content.trim().is_empty() {
                return Err(PostError::EmptyContentWhenPublished);
            }
        }
        Ok(Self { snapshot })
    }

    pub fn snapshot(&self) -> PostSnapshot {
        self.snapshot.clone()
    }

    pub fn id(&self) -> PostId {
        PostId(self.snapshot.id)
    }

    pub fn author_id(&self) -> UserId {
        UserId(self.snapshot.author_id)
    }

    pub fn slug(&self) -> &str {
        &self.snapshot.slug
    }

    pub fn status(&self) -> PostStatus {
        self.snapshot.status
    }

    pub fn version(&self) -> i64 {
        self.snapshot.version
    }

    /// 匿名公开条件：published + public + 未进回收站。
    pub fn is_publicly_visible(&self, now: OffsetDateTime) -> bool {
        self.snapshot.status == PostStatus::Published
            && self.snapshot.visibility == Visibility::Public
            && self.snapshot.deleted_at.is_none()
            && self.snapshot.published_at.is_some_and(|at| at <= now)
    }

    fn validate_mutation_fields(title: &str, excerpt: Option<&str>) -> Result<(), PostError> {
        if title.chars().count() > TITLE_MAX_CHARS {
            return Err(PostError::TitleTooLong);
        }
        if let Some(excerpt) = excerpt
            && excerpt.chars().count() > EXCERPT_MAX_CHARS
        {
            return Err(PostError::ExcerptTooLong);
        }
        Ok(())
    }

    /// 编辑当前正文。保存已发布内容会直接反映到线上，因此已发布状态
    /// 不允许把标题/正文清空。返回是否存在实际变化（决定 version 是否 +1）。
    ///
    /// 实现：先在候选值上完成全部校验，再整体提交——任一校验失败时
    /// 聚合保持原状，不会留下半套修改。
    pub fn edit(&mut self, patch: PostPatch) -> Result<bool, PostError> {
        if self.snapshot.deleted_at.is_some() {
            return Err(PostError::InTrash);
        }
        if self.snapshot.status == PostStatus::Archived {
            return Err(PostError::ArchivedNotEditable);
        }

        // 1. 计算候选值（未提供的字段保持当前值）。
        let new_title = patch.title.unwrap_or_else(|| self.snapshot.title.clone());
        let new_excerpt_owned = patch
            .excerpt
            .map(|e| if e.is_empty() { None } else { Some(e) })
            .unwrap_or_else(|| self.snapshot.excerpt.clone());
        let new_content = patch
            .content
            .unwrap_or_else(|| self.snapshot.content.clone());
        let new_visibility = patch.visibility.unwrap_or(self.snapshot.visibility);
        let new_category_id = patch.category_id.unwrap_or(self.snapshot.category_id);
        let new_series =
            normalize_series(patch.series.unwrap_or_else(|| self.snapshot.series.clone()))?;
        let slug_changed = match patch.slug.as_deref() {
            Some(new_slug) => new_slug != self.snapshot.slug,
            None => false,
        };
        let new_cover_media_id = patch.cover_media_id.unwrap_or(self.snapshot.cover_media_id);

        // 2. 全量校验候选值（不写任何字段）。
        super::budget::validate_source(&new_content)?;
        Self::validate_mutation_fields(&new_title, new_excerpt_owned.as_deref())?;
        if slug_changed {
            // 首次预约或发布产生 published_at 后 slug 锁定；退回草稿不解锁。
            if self.snapshot.published_at.is_some() {
                return Err(PostError::SlugLocked);
            }
            Slug::new(
                patch
                    .slug
                    .as_deref()
                    .expect("slug_changed 蕴含 patch.slug 存在"),
            )?;
        }
        if matches!(
            self.snapshot.status,
            PostStatus::Published | PostStatus::Scheduled
        ) {
            if new_title.trim().is_empty() {
                return Err(PostError::EmptyContentWhenPublished);
            }
            if new_content.trim().is_empty() {
                return Err(PostError::EmptyContentWhenPublished);
            }
        }

        // 3. 整体提交。
        let mut changed = false;
        if new_title != self.snapshot.title {
            self.snapshot.title = new_title;
            changed = true;
        }
        if new_excerpt_owned != self.snapshot.excerpt {
            self.snapshot.excerpt = new_excerpt_owned;
            changed = true;
        }
        if new_content != self.snapshot.content {
            self.snapshot.content = new_content;
            changed = true;
        }
        if new_visibility != self.snapshot.visibility {
            self.snapshot.visibility = new_visibility;
            changed = true;
        }
        if new_category_id != self.snapshot.category_id {
            self.snapshot.category_id = new_category_id;
            changed = true;
        }
        if new_series != self.snapshot.series {
            self.snapshot.series = new_series;
            changed = true;
        }
        if new_cover_media_id != self.snapshot.cover_media_id {
            self.snapshot.cover_media_id = new_cover_media_id;
            changed = true;
        }
        if slug_changed {
            self.snapshot.slug = patch.slug.expect("slug_changed 蕴含 patch.slug 存在");
            changed = true;
        }

        Ok(changed)
    }

    /// 立即发布；过去的发布时间保留，未来的预约时间改为当前时间。
    /// 已发布时幂等；归档内容需要先退回草稿。
    pub fn publish(&mut self, now: OffsetDateTime) -> Result<bool, PostError> {
        super::budget::validate_source(&self.snapshot.content)?;
        if self.snapshot.deleted_at.is_some() {
            return Err(PostError::InTrash);
        }
        match self.snapshot.status {
            PostStatus::Published => Ok(false),
            PostStatus::Archived => Err(PostError::ArchivedRequiresDraft),
            PostStatus::Draft | PostStatus::Scheduled => {
                if self.snapshot.title.trim().is_empty() {
                    return Err(PostError::EmptyTitleOnPublish);
                }
                if self.snapshot.content.trim().is_empty() {
                    return Err(PostError::EmptyContentOnPublish);
                }
                self.snapshot.status = PostStatus::Published;
                if self.snapshot.published_at.is_none_or(|at| at > now) {
                    self.snapshot.published_at = Some(now);
                }
                Ok(true)
            }
        }
    }

    /// 撤回、取消预约或解除归档：回到草稿，保留 published_at。
    pub fn withdraw(&mut self) -> bool {
        if self.snapshot.deleted_at.is_none() && self.snapshot.status != PostStatus::Draft {
            self.snapshot.status = PostStatus::Draft;
            true
        } else {
            false
        }
    }

    /// 预约发布只接受草稿或已有预约；所有校验通过后才改变状态。
    pub fn schedule(&mut self, at: OffsetDateTime, now: OffsetDateTime) -> Result<bool, PostError> {
        if self.snapshot.deleted_at.is_some() {
            return Err(PostError::InTrash);
        }
        if self.snapshot.status == PostStatus::Archived {
            return Err(PostError::ArchivedRequiresDraft);
        }
        if self.snapshot.status == PostStatus::Published {
            return Err(PostError::AlreadyPublished);
        }
        if at <= now {
            return Err(PostError::ScheduleMustBeFuture);
        }
        super::budget::validate_source(&self.snapshot.content)?;
        if self.snapshot.title.trim().is_empty() {
            return Err(PostError::EmptyTitleOnPublish);
        }
        if self.snapshot.content.trim().is_empty() {
            return Err(PostError::EmptyContentOnPublish);
        }
        let changed =
            self.snapshot.status != PostStatus::Scheduled || self.snapshot.published_at != Some(at);
        self.snapshot.status = PostStatus::Scheduled;
        self.snapshot.published_at = Some(at);
        Ok(changed)
    }

    pub fn archive(&mut self) -> Result<bool, PostError> {
        if self.snapshot.deleted_at.is_some() {
            return Err(PostError::InTrash);
        }
        let changed = self.snapshot.status != PostStatus::Archived;
        self.snapshot.status = PostStatus::Archived;
        Ok(changed)
    }

    /// 移入回收站，保留原状态与发布时间。重复操作幂等。
    /// version 与 updated_at 由提交边界统一处理。
    pub fn trash(&mut self, now: OffsetDateTime) -> bool {
        if self.snapshot.deleted_at.is_some() {
            return false;
        }
        self.snapshot.deleted_at = Some(now);
        true
    }

    /// 从回收站恢复为草稿，避免意外重新上线。
    /// 保留发布时间（slug 仍锁定）；非回收站内容幂等无操作。
    /// version 与 updated_at 由提交边界统一处理。
    pub fn restore(&mut self) -> bool {
        if self.snapshot.deleted_at.is_none() {
            return false;
        }
        self.snapshot.deleted_at = None;
        self.snapshot.status = PostStatus::Draft;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn author() -> UserId {
        UserId::generate()
    }

    fn draft() -> Post {
        Post::create_draft(
            author(),
            Slug::new("hello-world").unwrap(),
            "标题".into(),
            None,
            "# Hello\n正文".into(),
            Visibility::Public,
            OffsetDateTime::now_utc(),
        )
        .unwrap()
    }

    #[test]
    fn draft_may_have_empty_title_and_content() {
        let post = Post::create_draft(
            author(),
            Slug::new("d-1").unwrap(),
            String::new(),
            None,
            String::new(),
            Visibility::Public,
            OffsetDateTime::now_utc(),
        )
        .unwrap();
        assert_eq!(post.status(), PostStatus::Draft);
        assert!(!post.is_publicly_visible(OffsetDateTime::now_utc()));
    }

    #[test]
    fn create_draft_sets_validated_metadata_atomically() {
        let category_id = Uuid::now_v7();
        let series_id = Uuid::now_v7();
        let cover_media_id = Uuid::now_v7();
        let post = Post::create_draft_with_metadata(
            author(),
            Slug::new("with-metadata").unwrap(),
            "标题".into(),
            None,
            "正文".into(),
            Visibility::Public,
            PostDraftMetadata {
                category_id: Some(category_id),
                series: vec![SeriesPlacement {
                    series_id,
                    position: 1,
                }],
                cover_media_id: Some(cover_media_id),
            },
            OffsetDateTime::now_utc(),
        )
        .unwrap();
        let snapshot = post.snapshot();
        assert_eq!(snapshot.category_id, Some(category_id));
        assert_eq!(
            snapshot.series,
            vec![SeriesPlacement {
                series_id,
                position: 1
            }]
        );
        assert_eq!(snapshot.cover_media_id, Some(cover_media_id));
        assert_eq!(snapshot.version, 1);
    }

    #[test]
    fn create_and_edit_reject_the_same_invalid_series_positions() {
        let series_id = Uuid::now_v7();
        for order in [-1, i32::MIN] {
            let create_error = Post::create_draft_with_metadata(
                author(),
                Slug::new("invalid-series").unwrap(),
                "标题".into(),
                None,
                "正文".into(),
                Visibility::Public,
                PostDraftMetadata {
                    series: vec![SeriesPlacement {
                        series_id,
                        position: order,
                    }],
                    ..Default::default()
                },
                OffsetDateTime::now_utc(),
            )
            .unwrap_err();

            let mut post = draft();
            let before = post.snapshot();
            let edit_error = post
                .edit(PostPatch {
                    title: Some("不得部分保存的新标题".into()),
                    series: Some(vec![SeriesPlacement {
                        series_id,
                        position: order,
                    }]),
                    ..Default::default()
                })
                .unwrap_err();
            assert_eq!(create_error, PostError::InvalidSeriesPosition(order));
            assert_eq!(edit_error, create_error);
            assert_eq!(post.snapshot(), before, "非法系列序号不得部分修改内容");
        }
    }

    #[test]
    fn publish_sets_published_at_once() {
        let mut post = draft();
        assert!(post.publish(OffsetDateTime::now_utc()).unwrap());
        let first = post.snapshot().published_at.unwrap();
        post.withdraw();
        assert!(post.publish(OffsetDateTime::now_utc()).unwrap());
        assert_eq!(
            post.snapshot().published_at,
            Some(first),
            "重新发布保留首次时间"
        );
    }

    #[test]
    fn publish_requires_title_and_content() {
        let mut post = Post::create_draft(
            author(),
            Slug::new("d-2").unwrap(),
            String::new(),
            None,
            "内容".into(),
            Visibility::Public,
            OffsetDateTime::now_utc(),
        )
        .unwrap();
        assert_eq!(
            post.publish(OffsetDateTime::now_utc()).unwrap_err(),
            PostError::EmptyTitleOnPublish
        );
    }

    #[test]
    fn publish_is_idempotent() {
        let mut post = draft();
        assert!(post.publish(OffsetDateTime::now_utc()).unwrap());
        assert!(!post.publish(OffsetDateTime::now_utc()).unwrap());
    }

    #[test]
    fn slug_locks_after_first_publish_even_after_withdraw() {
        let mut post = draft();
        post.publish(OffsetDateTime::now_utc()).unwrap();
        let err = post
            .edit(PostPatch {
                slug: Some("new-slug".into()),
                ..Default::default()
            })
            .unwrap_err();
        assert_eq!(err, PostError::SlugLocked);

        post.withdraw();
        let err = post
            .edit(PostPatch {
                slug: Some("new-slug".into()),
                ..Default::default()
            })
            .unwrap_err();
        assert_eq!(err, PostError::SlugLocked, "撤回后 slug 仍锁定");
    }

    #[test]
    fn slug_can_change_before_first_publish() {
        let mut post = draft();
        assert!(
            post.edit(PostPatch {
                slug: Some("renamed".into()),
                ..Default::default()
            })
            .unwrap()
        );
        assert_eq!(post.slug(), "renamed");
    }

    #[test]
    fn withdraw_is_idempotent_and_keeps_draft() {
        let mut post = draft();
        assert!(!post.withdraw());
        post.publish(OffsetDateTime::now_utc()).unwrap();
        assert!(post.withdraw());
        assert!(!post.withdraw());
        assert_eq!(post.status(), PostStatus::Draft);
    }

    #[test]
    fn trash_and_restore_preserve_archival_and_publication_history() {
        let now = OffsetDateTime::now_utc();
        for original_status in [
            PostStatus::Draft,
            PostStatus::Published,
            PostStatus::Archived,
        ] {
            let mut post = draft();
            post.publish(now).unwrap();
            let mut original = post.snapshot();
            original.status = original_status;
            let mut post = Post::reconstitute(original.clone()).unwrap();

            assert!(post.trash(now));
            assert_eq!(post.snapshot().deleted_at, Some(now));
            assert_eq!(post.status(), original_status);
            assert!(!post.is_publicly_visible(OffsetDateTime::now_utc()));
            let trashed = post.snapshot();
            assert!(!post.trash(now + time::Duration::seconds(1)));
            assert_eq!(post.snapshot(), trashed, "重复删除保留原回收时间");

            assert!(post.restore());
            let restored = post.snapshot();
            assert_eq!(restored.deleted_at, None);
            assert_eq!(restored.status, PostStatus::Draft);
            assert!(
                !post.is_publicly_visible(OffsetDateTime::now_utc()),
                "恢复内容不得自动重新上线"
            );
            assert_eq!(restored.published_at, original.published_at);
            assert_eq!(restored.version, original.version);
            assert_eq!(restored.updated_at, original.updated_at);
            assert!(!post.restore());
            assert_eq!(post.snapshot(), restored);

            if original_status != PostStatus::Archived {
                assert_eq!(
                    post.edit(PostPatch {
                        slug: Some("renamed-after-restore".into()),
                        ..Default::default()
                    })
                    .unwrap_err(),
                    PostError::SlugLocked,
                    "恢复不解除首次发布后对 slug 的锁定"
                );
            }
        }
    }

    #[test]
    fn restoring_live_content_is_a_noop() {
        let mut post = draft();
        post.publish(OffsetDateTime::now_utc()).unwrap();
        let before = post.snapshot();
        assert!(!post.restore());
        assert_eq!(post.snapshot(), before);
        assert!(post.is_publicly_visible(OffsetDateTime::now_utc()));
    }

    #[test]
    fn published_post_cannot_be_emptied() {
        let mut post = draft();
        post.publish(OffsetDateTime::now_utc()).unwrap();
        let err = post
            .edit(PostPatch {
                content: Some(String::new()),
                ..Default::default()
            })
            .unwrap_err();
        assert_eq!(err, PostError::EmptyContentWhenPublished);
    }

    #[test]
    fn edit_reports_whether_changed() {
        let mut post = draft();
        assert!(
            !post
                .edit(PostPatch {
                    title: Some("标题".into()), // 相同值不算变化
                    ..Default::default()
                })
                .unwrap()
        );
        assert!(
            post.edit(PostPatch {
                content: Some("新正文".into()),
                ..Default::default()
            })
            .unwrap()
        );
    }

    #[test]
    fn public_visibility_rules() {
        let mut post = draft();
        assert!(!post.is_publicly_visible(OffsetDateTime::now_utc()));
        post.publish(OffsetDateTime::now_utc()).unwrap();
        assert!(post.is_publicly_visible(OffsetDateTime::now_utc()));
        post.edit(PostPatch {
            visibility: Some(Visibility::Private),
            ..Default::default()
        })
        .unwrap();
        assert!(
            !post.is_publicly_visible(OffsetDateTime::now_utc()),
            "private 不公开"
        );
    }

    #[test]
    fn archived_requires_return_to_draft() {
        // 归档由专门用例驱动；这里验证聚合规则：archived 不能发布、撤回无效、不能编辑。
        let mut post = draft();
        post.publish(OffsetDateTime::now_utc()).unwrap();
        let mut snapshot = post.snapshot();
        snapshot.status = PostStatus::Archived;
        let mut archived = Post::reconstitute(snapshot).unwrap();
        assert_eq!(
            archived.publish(OffsetDateTime::now_utc()).unwrap_err(),
            PostError::ArchivedRequiresDraft
        );
        assert_eq!(
            archived
                .edit(PostPatch {
                    content: Some("改写归档文章".into()),
                    ..Default::default()
                })
                .unwrap_err(),
            PostError::ArchivedNotEditable,
            "归档状态下正文不可改写，须先退回草稿"
        );
        assert!(archived.withdraw());
        assert_eq!(archived.status(), PostStatus::Draft);
    }

    #[test]
    fn edit_failure_leaves_aggregate_untouched() {
        // 回归：任一字段校验失败（此处为非法 slug）时，聚合不得留下部分修改。
        let mut post = draft();
        let before = post.snapshot();

        let err = post
            .edit(PostPatch {
                title: Some("新标题".into()),
                content: Some("新正文".into()),
                slug: Some("bad/slug".into()),
                ..Default::default()
            })
            .unwrap_err();
        assert!(matches!(err, PostError::InvalidSlug(_)));

        let after = post.snapshot();
        assert_eq!(before.title, after.title, "标题未被部分写入");
        assert_eq!(before.content, after.content, "正文未被部分写入");
        assert_eq!(before.slug, after.slug);
    }

    #[test]
    fn published_post_cannot_be_emptied_atomically() {
        let mut post = draft();
        post.publish(OffsetDateTime::now_utc()).unwrap();
        let before = post.snapshot();

        // 同时改标题与清空正文：应整体失败，标题不能被单独写入。
        let err = post
            .edit(PostPatch {
                title: Some("只改标题".into()),
                content: Some(String::new()),
                ..Default::default()
            })
            .unwrap_err();
        assert_eq!(err, PostError::EmptyContentWhenPublished);
        assert_eq!(post.snapshot().title, before.title);
        assert_eq!(post.snapshot().content, before.content);
    }

    #[test]
    fn cover_is_three_state_and_only_a_real_change_bumps_the_aggregate() {
        let mut post = draft();
        let cover = Uuid::now_v7();

        // None = 不修改：默认补丁不应把封面误清空。
        assert!(!post.edit(PostPatch::default()).unwrap());
        assert_eq!(post.snapshot().cover_media_id, None);

        // Some(Some(id)) = 设置；仅封面变化也算变化。
        assert!(
            post.edit(PostPatch {
                cover_media_id: Some(Some(cover)),
                ..Default::default()
            })
            .unwrap()
        );
        assert_eq!(post.snapshot().cover_media_id, Some(cover));

        // 同值幂等。
        assert!(
            !post
                .edit(PostPatch {
                    cover_media_id: Some(Some(cover)),
                    ..Default::default()
                })
                .unwrap()
        );

        // Some(None) = 移除。
        assert!(
            post.edit(PostPatch {
                cover_media_id: Some(None),
                ..Default::default()
            })
            .unwrap()
        );
        assert_eq!(post.snapshot().cover_media_id, None);
    }

    #[test]
    fn trash_blocks_edit_publish_and_withdraw_without_mutating() {
        for published in [false, true] {
            let mut post = draft();
            if published {
                post.publish(OffsetDateTime::UNIX_EPOCH).unwrap();
            }
            post.trash(OffsetDateTime::UNIX_EPOCH);
            let before = post.snapshot();
            assert_eq!(
                post.edit(PostPatch {
                    title: Some("new".into()),
                    ..Default::default()
                }),
                Err(PostError::InTrash)
            );
            assert_eq!(
                post.publish(OffsetDateTime::UNIX_EPOCH),
                Err(PostError::InTrash)
            );
            assert!(!post.withdraw());
            assert_eq!(post.snapshot(), before);
        }
    }

    #[test]
    fn reconstitution_rejects_invalid_slug_series_version_and_published_state() {
        for field in 0..5 {
            let mut snapshot = draft().snapshot();
            match field {
                0 => snapshot.slug = "bad/path".into(),
                1 => {
                    let placement = SeriesPlacement {
                        series_id: Uuid::now_v7(),
                        position: 0,
                    };
                    snapshot.series = vec![placement, placement];
                }
                2 => {
                    snapshot.series = vec![SeriesPlacement {
                        series_id: Uuid::now_v7(),
                        position: -1,
                    }];
                }
                3 => snapshot.version = 0,
                _ => snapshot.status = PostStatus::Published,
            }
            assert!(Post::reconstitute(snapshot).is_err());
        }
    }

    #[test]
    fn oversized_source_is_rejected_atomically() {
        let mut post = draft();
        let before = post.snapshot();
        assert!(
            post.edit(PostPatch {
                content: Some("x".repeat(1_100_000)),
                ..Default::default()
            })
            .is_err()
        );
        assert_eq!(post.snapshot(), before);
    }

    #[test]
    fn legacy_large_content_can_be_loaded_and_shortened_but_not_published() {
        let mut snapshot = draft().snapshot();
        snapshot.content = "x".repeat(1_100_000);
        let mut post = Post::reconstitute(snapshot).unwrap();
        assert!(post.publish(OffsetDateTime::UNIX_EPOCH).is_err());
        post.edit(PostPatch {
            content: Some("shorter".into()),
            ..Default::default()
        })
        .unwrap();
        assert!(post.publish(OffsetDateTime::UNIX_EPOCH).unwrap());
    }
    #[test]
    fn schedule_cancel_archive_and_restore_keep_slug_locked() {
        let mut content = draft();
        let now = OffsetDateTime::UNIX_EPOCH;
        let at = now + time::Duration::hours(1);
        assert_eq!(
            content.schedule(now, now),
            Err(PostError::ScheduleMustBeFuture)
        );
        assert!(content.schedule(at, now).unwrap());
        assert!(!content.schedule(at, now).unwrap());
        assert!(!content.is_publicly_visible(at));
        assert!(
            content
                .edit(PostPatch {
                    content: Some(" ".into()),
                    ..Default::default()
                })
                .is_err()
        );
        assert!(content.withdraw());
        assert_eq!(content.snapshot().published_at, Some(at));
        assert_eq!(
            content.edit(PostPatch {
                slug: Some("renamed".into()),
                ..Default::default()
            }),
            Err(PostError::SlugLocked)
        );
        assert!(content.publish(now).unwrap());
        assert_eq!(
            content.snapshot().published_at,
            Some(now),
            "立即发布修正未来时间"
        );
        assert!(content.is_publicly_visible(now));
        assert!(content.archive().unwrap());
        assert!(!content.is_publicly_visible(now));
        assert!(content.trash(now));
        assert!(content.restore());
        assert_eq!(content.status(), PostStatus::Draft);
        assert_eq!(content.snapshot().published_at, Some(now));
    }
}
