//! 页面用例测试：站点级 page.* 授权、保留路径、版本冲突与 slug 锁定。
//! 用内存 fake 仓储，不依赖生产 infrastructure。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use application::error::{ConflictKind, UseCaseError};
use application::identity::{Actor, ActorChannel};
use application::page::{CreatePageCmd, DeletePageCmd, EditPageCmd, PageInteractor};
use application::ports::{Clock, PageCommitOutcome, PageDeleteOutcome, PageRepository};
use domain::content::page::{PageSnapshot, PageStatus, Visibility};
use domain::identity::{PermissionSet, UserId};
use time::OffsetDateTime;
use uuid::Uuid;

struct FixedClock;

impl Clock for FixedClock {
    fn now(&self) -> OffsetDateTime {
        time::macros::datetime!(2026-09-21 12:00:00 UTC)
    }
}

#[derive(Default)]
struct FakePageRepo {
    pages: Mutex<HashMap<String, PageSnapshot>>,
}

#[async_trait::async_trait]
impl PageRepository for FakePageRepo {
    async fn find_by_id(&self, id: Uuid) -> Result<Option<PageSnapshot>, UseCaseError> {
        Ok(self
            .pages
            .lock()
            .unwrap()
            .values()
            .find(|p| p.id == id)
            .cloned())
    }

    async fn list(&self) -> Result<Vec<PageSnapshot>, UseCaseError> {
        let mut all: Vec<PageSnapshot> = self.pages.lock().unwrap().values().cloned().collect();
        all.sort_by_key(|a| std::cmp::Reverse(a.updated_at));
        Ok(all)
    }

    async fn insert_page(
        &self,
        page: &domain::content::page::Page,
    ) -> Result<PageSnapshot, UseCaseError> {
        let snapshot = page.snapshot();
        let mut pages = self.pages.lock().unwrap();
        if pages.contains_key(&snapshot.slug) {
            return Err(UseCaseError::Conflict(ConflictKind::Slug));
        }
        pages.insert(snapshot.slug.clone(), snapshot.clone());
        Ok(snapshot)
    }

    async fn commit_page(
        &self,
        page: &domain::content::page::Page,
        expected_version: i64,
        now: OffsetDateTime,
    ) -> Result<PageCommitOutcome, UseCaseError> {
        let snapshot = page.snapshot();
        let mut pages = self.pages.lock().unwrap();
        let Some(existing) = pages.values().find(|p| p.id == snapshot.id).cloned() else {
            return Ok(PageCommitOutcome::Gone);
        };
        if existing.version != expected_version {
            return Ok(PageCommitOutcome::StaleConflict);
        }
        pages.remove(&existing.slug);
        let mut next = snapshot.clone();
        next.version = existing.version + 1;
        next.updated_at = now;
        pages.insert(next.slug.clone(), next.clone());
        Ok(PageCommitOutcome::Saved(next))
    }

    async fn delete(
        &self,
        id: Uuid,
        expected_version: i64,
    ) -> Result<PageDeleteOutcome, UseCaseError> {
        let mut pages = self.pages.lock().unwrap();
        let Some(existing) = pages.values().find(|p| p.id == id).cloned() else {
            return Ok(PageDeleteOutcome::Gone);
        };
        if existing.version != expected_version {
            return Ok(PageDeleteOutcome::StaleVersion);
        }
        pages.remove(&existing.slug);
        Ok(PageDeleteOutcome::Deleted)
    }
}

