//! 站点设置用例测试：权限边界、优先级回退、校验、并发版本与「重启」保留。
//! fake 存储模拟 UPSERT+CAS 语义；不依赖生产 infrastructure。

use std::sync::{Arc, Mutex};

use application::error::UseCaseError;
use application::identity::{Actor, ActorChannel};
use application::ports::{
    Clock, SaveOutcome, SettingsStore, SiteSettingsRecord, SiteSettingsValue,
};
use application::public_site::SiteInfo;
use application::settings::{
    SaveSiteSettingsCmd, SettingsInteractor, SiteSettingsSource, SiteSettingsView,
};
use domain::identity::{PermissionSet, UserId};
use time::OffsetDateTime;
use uuid::Uuid;

mod common;

// ---------------------------------------------------------------------------
// Fakes
// ---------------------------------------------------------------------------

struct FixedClock;

impl Clock for FixedClock {
    fn now(&self) -> OffsetDateTime {
        time::macros::datetime!(2026-09-23 12:00:00 UTC)
    }
}

/// site 分组 fake：镜像 Postgres 的 UPSERT + 版本 CAS 语义
/// （expected=0 且行不存在 → 插入 v1；版本匹配 → 替换并 v+1；否则冲突）。
struct FakeSettingsStore {
    row: Mutex<Option<SiteSettingsRecord>>,
}

impl FakeSettingsStore {
    fn new() -> Self {
        Self {
            row: Mutex::new(None),
        }
    }

    fn seed(&self, value: SiteSettingsValue, version: i64) {
        *self.row.lock().unwrap() = Some(SiteSettingsRecord { value, version });
    }
}

#[async_trait::async_trait]
impl SettingsStore for FakeSettingsStore {
    async fn find_site(&self) -> Result<Option<SiteSettingsRecord>, UseCaseError> {
        Ok(self.row.lock().unwrap().clone())
    }

    async fn save_site(
        &self,
        value: &SiteSettingsValue,
        expected_version: i64,
        _now: OffsetDateTime,
        _audit_actor: Option<uuid::Uuid>,
    ) -> Result<SaveOutcome, UseCaseError> {
        let mut row = self.row.lock().unwrap();
        match row.as_mut() {
            None => {
                if expected_version != 0 {
                    return Ok(SaveOutcome::StaleConflict);
                }
                *row = Some(SiteSettingsRecord {
                    value: value.clone(),
                    version: 1,
                });
                Ok(SaveOutcome::Saved { new_version: 1 })
            }
            Some(record) => {
                if record.version != expected_version {
                    return Ok(SaveOutcome::StaleConflict);
                }
                record.value = value.clone();
                record.version += 1;
                Ok(SaveOutcome::Saved {
                    new_version: record.version,
                })
            }
        }
    }
}

/// 持有 `settings.manage` 的会话身份（内置 admin/owner 的关键子集）。
fn admin_actor() -> Actor {
    Actor::new(
        UserId(Uuid::nil()),
        ActorChannel::Session,
        PermissionSet::from_keys(["settings.manage"]),
    )
}

/// 无 settings.manage 的会话身份（内置 author/editor 都不含该权限）。
fn content_actor() -> Actor {
    Actor::new(
        UserId(Uuid::nil()),
        ActorChannel::Session,
        PermissionSet::from_keys(["post.create", "tag.manage"]),
    )
}

fn fallback() -> SiteInfo {
    SiteInfo {
        title: "环境变量标题".into(),
        description: "环境变量描述".into(),
        logo_url: None,
    }
}

fn stored(title: Option<&str>, description: Option<&str>) -> SiteSettingsValue {
    SiteSettingsValue {
        title: title.map(str::to_string),
        description: description.map(str::to_string),
        logo_media_id: None,
    }
}

fn cmd(title: &str, description: &str, expected_version: Option<i64>) -> SaveSiteSettingsCmd {
    SaveSiteSettingsCmd {
        title: title.into(),
        description: description.into(),
        logo_media_id: None,
        expected_version,
    }
}

fn interactor(store: Arc<FakeSettingsStore>) -> SettingsInteractor {
    interactor_with_guard(store, Arc::new(common::FakeMediaGuard::new()))
}

fn interactor_with_guard(
    store: Arc<FakeSettingsStore>,
    media_guard: Arc<common::FakeMediaGuard>,
) -> SettingsInteractor {
    SettingsInteractor::new(store, Arc::new(FixedClock), fallback(), media_guard)
}

// ---------------------------------------------------------------------------
// 读取：优先级与权限
// ---------------------------------------------------------------------------

#[tokio::test]
async fn read_falls_back_to_assembly_when_not_configured() {
    let settings = interactor(Arc::new(FakeSettingsStore::new()));
    let view = settings.site_view(&admin_actor()).await.unwrap();
    assert_eq!(
        view,
        SiteSettingsView {
            title: "环境变量标题".into(),
            description: "环境变量描述".into(),
            logo_media_id: None,
            logo_url: None,
            source: SiteSettingsSource::Fallback,
            version: 0,
        }
    );
}

