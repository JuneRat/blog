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
    #[error("首次发布后 slug 已锁定，撤回也不允许改名")]
    SlugLocked,
    #[error("发布前标题不能为空")]
    EmptyTitleOnPublish,
    #[error("发布前正文不能为空")]
    EmptyContentOnPublish,
    #[error("已发布文章的标题与正文不能清空")]
    EmptyContentWhenPublished,
    #[error("归档是终态，不能直接重新发布；需要先恢复为草稿的流程另行扩展")]
    ArchivedIsTerminal,
}

/// 单一路径片段 slug：非空、UTF-8 字节数不超过上限，
/// 禁止路径分隔符、点、百分号、空白与控制字符，防止编码与遍历绕过。
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
            if ch.is_control() || ch.is_whitespace() {
                return Err(PostError::InvalidSlug("不能包含空白或控制字符".into()));
            }
            if matches!(ch, '/' | '\\' | '?' | '#' | '%' | '.') {
                return Err(PostError::InvalidSlug(format!("不能包含字符 {ch:?}")));
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
    pub cover: Option<String>,
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
#[derive(Debug, Clone, Default)]
pub struct PostPatch {
    pub slug: Option<String>,
    pub title: Option<String>,
    pub excerpt: Option<String>,
    pub content: Option<String>,
    pub visibility: Option<Visibility>,
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
        if title.chars().count() > TITLE_MAX_CHARS {
            return Err(PostError::TitleTooLong);
        }
        if let Some(excerpt) = excerpt.as_deref() {
            if excerpt.chars().count() > EXCERPT_MAX_CHARS {
                return Err(PostError::ExcerptTooLong);
            }
        }
        Ok(Self {
            snapshot: PostSnapshot {
                id: PostId::generate().0,
                author_id: author.0,
                category_id: None,
                series_id: None,
                title,
                slug: slug.into_string(),
                excerpt,
                content,
                cover: None,
                series_order: None,
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
        if let Some(excerpt) = excerpt {
            if excerpt.chars().count() > EXCERPT_MAX_CHARS {
                return Err(PostError::ExcerptTooLong);
            }
        }
        Ok(())
    }

    /// 编辑当前正文。保存已发布内容会直接反映到线上，因此已发布状态
    /// 不允许把标题/正文清空。返回是否存在实际变化（决定 version 是否 +1）。
    pub fn edit(&mut self, patch: PostPatch) -> Result<bool, PostError> {
        let mut changed = false;

        if let Some(new_title) = patch.title {
            if new_title != self.snapshot.title {
                Self::validate_mutation_fields(&new_title, patch.excerpt.as_deref())?;
                self.snapshot.title = new_title;
                changed = true;
            }
        }
        if let Some(new_excerpt) = patch.excerpt {
            if Some(&new_excerpt) != self.snapshot.excerpt.as_ref() {
                Self::validate_mutation_fields(&self.snapshot.title, Some(&new_excerpt))?;
                self.snapshot.excerpt = if new_excerpt.is_empty() {
                    None
                } else {
                    Some(new_excerpt)
                };
                changed = true;
            }
        }
        if let Some(new_content) = patch.content {
            if new_content != self.snapshot.content {
                self.snapshot.content = new_content;
                changed = true;
            }
        }
        if let Some(new_visibility) = patch.visibility {
            if new_visibility != self.snapshot.visibility {
                self.snapshot.visibility = new_visibility;
                changed = true;
            }
        }
        if let Some(new_slug_raw) = patch.slug {
            if new_slug_raw != self.snapshot.slug {
                // 首次发布产生 published_at 后 slug 锁定；撤回不解锁。
                if self.snapshot.published_at.is_some() {
                    return Err(PostError::SlugLocked);
                }
                let slug = Slug::new(&new_slug_raw)?;
                self.snapshot.slug = slug.into_string();
                changed = true;
            }
        }

        // 已发布内容直接更新线上，标题/正文不可为空。
        if changed && self.snapshot.status == PostStatus::Published {
            if self.snapshot.title.trim().is_empty() {
                return Err(PostError::EmptyContentWhenPublished);
            }
            if self.snapshot.content.trim().is_empty() {
                return Err(PostError::EmptyContentWhenPublished);
            }
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
        assert!(Slug::new("你好-世界").is_ok());
        assert!(Slug::new(&"x".repeat(201)).is_err());
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
    fn publish_sets_published_at_once() {
        let mut post = draft();
        assert!(post.publish(OffsetDateTime::now_utc()).unwrap());
        let first = post.snapshot().published_at.unwrap();
        post.withdraw();
        assert!(post.publish(OffsetDateTime::now_utc()).unwrap());
        assert_eq!(post.snapshot().published_at, Some(first), "重新发布保留首次时间");
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
        assert_eq!(post.publish(OffsetDateTime::now_utc()).unwrap_err(), PostError::EmptyTitleOnPublish);
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
        assert!(post
            .edit(PostPatch {
                slug: Some("renamed".into()),
                ..Default::default()
            })
            .unwrap());
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
        assert!(!post
            .edit(PostPatch {
                title: Some("标题".into()), // 相同值不算变化
                ..Default::default()
            })
            .unwrap());
        assert!(post
            .edit(PostPatch {
                content: Some("新正文".into()),
                ..Default::default()
            })
            .unwrap());
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
        // 归档由专门用例驱动；这里验证聚合规则：archived 不能发布、撤回无效。
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
    }
}
