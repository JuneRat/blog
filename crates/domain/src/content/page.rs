//! Page 聚合：无作者、无分类/标签/系列的独立页面（/about、/friends 等）。
//!
//! 规则来源 docs/content-lifecycle.md：
//! - 与 Post 相同的一份当前正文、published_at 后锁定 slug、归档终态；
//! - 但 Page **没有 author_id**，权限是站点范围（page.*），不套用 own/any；
//! - 公开地址是根路径 `/{slug}`，因此必须避开系统保留路径；
//! - schema 没有 deleted_at：删除是物理删除（本轮不做删除用例）。

use time::OffsetDateTime;
use uuid::Uuid;

use super::post::PostError;
pub use super::post::{Slug, TITLE_MAX_CHARS, Visibility};

/// 根路径下被系统占用的保留 slug（与公开路由注册表保持同步）。
///
/// 实际路由见 `interfaces::http::public_router`：`/`、`/posts/{slug}`、`/healthz`、
/// `/assets`、`/admin`、`/api`、`/auth`；其余为 docs/content-lifecycle.md §4
/// 明确保留的后续路由（分类/标签/系列、媒体、RSS、sitemap、robots、图标）。
/// Page 不得占用，创建、改名与发布都会复核；固定路由优先，Page 最后匹配。
pub const RESERVED_ROOT_SLUGS: &[&str] = &[
    "admin",
    "api",
    "auth",
    "posts",
    "categories",
    "tags",
    "series",
    "assets",
    "media",
    "rss",
    "feed",
    "atom",
    "sitemap",
    "robots",
    "favicon",
    "icon",
    "apple-touch-icon",
    "healthz",
];

/// slug 是否与系统保留路径冲突。保留名全为 ASCII，按 ASCII 大小写不敏感比较，
/// 避免 `Admin` 这类视觉混淆地址。
pub fn is_reserved_root_slug(slug: &str) -> bool {
    RESERVED_ROOT_SLUGS
        .iter()
        .any(|reserved| slug.eq_ignore_ascii_case(reserved))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PageId(pub Uuid);

impl PageId {
    pub fn generate() -> Self {
        Self(Uuid::now_v7())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PageStatus {
    Draft,
    Published,
    Archived,
}

impl PageStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            PageStatus::Draft => "draft",
            PageStatus::Published => "published",
            PageStatus::Archived => "archived",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "draft" => Some(PageStatus::Draft),
            "published" => Some(PageStatus::Published),
            "archived" => Some(PageStatus::Archived),
            _ => None,
        }
    }
}

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum PageError {
    #[error("slug 不合法：{0}")]
    InvalidSlug(String),
    #[error("slug「{0}」是系统保留路径，不能用于页面")]
    ReservedSlug(String),
    #[error("标题长度不能超过 {TITLE_MAX_CHARS} 字符")]
    TitleTooLong,
    #[error("首次发布后 slug 已锁定，撤回也不允许改名")]
    SlugLocked,
    #[error("发布前标题不能为空")]
    EmptyTitleOnPublish,
    #[error("发布前正文不能为空")]
    EmptyContentOnPublish,
    #[error("已发布页面的标题与正文不能清空")]
    EmptyContentWhenPublished,
    #[error("归档是终态，不能重新发布")]
    ArchivedIsTerminal,
    #[error("归档是终态，不能编辑；需要恢复为草稿的流程另行扩展")]
    ArchivedNotEditable,
}

/// 仓储重建聚合的受控快照载体。Page 无作者、摘要、封面与软删除字段。
#[derive(Debug, Clone, PartialEq)]
pub struct PageSnapshot {
    pub id: Uuid,
    pub title: String,
    pub slug: String,
    pub content: String,
    pub status: PageStatus,
    pub visibility: Visibility,
    pub published_at: Option<OffsetDateTime>,
    pub version: i64,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

/// 编辑补丁：None 表示不修改该字段。
#[derive(Debug, Clone, Default)]
pub struct PagePatch {
    pub slug: Option<String>,
    pub title: Option<String>,
    pub content: Option<String>,
    pub visibility: Option<Visibility>,
}

/// Page 聚合。字段私有，状态转换只经由行为方法。
#[derive(Debug, Clone)]
pub struct Page {
    snapshot: PageSnapshot,
}

fn map_slug_error(error: PostError) -> PageError {
    match error {
        PostError::InvalidSlug(message) => PageError::InvalidSlug(message),
        other => PageError::InvalidSlug(other.to_string()),
    }
}

impl Page {
    /// 创建草稿：标题与正文可为空，slug 必须已验证且不属于保留路径。
    pub fn create_draft(
        slug: Slug,
        title: String,
        content: String,
        visibility: Visibility,
        now: OffsetDateTime,
    ) -> Result<Self, PageError> {
        if title.chars().count() > TITLE_MAX_CHARS {
            return Err(PageError::TitleTooLong);
        }
        if is_reserved_root_slug(slug.as_str()) {
            return Err(PageError::ReservedSlug(slug.into_string()));
        }
        Ok(Self {
            snapshot: PageSnapshot {
                id: PageId::generate().0,
                title,
                slug: slug.into_string(),
                content,
                status: PageStatus::Draft,
                visibility,
                published_at: None,
                version: 1,
                created_at: now,
                updated_at: now,
            },
        })
    }