#[tokio::test]
async fn read_and_write_require_settings_manage() {
    let store = Arc::new(FakeSettingsStore::new());
    let settings = interactor(store.clone());

    let err = settings.site_view(&content_actor()).await.unwrap_err();
    assert!(
        matches!(err, UseCaseError::Forbidden),
        "读取需授权：{err:?}"
    );

    let err = settings
        .save_site(&content_actor(), cmd("新标题", "新描述", None))
        .await
        .unwrap_err();
    assert!(
        matches!(err, UseCaseError::Forbidden),
        "写入需授权：{err:?}"
    );
    assert!(store.row.lock().unwrap().is_none(), "越权写入不得触碰存储");
}

#[tokio::test]
async fn partial_row_falls_back_per_field() {
    let store = FakeSettingsStore::new();
    // 手工/历史写入的行只有标题：标题取数据库，描述回退装配值。
    store.seed(stored(Some("数据库标题"), None), 3);
    let settings = interactor(Arc::new(store));
    let view = settings.site_view(&admin_actor()).await.unwrap();
    assert_eq!(view.title, "数据库标题");
    assert_eq!(view.description, "环境变量描述");
    assert_eq!(view.source, SiteSettingsSource::Database);
    assert_eq!(view.version, 3);
}

// ---------------------------------------------------------------------------
// 保存：校验、生效与幂等
// ---------------------------------------------------------------------------

#[tokio::test]
async fn save_inserts_row_then_updates_with_version() {
    let store = Arc::new(FakeSettingsStore::new());
    let settings = interactor(store.clone());

    let first = settings
        .save_site(&admin_actor(), cmd("  数据库标题  ", "数据库描述", Some(0)))
        .await
        .unwrap();
    assert_eq!(first.version, 1);
    assert_eq!(first.title, "数据库标题", "标题按 trim 后的规范值保存");
    assert_eq!(first.source, SiteSettingsSource::Database);

    let second = settings
        .save_site(&admin_actor(), cmd("新标题", "新描述", Some(1)))
        .await
        .unwrap();
    assert_eq!(second.version, 2);
    assert_eq!(second.title, "新标题");

    let view = settings.site_view(&admin_actor()).await.unwrap();
    assert_eq!(view.version, 2);
    assert_eq!(
        (view.title.as_str(), view.description.as_str()),
        ("新标题", "新描述")
    );
}

#[tokio::test]
async fn invalid_values_are_rejected_without_touching_store() {
    let store = Arc::new(FakeSettingsStore::new());
    let settings = interactor(store.clone());
    store.seed(stored(Some("旧标题"), Some("旧描述")), 1);

    let long_title = "长".repeat(domain::settings::SITE_TITLE_MAX_CHARS + 1);
    let long_description = "述".repeat(domain::settings::SITE_DESCRIPTION_MAX_CHARS + 1);
    for (title, description) in [
        ("", "描述"),
        ("   ", "描述"),
        (&long_title, "描述"),
        ("标题", &long_description),
    ] {
        let err = settings
            .save_site(&admin_actor(), cmd(title, description, Some(1)))
            .await
            .unwrap_err();
        assert!(
            matches!(err, UseCaseError::Invalid(_)),
            "({title:?}, ..) 应为非法值：{err:?}"
        );
    }
    let row = store.row.lock().unwrap().clone().unwrap();
    assert_eq!((row.version, row.value.title), (1, Some("旧标题".into())));
}

#[tokio::test]
async fn identical_save_is_idempotent_without_version_bump() {
    let store = Arc::new(FakeSettingsStore::new());
    let settings = interactor(store.clone());
    settings
        .save_site(&admin_actor(), cmd("标题", "描述", Some(0)))
        .await
        .unwrap();

    let again = settings
        .save_site(&admin_actor(), cmd(" 标题 ", "描述", Some(1)))
        .await
        .unwrap();
    assert_eq!(again.version, 1, "内容一致不递增版本");
    assert_eq!(store.row.lock().unwrap().as_ref().unwrap().version, 1);
}

#[tokio::test]
async fn saving_fallback_values_still_persists_row() {
    // 保存动作的意图是「让数据库接管」：即使值恰好等于装配回退值，
    // 行不存在时也必须落库，此后环境变量调整不再影响站点。
    let store = Arc::new(FakeSettingsStore::new());
    let settings = interactor(store.clone());
    let view = settings
        .save_site(&admin_actor(), cmd("环境变量标题", "环境变量描述", Some(0)))
        .await
        .unwrap();
    assert_eq!(view.source, SiteSettingsSource::Database);
    assert_eq!(view.version, 1);
    assert!(store.row.lock().unwrap().is_some());
}

