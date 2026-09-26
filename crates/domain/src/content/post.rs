//! Post 聚合：一份当前正文的状态、转换与不变量。
//!
//! 规则来源 docs/content-lifecycle.md：
//! - 每条记录只有一份当前正文；保存已发布内容直接更新线上。
//! - slug 草稿创建时即唯一；首次发布（published_at 产生）后锁定，撤回也不解锁。
//! - 首次发布校验标题与正文；重新发布保留首次 published_at。
//! - 撤回 published → draft；归档为终态。
//! - version 由仓储按“有实际变化才 +1”递增；聚合只报告是否发生变化。

use time::OffsetDateTime;
use uuid::Uuid;

use crate::identity::UserId;

pub const TITLE_MAX_CHARS: usize = 300;
pub const EXCERPT_MAX_CHARS: usize = 1000;
pub const SLUG_MAX_BYTES: usize = 200;

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
    Published,
    Archived,
}

impl PostStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            PostStatus::Draft => "draft",
            PostStatus::Published => "published",
            PostStatus::Archived => "archived",
        }
    }

    /// 从数据库字符串恢复；未知值视为损坏数据。
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "draft" => Some(PostStatus::Draft),
            "published" => Some(PostStatus::Published),
            "archived" => Some(PostStatus::Archived),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Visibility {
    Public,
    Private,
}

impl Visibility {
    pub fn as_str(self) -> &'static str {
        match self {
            Visibility::Public => "public",
            Visibility::Private => "private",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "public" => Some(Visibility::Public),
            "private" => Some(Visibility::Private),
            _ => None,
        }
    }
}

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum PostError {
    #[error("slug 不合法：{0}")]
    InvalidSlug(String),
    #[error("标题长度不能超过 {TITLE_MAX_CHARS} 字符")]
    TitleTooLong,
    #[error("摘要长度不能超过 {EXCERPT_MAX_CHARS} 字符")]
    ExcerptTooLong,
    #[error("系列序号必须为正整数，收到 {0}")]
    InvalidSeriesOrder(i32),
    #[error("首次发布后 slug 已锁定，撤回也不允许改名")]
    SlugLocked,
    #[error("发布前标题不能为空")]
    EmptyTitleOnPublish,
    #[error("发布前正文不能为空")]
    EmptyContentOnPublish,
    #[error("已发布文章的标题与正文不能清空")]
    EmptyContentWhenPublished,
    #[error("归档是终态，不能重新发布")]
    ArchivedIsTerminal,
    #[error("归档是终态，不能编辑；需要恢复为草稿的流程另行扩展")]
    ArchivedNotEditable,
}

/// 单一路径片段 slug：非空、UTF-8 字节数不超过上限。
/// 字符集固定为 Unicode 字母数字加 `-`、`_`；禁止其他 ASCII 符号与空白，
/// 防止路径分隔、编码与模板输出层面的绕过。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Slug(String);