    /// 受控重建入口：仅供持久化适配器从数据库恢复聚合。
    pub fn reconstitute(snapshot: PageSnapshot) -> Self {
        Self { snapshot }
    }

    pub fn snapshot(&self) -> PageSnapshot {
        self.snapshot.clone()
    }

    pub fn id(&self) -> PageId {
        PageId(self.snapshot.id)
    }

    pub fn slug(&self) -> &str {
        &self.snapshot.slug
    }

    pub fn status(&self) -> PageStatus {
        self.snapshot.status
    }

    pub fn version(&self) -> i64 {
        self.snapshot.version
    }

    /// 匿名公开条件：published + public（Page 无软删除）。
    pub fn is_publicly_visible(&self) -> bool {
        self.snapshot.status == PageStatus::Published
            && self.snapshot.visibility == Visibility::Public
    }

    /// 编辑当前正文。与 Post 相同的原子提交语义：先校验候选值，再整体写入，
    /// 任一校验失败聚合保持原状。返回是否存在实际变化。
    pub fn edit(&mut self, patch: PagePatch) -> Result<bool, PageError> {
        if self.snapshot.status == PageStatus::Archived {
            return Err(PageError::ArchivedNotEditable);
        }

        // 1. 候选值（未提供的字段保持当前值）。
        let new_title = patch.title.unwrap_or_else(|| self.snapshot.title.clone());
        let new_content = patch
            .content
            .unwrap_or_else(|| self.snapshot.content.clone());
        let new_visibility = patch.visibility.unwrap_or(self.snapshot.visibility);
        let slug_changed = match patch.slug.as_deref() {
            Some(new_slug) => new_slug != self.snapshot.slug,
            None => false,
        };

        // 2. 全量校验候选值（不写任何字段）。
        if new_title.chars().count() > TITLE_MAX_CHARS {
            return Err(PageError::TitleTooLong);
        }
        if slug_changed {
            if self.snapshot.published_at.is_some() {
                return Err(PageError::SlugLocked);
            }
            let raw = patch.slug.as_deref().expect("slug_changed 蕴含存在");
            Slug::new(raw).map_err(map_slug_error)?;
            if is_reserved_root_slug(raw) {
                return Err(PageError::ReservedSlug(raw.to_string()));
            }
        }
        if self.snapshot.status == PageStatus::Published
            && (new_title.trim().is_empty() || new_content.trim().is_empty())
        {
            return Err(PageError::EmptyContentWhenPublished);
        }

        // 3. 整体提交。
        let mut changed = false;
        if new_title != self.snapshot.title {
            self.snapshot.title = new_title;
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
        if slug_changed {
            self.snapshot.slug = patch.slug.expect("slug_changed 蕴含存在");
            changed = true;
        }
        Ok(changed)
    }

    /// 发布：draft → published（首次写入 published_at）；已发布幂等；归档终态。
    /// 发布时复核保留路径，避免历史数据或后续改名引入的系统路由占用。
    pub fn publish(&mut self, now: OffsetDateTime) -> Result<bool, PageError> {
        match self.snapshot.status {
            PageStatus::Published => Ok(false),
            PageStatus::Archived => Err(PageError::ArchivedIsTerminal),
            PageStatus::Draft => {
                if self.snapshot.title.trim().is_empty() {
                    return Err(PageError::EmptyTitleOnPublish);
                }
                if self.snapshot.content.trim().is_empty() {
                    return Err(PageError::EmptyContentOnPublish);
                }
                if is_reserved_root_slug(&self.snapshot.slug) {
                    return Err(PageError::ReservedSlug(self.snapshot.slug.clone()));
                }
                self.snapshot.status = PageStatus::Published;
                if self.snapshot.published_at.is_none() {
                    self.snapshot.published_at = Some(now);
                }
                Ok(true)
            }
        }
    }

    /// 撤回：published → draft，保留 published_at（slug 仍锁定）。非发布幂等。
    pub fn withdraw(&mut self) -> bool {
        if self.snapshot.status == PageStatus::Published {
            self.snapshot.status = PageStatus::Draft;
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::datetime;

    fn now() -> OffsetDateTime {
        datetime!(2026-09-21 12:00:00 UTC)
    }

    fn draft(slug: &str) -> Page {
        Page::create_draft(
            Slug::new(slug).unwrap(),
            "关于".into(),
            "# 关于\n正文".into(),
            Visibility::Public,
            now(),
        )
        .unwrap()
    }

    #[test]
    fn reserved_root_slugs_are_rejected() {
        for reserved in ["admin", "API", "Posts", "healthz", "assets", "sitemap"] {
            let result = Page::create_draft(
                Slug::new(reserved).unwrap(),
                "标题".into(),
                "正文".into(),
                Visibility::Public,
                now(),
            );
            assert!(
                matches!(result, Err(PageError::ReservedSlug(_))),
                "{reserved} 必须被拒绝：{result:?}"
            );
        }
        // 普通 slug 不受影响。
        assert!(
            Page::create_draft(
                Slug::new("about").unwrap(),
                "标题".into(),
                "正文".into(),
                Visibility::Public,
                now(),
            )
            .is_ok()
        );
        assert!(is_reserved_root_slug("HEALTHZ"));
        assert!(!is_reserved_root_slug("about"));
    }

    #[test]
    fn rename_to_reserved_slug_is_rejected_and_atomic() {
        let mut page = draft("about");
        let err = page
            .edit(PagePatch {
                slug: Some("api".into()),
                title: Some("新标题".into()),
                ..Default::default()
            })
            .unwrap_err();
        assert_eq!(err, PageError::ReservedSlug("api".into()));
        assert_eq!(page.snapshot().title, "关于", "失败时不得部分写入");
        assert_eq!(page.slug(), "about");
    }

    #[test]
    fn publish_requires_title_and_content() {
        let mut page = Page::create_draft(
            Slug::new("empty-title").unwrap(),
            String::new(),
            "正文".into(),
            Visibility::Public,
            now(),
        )
        .unwrap();
        assert_eq!(
            page.publish(now()).unwrap_err(),
            PageError::EmptyTitleOnPublish
        );
    }

    #[test]
    fn publish_sets_published_at_once_and_withdraw_is_idempotent() {
        let mut page = draft("about");
        assert!(page.publish(now()).unwrap());
        let first = page.snapshot().published_at.unwrap();
        assert!(!page.publish(now()).unwrap(), "重复发布幂等");
        assert!(page.withdraw());
        assert!(!page.withdraw());
        assert!(page.publish(now()).unwrap());
        assert_eq!(page.snapshot().published_at, Some(first));
    }

    #[test]
    fn slug_locks_after_first_publish_even_after_withdraw() {
        let mut page = draft("about");
        page.publish(now()).unwrap();
        page.withdraw();
        let err = page
            .edit(PagePatch {
                slug: Some("contact".into()),
                ..Default::default()
            })
            .unwrap_err();
        assert_eq!(err, PageError::SlugLocked);
    }

    #[test]
    fn published_page_cannot_be_emptied() {
        let mut page = draft("about");
        page.publish(now()).unwrap();
        let err = page
            .edit(PagePatch {
                content: Some(String::new()),
                ..Default::default()
            })
            .unwrap_err();
        assert_eq!(err, PageError::EmptyContentWhenPublished);
    }

    #[test]
    fn public_visibility_rules() {
        let mut page = draft("about");
        assert!(!page.is_publicly_visible());
        page.publish(now()).unwrap();
        assert!(page.is_publicly_visible());
        page.edit(PagePatch {
            visibility: Some(Visibility::Private),
            ..Default::default()
        })
        .unwrap();
        assert!(!page.is_publicly_visible());
    }

    #[test]
    fn archived_is_terminal() {
        let mut snapshot = draft("about").snapshot();
        snapshot.status = PageStatus::Archived;
        let mut archived = Page::reconstitute(snapshot);
        assert_eq!(
            archived.publish(now()).unwrap_err(),
            PageError::ArchivedIsTerminal
        );
        assert!(!archived.withdraw());
        assert_eq!(
            archived
                .edit(PagePatch {
                    content: Some("改写".into()),
                    ..Default::default()
                })
                .unwrap_err(),
            PageError::ArchivedNotEditable
        );
    }
}