// ---------------------------------------------------------------------------
// 并发与持久性
// ---------------------------------------------------------------------------

#[tokio::test]
async fn stale_expected_version_conflicts() {
    let store = Arc::new(FakeSettingsStore::new());
    let settings = interactor(store.clone());
    store.seed(stored(Some("当前"), Some("值")), 3);

    for stale in [0, 2, 4] {
        let err = settings
            .save_site(&admin_actor(), cmd("并发标题", "描述", Some(stale)))
            .await
            .unwrap_err();
        assert!(
            matches!(err, UseCaseError::VersionConflict),
            "expected={stale} 应为版本冲突，得到 {err:?}"
        );
    }
    // 存储未被触碰，仍是版本 3。
    assert_eq!(
        store.row.lock().unwrap().as_ref().unwrap().value.title,
        Some("当前".into())
    );
}

#[tokio::test]
async fn concurrent_saves_exactly_one_wins() {
    let store = Arc::new(FakeSettingsStore::new());
    store.seed(stored(Some("初版"), Some("描述")), 1);
    let left = interactor(store.clone());
    let right = interactor(store.clone());

    let left_actor = admin_actor();
    let right_actor = admin_actor();
    let (a, b) = tokio::join!(
        left.save_site(&left_actor, cmd("甲的标题", "描述", Some(1))),
        right.save_site(&right_actor, cmd("乙的标题", "描述", Some(1))),
    );
    let outcomes = [a, b];
    let saved = outcomes.iter().filter(|r| r.is_ok()).count();
    let conflicted = outcomes
        .iter()
        .filter(|r| matches!(r, Err(UseCaseError::VersionConflict)))
        .count();
    assert_eq!(saved, 1, "并发保存必须恰好一方成功：{outcomes:?}");
    assert_eq!(conflicted, 1, "另一方必须收到可定位的版本冲突");

    let row = store.row.lock().unwrap().clone().unwrap();
    assert_eq!(row.version, 2);
    let winner = outcomes
        .iter()
        .find(|r| r.is_ok())
        .unwrap()
        .as_ref()
        .unwrap();
    assert_eq!(row.value.title, Some(winner.title.clone()));
}

#[tokio::test]
async fn new_interactor_over_same_store_keeps_configuration() {
    // 「重启」语义：进程内状态（interactor 实例）全部丢弃，配置保留在存储里。
    let store = Arc::new(FakeSettingsStore::new());
    interactor(store.clone())
        .save_site(&admin_actor(), cmd("持久标题", "持久描述", Some(0)))
        .await
        .unwrap();

    let revived = interactor(store);
    let view = revived.site_view(&admin_actor()).await.unwrap();
    assert_eq!(
        (view.title.as_str(), view.description.as_str()),
        ("持久标题", "持久描述")
    );
    assert_eq!(view.source, SiteSettingsSource::Database);
}

// ---------------------------------------------------------------------------
// 站点 logo 的媒体引用可用性
// ---------------------------------------------------------------------------

fn logo_cmd(logo: uuid::Uuid, expected_version: Option<i64>) -> SaveSiteSettingsCmd {
    SaveSiteSettingsCmd {
        title: "站点标题".into(),
        description: "站点描述".into(),
        logo_media_id: Some(logo),
        expected_version,
    }
}

#[tokio::test]
async fn site_logo_accepts_shared_image_without_media_read_permission() {
    let store = Arc::new(FakeSettingsStore::new());
    let guard = Arc::new(common::FakeMediaGuard::new());
    let settings = interactor_with_guard(store, guard.clone());
    let image = uuid::Uuid::now_v7();
    guard.allow(image);
    settings
        .save_site(&admin_actor(), logo_cmd(image, Some(0)))
        .await
        .unwrap();
    guard.trash(image);
    settings
        .save_site(&admin_actor(), logo_cmd(image, Some(1)))
        .await
        .unwrap();
    assert_eq!(
        settings
            .site_view(&admin_actor())
            .await
            .unwrap()
            .logo_media_id,
        Some(image)
    );
    assert!(matches!(
        settings
            .save_site(&admin_actor(), logo_cmd(uuid::Uuid::now_v7(), Some(1)))
            .await,
        Err(UseCaseError::Invalid(_))
    ));
}

#[tokio::test]
async fn site_logo_rejects_trashed_image_as_a_new_reference() {
    let guard = Arc::new(common::FakeMediaGuard::new());
    let image = uuid::Uuid::now_v7();
    guard.allow(image);
    guard.trash(image);
    let settings = interactor_with_guard(Arc::new(FakeSettingsStore::new()), guard);
    assert!(matches!(
        settings
            .save_site(&admin_actor(), logo_cmd(image, Some(0)))
            .await,
        Err(UseCaseError::Invalid(_))
    ));
    assert_eq!(settings.site_view(&admin_actor()).await.unwrap().version, 0);
}