fn actor_with(keys: &[&'static str]) -> Actor {
    Actor::new(
        UserId::generate(),
        ActorChannel::Session,
        PermissionSet::from_keys(keys.iter().copied()),
    )
}

const EDITOR: &[&str] = &[
    "page.read",
    "page.create",
    "page.update",
    "page.publish",
    "page.unpublish",
];

fn interactor() -> PageInteractor {
    PageInteractor::new(Arc::new(FakePageRepo::default()), Arc::new(FixedClock))
}

fn cmd(slug: Option<&str>, title: &str) -> CreatePageCmd {
    CreatePageCmd {
        slug: slug.map(str::to_string),
        title: title.into(),
        content: format!("# {title}\n正文"),
        visibility: Visibility::Public,
    }
}

#[tokio::test]
async fn create_requires_page_create_and_can_generate_slug() {
    let pages = interactor();
    let editor = actor_with(EDITOR);
    let outsider = actor_with(&["post.create"]);

    let err = pages
        .create(&outsider, cmd(Some("about"), "关于"))
        .await
        .unwrap_err();
    assert!(matches!(err, UseCaseError::Forbidden), "{err:?}");

    // 缺省 slug 由应用生成，且必须是合法单段（page- 前缀避开保留路径）。
    let created = pages.create(&editor, cmd(None, "关于")).await.unwrap();
    assert!(created.slug.starts_with("page-"), "{}", created.slug);
    assert_eq!(created.status, "draft");
    assert_eq!(created.version, 1);
}

#[tokio::test]
async fn reserved_root_slug_is_rejected_on_create_and_rename() {
    let pages = interactor();
    let editor = actor_with(EDITOR);

    for reserved in ["admin", "api", "auth", "posts", "healthz", "assets"] {
        let err = pages
            .create(&editor, cmd(Some(reserved), "保留路径"))
            .await
            .unwrap_err();
        assert!(
            matches!(err, UseCaseError::Invalid(_)),
            "{reserved} 必须被拒绝：{err:?}"
        );
    }

    let created = pages
        .create(&editor, cmd(Some("about"), "关于"))
        .await
        .unwrap();
    let err = pages
        .edit(
            &editor,
            EditPageCmd {
                id: created.id,
                new_slug: Some("healthz".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap_err();
    assert!(matches!(err, UseCaseError::Invalid(_)), "{err:?}");
    // 失败不得部分写入。
    let shown = pages.find(&editor, created.id).await.unwrap();
    assert_eq!(shown.slug, "about");
}

#[tokio::test]
async fn read_and_list_require_page_read() {
    let pages = interactor();
    let editor = actor_with(EDITOR);
    let reader = actor_with(&["page.read"]);
    let outsider = actor_with(&["post.read"]);

    let created = pages
        .create(&editor, cmd(Some("about"), "关于"))
        .await
        .unwrap();
    pages
        .create(&editor, cmd(Some("contact"), "联系"))
        .await
        .unwrap();

    assert!(pages.find(&reader, created.id).await.is_ok());
    assert!(matches!(
        pages.find(&outsider, created.id).await.unwrap_err(),
        UseCaseError::Forbidden
    ));

    let list = pages.list(&reader).await.unwrap();
    assert_eq!(list.len(), 2, "站点级列表返回全部页面");
    assert!(matches!(
        pages.list(&outsider).await.unwrap_err(),
        UseCaseError::Forbidden
    ));
}

#[tokio::test]
async fn physical_delete_requires_permission_and_exact_identity_and_version() {
    let pages = interactor();
    let editor = actor_with(EDITOR);
    let deleter = actor_with(&["page.delete"]);
    let without_delete = actor_with(&["page.read", "page.update"]);
    let created = pages
        .create(&editor, cmd(Some("about"), "关于"))
        .await
        .unwrap();
    let command = || DeletePageCmd {
        id: created.id,
        expected_version: created.version,
    };
    assert!(matches!(
        pages.delete(&without_delete, command()).await,
        Err(UseCaseError::Forbidden)
    ));
    assert!(matches!(
        pages
            .delete(
                &deleter,
                DeletePageCmd {
                    id: Uuid::now_v7(),
                    ..command()
                }
            )
            .await,
        Err(UseCaseError::NotFound(_))
    ));
    assert!(matches!(
        pages
            .delete(
                &deleter,
                DeletePageCmd {
                    expected_version: 99,
                    ..command()
                }
            )
            .await,
        Err(UseCaseError::VersionConflict)
    ));
    assert!(pages.find(&editor, created.id).await.is_ok());
    pages.delete(&deleter, command()).await.unwrap();
    assert!(matches!(
        pages.delete(&deleter, command()).await,
        Err(UseCaseError::NotFound(_))
    ));
    let replacement = pages
        .create(&editor, cmd(Some("about"), "新页面"))
        .await
        .unwrap();
    assert_ne!(replacement.id, created.id);
    assert!(matches!(
        pages.delete(&deleter, command()).await,
        Err(UseCaseError::NotFound(_))
    ));
    assert_eq!(
        pages.find(&editor, replacement.id).await.unwrap().id,
        replacement.id
    );
}

#[tokio::test]
async fn stale_version_is_a_version_conflict_and_slug_locks_after_publish() {
    let pages = interactor();
    let editor = actor_with(EDITOR);
    let created = pages
        .create(&editor, cmd(Some("about"), "关于"))
        .await
        .unwrap();

    // 正确版本：+1。
    let saved = pages
        .edit(
            &editor,
            EditPageCmd {
                id: created.id,
                title: Some("关于我们".into()),
                expected_version: Some(1),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(saved.version, 2);

    // 过期版本：版本冲突（与 slug 占用的 conflict 区分）。
    let err = pages
        .edit(
            &editor,
            EditPageCmd {
                id: created.id,
                title: Some("基于旧版本".into()),
                expected_version: Some(1),
                ..Default::default()
            },
        )
        .await
        .unwrap_err();
    assert!(matches!(err, UseCaseError::VersionConflict), "{err:?}");

    // 发布后 slug 锁定，撤回也不解锁。
    pages.publish(&editor, created.id, None).await.unwrap();
    let err = pages
        .edit(
            &editor,
            EditPageCmd {
                id: created.id,
                new_slug: Some("contact".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap_err();
    assert!(matches!(err, UseCaseError::Invalid(_)), "{err:?}");
    pages.withdraw(&editor, created.id, None).await.unwrap();
    let err = pages
        .edit(
            &editor,
            EditPageCmd {
                id: created.id,
                new_slug: Some("contact".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap_err();
    assert!(matches!(err, UseCaseError::Invalid(_)), "撤回后仍锁定");
}

#[tokio::test]
async fn publish_requires_permission_and_content_then_withdraws_to_draft() {
    let pages = interactor();
    let editor = actor_with(EDITOR);
    let updater = actor_with(&["page.read", "page.create", "page.update"]);

    let created = pages
        .create(
            &editor,
            CreatePageCmd {
                slug: Some("blank".into()),
                title: "有标题但无正文".into(),
                content: String::new(),
                visibility: Visibility::Public,
            },
        )
        .await
        .unwrap();
    // 正文为空：发布被拒（domain 规则映射为 Invalid）。
    let err = pages.publish(&editor, created.id, None).await.unwrap_err();
    assert!(matches!(err, UseCaseError::Invalid(_)), "{err:?}");

    // 有 update 但没有 publish：仍然被拒。
    let err = pages.publish(&updater, created.id, None).await.unwrap_err();
    assert!(matches!(err, UseCaseError::Forbidden), "{err:?}");

    pages
        .edit(
            &editor,
            EditPageCmd {
                id: created.id,
                content: Some("正文".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let published = pages.publish(&editor, created.id, None).await.unwrap();
    assert_eq!(published.status, "published");
    assert!(published.published_at.is_some());
    // 幂等：重复发布不递增版本。
    let again = pages.publish(&editor, created.id, None).await.unwrap();
    assert_eq!(again.version, published.version);

    let withdrawn = pages.withdraw(&editor, created.id, None).await.unwrap();
    assert_eq!(withdrawn.status, "draft");
    assert!(withdrawn.published_at.is_some(), "保留首次发布时间");
}

#[tokio::test]
async fn page_status_and_visibility_round_trip_through_repository() {
    let pages = interactor();
    let editor = actor_with(EDITOR);
    let created = pages
        .create(&editor, cmd(Some("about"), "关于"))
        .await
        .unwrap();

    // 通过 find 重建聚合后仍能识别为草稿且不可公开。
    let found = pages.find(&editor, created.id).await.unwrap();
    assert_eq!(found.status, PageStatus::Draft.as_str());

    pages.publish(&editor, created.id, None).await.unwrap();
    let public = pages.find(&editor, created.id).await.unwrap();
    assert_eq!(public.status, "published");
    assert_eq!(public.visibility, "public");
}

/// 幂等的发布/撤回也必须校验调用方声明的版本前提。
///
/// 回归：早先 no-op 路径直接返回，带过期 `expected_version` 的请求会拿到「成功」，
/// 客户端据此认为旧版本已生效，把中间发生的并发修改掩盖掉。
#[tokio::test]
async fn idempotent_publish_and_withdraw_still_enforce_expected_version() {
    let pages = interactor();
    let editor = actor_with(EDITOR);
    let created = pages
        .create(&editor, cmd(Some("about"), "关于"))
        .await
        .unwrap();

    // 首次发布 v1 → v2，页面已发布。
    let published = pages.publish(&editor, created.id, Some(1)).await.unwrap();
    assert_eq!(published.version, 2);

    // 再次发布本是幂等 no-op，但带过期版本必须报冲突，而不是假装成功。
    let err = pages
        .publish(&editor, created.id, Some(1))
        .await
        .unwrap_err();
    assert!(matches!(err, UseCaseError::VersionConflict), "{err:?}");

    // 用当前版本重试：幂等成功且不递增版本。
    let again = pages.publish(&editor, created.id, Some(2)).await.unwrap();
    assert_eq!(again.version, 2, "幂等发布不产生新版本");

    // 撤回 v2 → v3；再次撤回带过期版本同样报冲突。
    let withdrawn = pages.withdraw(&editor, created.id, Some(2)).await.unwrap();
    assert_eq!(withdrawn.version, 3);
    let err = pages
        .withdraw(&editor, created.id, Some(2))
        .await
        .unwrap_err();
    assert!(matches!(err, UseCaseError::VersionConflict), "{err:?}");
}

/// 无变化的编辑同样不能绕过版本前提。
#[tokio::test]
async fn noop_edit_still_enforces_expected_version() {
    let pages = interactor();
    let editor = actor_with(EDITOR);
    let created = pages
        .create(&editor, cmd(Some("about"), "关于"))
        .await
        .unwrap();
    // 提升到 v2。
    pages
        .edit(
            &editor,
            EditPageCmd {
                id: created.id,
                title: Some("关于我们".into()),
                expected_version: Some(1),
                ..Default::default()
            },
        )
        .await
        .unwrap();

    // 提交与当前完全一致的内容（no-op）但带过期版本：必须冲突。
    let noop = || EditPageCmd {
        id: created.id,
        title: Some("关于我们".into()),
        expected_version: Some(1),
        ..Default::default()
    };
    let err = pages.edit(&editor, noop()).await.unwrap_err();
    assert!(matches!(err, UseCaseError::VersionConflict), "{err:?}");

    // 正确的版本下 no-op 才幂等成功，版本不变。
    let same = pages
        .edit(
            &editor,
            EditPageCmd {
                expected_version: Some(2),
                ..noop()
            },
        )
        .await
        .unwrap();
    assert_eq!(same.version, 2);
}