impl Slug {
    pub fn new(raw: &str) -> Result<Self, PostError> {
        if raw.is_empty() {
            return Err(PostError::InvalidSlug("不能为空".into()));
        }
        if raw.len() > SLUG_MAX_BYTES {
            return Err(PostError::InvalidSlug(format!(
                "超过 {} 字节上限",
                SLUG_MAX_BYTES
            )));
        }
        for ch in raw.chars() {
            let allowed = ch.is_alphanumeric() || ch == '-' || ch == '_';
            if !allowed {
                return Err(PostError::InvalidSlug(format!(
                    "只允许 Unicode 字母数字、-、_，包含非法字符 {ch:?}"
                )));
            }
        }
        Ok(Self(raw.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn into_string(self) -> String {
        self.0
    }
}

/// 系列中的有效位置。系列 ID 与序号一起设置，序号始终为正整数。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SeriesPlacement {
    series_id: Uuid,
    order: i32,
}

impl SeriesPlacement {
    pub fn new(series_id: Uuid, order: i32) -> Result<Self, PostError> {
        if order <= 0 {
            return Err(PostError::InvalidSeriesOrder(order));
        }
        Ok(Self { series_id, order })
    }

    pub fn series_id(self) -> Uuid {
        self.series_id
    }

    pub fn order(self) -> i32 {
        self.order
    }
}

/// 创建草稿时一并设置的关联信息；存在性由应用和提交事务校验。
/// 系列序号由聚合使用与编辑相同的规则验证。
#[derive(Debug, Clone, Default)]
pub struct PostDraftMetadata {
    pub category_id: Option<Uuid>,
    pub series: Option<(Uuid, i32)>,
    pub cover_media_id: Option<Uuid>,
}

/// 仓储重建聚合的受控快照载体。
#[derive(Debug, Clone, PartialEq)]
pub struct PostSnapshot {
    pub id: Uuid,
    pub author_id: Uuid,
    pub category_id: Option<Uuid>,
    pub series_id: Option<Uuid>,
    pub title: String,
    pub slug: String,
    pub excerpt: Option<String>,
    pub content: String,
    /// 封面所引用的媒体资产（None = 无封面）。
    ///
    /// 与正文引用同源：保存时把 `{封面} ∪ 正文图片` 写进 `content_media_refs`，
    /// 因此「仍被引用不能删除」与「匿名访问跟随内容公开状态」对封面同样成立。
    pub cover_media_id: Option<Uuid>,
    pub series_order: Option<i32>,
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
    /// 系列归属三态：None 不修改；Some(None) 退出系列；Some(Some((id, order)))
    /// 设置系列与序号（order 必须为正整数；同空同非空由类型形状保证）。
    pub series: Option<Option<(Uuid, i32)>>,
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
        Self::validate_mutation_fields(&title, excerpt.as_deref())?;
        let series = metadata
            .series
            .map(|(id, order)| SeriesPlacement::new(id, order))
            .transpose()?;
        Ok(Self {
            snapshot: PostSnapshot {
                id: PostId::generate().0,
                author_id: author.0,
                category_id: metadata.category_id,
                series_id: series.map(SeriesPlacement::series_id),
                title,
                slug: slug.into_string(),
                excerpt,
                content,
                cover_media_id: metadata.cover_media_id,
                series_order: series.map(SeriesPlacement::order),
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
    pub fn reconstitute(snapshot: PostSnapshot) -> Self {
        Self { snapshot }
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
    pub fn is_publicly_visible(&self) -> bool {
        self.snapshot.status == PostStatus::Published
            && self.snapshot.visibility == Visibility::Public
            && self.snapshot.deleted_at.is_none()
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
        let new_series = match patch.series {
            None => (self.snapshot.series_id, self.snapshot.series_order),
            Some(None) => (None, None),
            Some(Some((series_id, order))) => {
                let placement = SeriesPlacement::new(series_id, order)?;
                (Some(placement.series_id()), Some(placement.order()))
            }
        };
        let slug_changed = match patch.slug.as_deref() {
            Some(new_slug) => new_slug != self.snapshot.slug,
            None => false,
        };
        let new_cover_media_id = patch.cover_media_id.unwrap_or(self.snapshot.cover_media_id);

        // 2. 全量校验候选值（不写任何字段）。
        Self::validate_mutation_fields(&new_title, new_excerpt_owned.as_deref())?;
        if slug_changed {
            // 首次发布产生 published_at 后 slug 锁定；撤回不解锁。
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
        if self.snapshot.status == PostStatus::Published {
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
        if new_series != (self.snapshot.series_id, self.snapshot.series_order) {
            self.snapshot.series_id = new_series.0;
            self.snapshot.series_order = new_series.1;
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

    /// 发布：draft → published（首次发布写入 published_at，之后保持不变）。
    /// 已发布时幂等无操作。归档为终态。
    pub fn publish(&mut self, now: OffsetDateTime) -> Result<bool, PostError> {
        match self.snapshot.status {
            PostStatus::Published => Ok(false),
            PostStatus::Archived => Err(PostError::ArchivedIsTerminal),
            PostStatus::Draft => {
                if self.snapshot.title.trim().is_empty() {
                    return Err(PostError::EmptyTitleOnPublish);
                }
                if self.snapshot.content.trim().is_empty() {
                    return Err(PostError::EmptyContentOnPublish);
                }
                self.snapshot.status = PostStatus::Published;
                if self.snapshot.published_at.is_none() {
                    self.snapshot.published_at = Some(now);
                }
                Ok(true)
            }
        }
    }

    /// 撤回：published → draft，保留 published_at（slug 仍锁定）。
    /// 非发布状态幂等无操作。
    pub fn withdraw(&mut self) -> bool {
        if self.snapshot.status == PostStatus::Published {
            self.snapshot.status = PostStatus::Draft;
            true
        } else {
            false
        }
    }

    /// 移入回收站，保留原状态与首次发布时间。重复操作幂等。
    /// version 与 updated_at 由提交边界统一处理。
    pub fn trash(&mut self, now: OffsetDateTime) -> bool {
        if self.snapshot.deleted_at.is_some() {
            return false;
        }
        self.snapshot.deleted_at = Some(now);
        true
    }

    /// 从回收站恢复：归档保持终态，其余内容恢复为草稿，避免意外重新上线。
    /// 保留首次发布时间（slug 仍锁定）；非回收站内容幂等无操作。
    /// version 与 updated_at 由提交边界统一处理。
    pub fn restore(&mut self) -> bool {
        if self.snapshot.deleted_at.is_none() {
            return false;
        }
        self.snapshot.deleted_at = None;
        if self.snapshot.status != PostStatus::Archived {
            self.snapshot.status = PostStatus::Draft;
        }
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
    fn slug_rejects_path_characters() {
        assert!(Slug::new("a/b").is_err());
        assert!(Slug::new("a.b").is_err());
        assert!(Slug::new("a%2Fb").is_err());
        assert!(Slug::new("a b").is_err());
        assert!(Slug::new("").is_err());
        assert!(Slug::new("a&b").is_err(), "符号 & 不再允许");
        assert!(Slug::new("a+b").is_err(), "符号 + 不再允许");
        assert!(Slug::new("a:b").is_err(), "符号 : 不再允许");
        assert!(Slug::new(&"x".repeat(201)).is_err());
        assert!(Slug::new("你好-世界").is_ok());
        assert!(Slug::new("Hello_World-01").is_ok());
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
        assert!(!post.is_publicly_visible());
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
                series: Some((series_id, 1)),
                cover_media_id: Some(cover_media_id),
            },
            OffsetDateTime::now_utc(),
        )
        .unwrap();
        let snapshot = post.snapshot();
        assert_eq!(snapshot.category_id, Some(category_id));
        assert_eq!(snapshot.series_id, Some(series_id));
        assert_eq!(snapshot.series_order, Some(1));
        assert_eq!(snapshot.cover_media_id, Some(cover_media_id));
        assert_eq!(snapshot.version, 1);
    }

    #[test]
    fn create_and_edit_reject_the_same_invalid_series_positions() {
        let series_id = Uuid::now_v7();
        for order in [0, -1, i32::MIN] {
            let create_error = Post::create_draft_with_metadata(
                author(),
                Slug::new("invalid-series").unwrap(),
                "标题".into(),
                None,
                "正文".into(),
                Visibility::Public,
                PostDraftMetadata {
                    series: Some((series_id, order)),
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
                    series: Some(Some((series_id, order))),
                    ..Default::default()
                })
                .unwrap_err();
            assert_eq!(create_error, PostError::InvalidSeriesOrder(order));
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
            let mut post = Post::reconstitute(original.clone());

            assert!(post.trash(now));
            assert_eq!(post.snapshot().deleted_at, Some(now));
            assert_eq!(post.status(), original_status);
            assert!(!post.is_publicly_visible());
            let trashed = post.snapshot();
            assert!(!post.trash(now + time::Duration::seconds(1)));
            assert_eq!(post.snapshot(), trashed, "重复删除保留原回收时间");

            assert!(post.restore());
            let restored = post.snapshot();
            assert_eq!(restored.deleted_at, None);
            assert_eq!(
                restored.status,
                if original_status == PostStatus::Archived {
                    PostStatus::Archived
                } else {
                    PostStatus::Draft
                }
            );
            assert!(!post.is_publicly_visible(), "恢复内容不得自动重新上线");
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
        assert!(post.is_publicly_visible());
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
        assert!(!post.is_publicly_visible());
        post.publish(OffsetDateTime::now_utc()).unwrap();
        assert!(post.is_publicly_visible());
        post.edit(PostPatch {
            visibility: Some(Visibility::Private),
            ..Default::default()
        })
        .unwrap();
        assert!(!post.is_publicly_visible(), "private 不公开");
    }

    #[test]
    fn archived_is_terminal() {
        // 归档由专门用例驱动；这里验证聚合规则：archived 不能发布、撤回无效、不能编辑。
        let mut post = draft();
        post.publish(OffsetDateTime::now_utc()).unwrap();
        let mut snapshot = post.snapshot();
        snapshot.status = PostStatus::Archived;
        let mut archived = Post::reconstitute(snapshot);
        assert_eq!(
            archived.publish(OffsetDateTime::now_utc()).unwrap_err(),
            PostError::ArchivedIsTerminal
        );
        assert!(!archived.withdraw());
        assert_eq!(
            archived
                .edit(PostPatch {
                    content: Some("改写归档文章".into()),
                    ..Default::default()
                })
                .unwrap_err(),
            PostError::ArchivedNotEditable,
            "归档是终态，正文不可改写"
        );
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
}
