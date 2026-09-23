//! 媒体用例测试：以测试模块内的假仓储验证权限、公开读取判定与回收失败路径。
//!
//! 真实数据库上的引用/可见性语义由 infrastructure/tests/media.rs 覆盖；
//! 这里聚焦「用例自己的分支」——尤其是文件删除失败时必须保留可重试状态，
//! 那是跨系统一致性问题，用假存储才能稳定复现。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use application::error::UseCaseError;
use application::identity::{Actor, ActorChannel};
use application::media::{MediaInteractor, STAGED_GRACE_SECS, UploadMediaCmd};
use application::ports::{
    Clock, MediaContentKind, MediaDeleteOutcome, MediaRepository, MediaStorage, MediaUsageRow,
    MediaWithUsage,
};
use async_trait::async_trait;
use domain::identity::{PermissionSet, UserId};
use domain::media::{MediaSnapshot, MediaStatus};
use time::OffsetDateTime;
use uuid::Uuid;

/// 最小合法 PNG 头。
fn png_bytes(width: u32, height: u32) -> Vec<u8> {
    let mut bytes = b"\x89PNG\r\n\x1a\n\x00\x00\x00\x0dIHDR".to_vec();
    bytes.extend_from_slice(&width.to_be_bytes());
    bytes.extend_from_slice(&height.to_be_bytes());
    bytes.extend_from_slice(&[8, 6, 0, 0, 0]);
    bytes
}

fn actor(user: Uuid, keys: &[&str]) -> Actor {
    Actor::new(
        UserId(user),
        ActorChannel::Session,
        PermissionSet::from_keys(keys.iter().copied()),
    )
}

/// 可控时钟：回收的宽限期按「现在」判定，测试必须能把时间往前推，
/// 否则要么永远无法认领超期对象，要么刚写入的上传立刻变成垃圾。
struct TestClock {
    now: Mutex<OffsetDateTime>,
}

impl TestClock {
    fn new(at: OffsetDateTime) -> Self {
        Self {
            now: Mutex::new(at),
        }
    }

    fn advance(&self, seconds: i64) {
        *self.now.lock().unwrap() += time::Duration::seconds(seconds);
    }
}

impl Clock for TestClock {
    fn now(&self) -> OffsetDateTime {
        *self.now.lock().unwrap()
    }
}

#[derive(Default)]
struct RepoState {
    items: HashMap<Uuid, MediaSnapshot>,
    created: Vec<Uuid>,
    /// media_id → 使用位置（`public` 字段决定匿名可读性）。
    usage: HashMap<Uuid, Vec<MediaUsageRow>>,
}

#[derive(Default)]
struct FakeMediaRepo {
    state: Mutex<RepoState>,
}

impl FakeMediaRepo {
    fn seed_reference(&self, media_id: Uuid, slug: &str, public: bool) {
        self.seed_owned_reference(media_id, slug, public, Uuid::now_v7());
    }

    /// 指定归属作者的使用位置（用于验证按 own/any 过滤）。
    fn seed_owned_reference(&self, media_id: Uuid, slug: &str, public: bool, owner: Uuid) {
        self.state
            .lock()
            .unwrap()
            .usage
            .entry(media_id)
            .or_default()
            .push(MediaUsageRow {
                kind: MediaContentKind::Post,
                content_id: Uuid::now_v7(),
                author_id: Some(owner),
                slug: slug.into(),
                title: format!("文章 {slug}"),
                status: if public { "published" } else { "draft" }.into(),
                visibility: "public".into(),
                deleted: false,
                public,
            });
    }

    /// 指定类型的使用位置（用于 Page 的站点级权限）。
    fn seed_reference_of_kind(&self, media_id: Uuid, slug: &str, kind: MediaContentKind) {
        self.state
            .lock()
            .unwrap()
            .usage
            .entry(media_id)
            .or_default()
            .push(MediaUsageRow {
                kind,
                content_id: Uuid::now_v7(),
                author_id: None,
                slug: slug.into(),
                title: format!("内容 {slug}"),
                status: "draft".into(),
                visibility: "public".into(),
                deleted: false,
                public: false,
            });
    }

    fn status_of(&self, id: Uuid) -> MediaStatus {
        self.state.lock().unwrap().items[&id].status
    }
}

#[async_trait]
impl MediaRepository for FakeMediaRepo {
    async fn insert_staged(&self, snapshot: &MediaSnapshot) -> Result<(), UseCaseError> {
        let mut state = self.state.lock().unwrap();
        state.created.push(snapshot.id);
        state.items.insert(snapshot.id, snapshot.clone());
        Ok(())
    }

    async fn mark_ready(&self, id: Uuid, now: OffsetDateTime) -> Result<bool, UseCaseError> {
        let mut state = self.state.lock().unwrap();
        let Some(item) = state.items.get_mut(&id) else {
            return Ok(false);
        };
        if item.status != MediaStatus::Staged {
            return Ok(false);
        }
        item.status = MediaStatus::Ready;
        item.version += 1;
        item.updated_at = now;
        Ok(true)
    }

    async fn find_by_id(&self, id: Uuid) -> Result<Option<MediaSnapshot>, UseCaseError> {
        Ok(self.state.lock().unwrap().items.get(&id).cloned())
    }

    async fn find_view(&self, id: Uuid) -> Result<Option<MediaWithUsage>, UseCaseError> {
        let state = self.state.lock().unwrap();
        let Some(item) = state.items.get(&id) else {
            return Ok(None);
        };
        if item.status != MediaStatus::Ready {
            return Ok(None);
        }
        let usage = state.usage.get(&id).cloned().unwrap_or_default();
        Ok(Some(MediaWithUsage {
            snapshot: item.clone(),
            owner_display: "上传者".into(),
            reference_count: usage.len() as i64,
            public_reference_count: usage.iter().filter(|row| row.public).count() as i64,
        }))
    }

    async fn list(
        &self,
        limit: i64,
        offset: i64,
    ) -> Result<(Vec<MediaWithUsage>, i64), UseCaseError> {
        let state = self.state.lock().unwrap();
        let ready: Vec<MediaSnapshot> = state
            .created
            .iter()
            .rev()
            .filter_map(|id| state.items.get(id))
            .filter(|item| item.status == MediaStatus::Ready)
            .cloned()
            .collect();
        let total = ready.len() as i64;
        let page = ready
            .into_iter()
            .skip(offset as usize)
            .take(limit as usize)
            .map(|snapshot| MediaWithUsage {
                snapshot,
                owner_display: "上传者".into(),
                reference_count: 0,
                public_reference_count: 0,
            })
            .collect();
        Ok((page, total))
    }

    async fn usage_of(&self, id: Uuid) -> Result<Vec<MediaUsageRow>, UseCaseError> {
        Ok(self
            .state
            .lock()
            .unwrap()
            .usage
            .get(&id)
            .cloned()
            .unwrap_or_default())
    }

    async fn has_public_reference(&self, id: Uuid) -> Result<bool, UseCaseError> {
        Ok(self
            .state
            .lock()
            .unwrap()
            .usage
            .get(&id)
            .is_some_and(|rows| rows.iter().any(|row| row.public)))
    }

    async fn begin_delete(
        &self,
        id: Uuid,
        expected_version: i64,
        now: OffsetDateTime,
    ) -> Result<MediaDeleteOutcome, UseCaseError> {
        let mut state = self.state.lock().unwrap();
        let usage = state.usage.get(&id).cloned().unwrap_or_default();
        let Some(item) = state.items.get_mut(&id) else {
            return Ok(MediaDeleteOutcome::Gone);
        };
        match item.status {
            MediaStatus::Deleted | MediaStatus::Staged => return Ok(MediaDeleteOutcome::Gone),
            MediaStatus::PendingDeletion => return Ok(MediaDeleteOutcome::Marked),
            MediaStatus::Ready => {}
        }
        if item.version != expected_version {
            return Ok(MediaDeleteOutcome::StaleVersion);
        }
        if !usage.is_empty() {
            return Ok(MediaDeleteOutcome::Referenced {
                count: usage.len() as i64,
            });
        }
        item.status = MediaStatus::PendingDeletion;
        item.version += 1;
        item.updated_at = now;
        Ok(MediaDeleteOutcome::Marked)
    }

    async fn confirm_deleted(&self, id: Uuid, now: OffsetDateTime) -> Result<bool, UseCaseError> {
        let mut state = self.state.lock().unwrap();
        let Some(item) = state.items.get_mut(&id) else {
            return Ok(false);
        };
        if item.status != MediaStatus::PendingDeletion {
            return Ok(false);
        }
        item.status = MediaStatus::Deleted;
        item.version += 1;
        item.updated_at = now;
        Ok(true)
    }

    async fn claim_abandoned_staged(
        &self,
        created_before: OffsetDateTime,
        now: OffsetDateTime,
        limit: i64,
    ) -> Result<Vec<MediaSnapshot>, UseCaseError> {
        let mut state = self.state.lock().unwrap();
        let claimed: Vec<Uuid> = state
            .items
            .values()
            .filter(|item| item.status == MediaStatus::Staged && item.created_at < created_before)
            .map(|item| item.id)
            .take(limit as usize)
            .collect();
        let mut out = Vec::new();
        for id in claimed {
            if let Some(item) = state.items.get_mut(&id)
                && item.status == MediaStatus::Staged
            {
                item.status = MediaStatus::PendingDeletion;
                item.version += 1;
                item.updated_at = now;
                out.push(item.clone());
            }
        }
        Ok(out)
    }

    async fn list_pending_deletion(&self, limit: i64) -> Result<Vec<MediaSnapshot>, UseCaseError> {
        let state = self.state.lock().unwrap();
        Ok(state
            .items
            .values()
            .filter(|item| item.status == MediaStatus::PendingDeletion)
            .take(limit as usize)
            .cloned()
            .collect())
    }
}

struct FakeStorage {
    objects: Mutex<HashMap<String, Vec<u8>>>,
    /// 暂存文件的写入时间；孤儿清扫的宽限期按它判定。
    staged_at: Mutex<HashMap<String, OffsetDateTime>>,
    /// 模拟文件系统故障：删除返回错误，用于验证「保留可重试状态」。
    fail_delete: Mutex<bool>,
    /// 与用例共用同一时钟，使「文件写入时间」与「现在」的关系可控。
    clock: Arc<TestClock>,
}

impl FakeStorage {
    fn new(clock: Arc<TestClock>) -> Self {
        Self {
            objects: Mutex::new(HashMap::new()),
            staged_at: Mutex::new(HashMap::new()),
            fail_delete: Mutex::new(false),
            clock,
        }
    }

    fn fail_deletes(&self) {
        *self.fail_delete.lock().unwrap() = true;
    }
    fn heal(&self) {
        *self.fail_delete.lock().unwrap() = false;
    }
    fn has(&self, key: &str) -> bool {
        self.objects.lock().unwrap().contains_key(key)
    }
}

#[async_trait]
impl MediaStorage for FakeStorage {
    async fn put_staged(&self, key: &str, bytes: &[u8]) -> Result<String, UseCaseError> {
        let staged = format!("staging/{key}");
        self.objects
            .lock()
            .unwrap()
            .insert(staged.clone(), bytes.to_vec());
        self.staged_at
            .lock()
            .unwrap()
            .insert(staged, self.clock.now());
        Ok("a".repeat(64))
    }

    async fn promote(&self, key: &str) -> Result<(), UseCaseError> {
        let mut objects = self.objects.lock().unwrap();
        if let Some(bytes) = objects.remove(&format!("staging/{key}")) {
            objects.insert(key.to_string(), bytes);
        }
        Ok(())
    }

    async fn read(&self, key: &str) -> Result<Option<Vec<u8>>, UseCaseError> {
        Ok(self.objects.lock().unwrap().get(key).cloned())
    }

    async fn delete(&self, key: &str) -> Result<(), UseCaseError> {
        if *self.fail_delete.lock().unwrap() {
            return Err(UseCaseError::Repository("模拟文件系统故障".into()));
        }
        let mut objects = self.objects.lock().unwrap();
        objects.remove(key);
        objects.remove(&format!("staging/{key}"));
        Ok(())
    }

    async fn discard_orphaned_staging(
        &self,
        older_than: OffsetDateTime,
    ) -> Result<i64, UseCaseError> {
        let mut objects = self.objects.lock().unwrap();
        let stale: Vec<String> = objects
            .keys()
            .filter(|key| key.starts_with("staging/"))
            .filter(|key| {
                self.staged_at
                    .lock()
                    .unwrap()
                    .get(*key)
                    .is_some_and(|at| *at < older_than)
            })
            .cloned()
            .collect();
        for key in &stale {
            objects.remove(key);
        }
        Ok(stale.len() as i64)
    }
}

struct Fixture {
    interactor: MediaInteractor,
    repo: Arc<FakeMediaRepo>,
    storage: Arc<FakeStorage>,
    clock: Arc<TestClock>,
}

fn fixture() -> Fixture {
    let clock = Arc::new(TestClock::new(OffsetDateTime::UNIX_EPOCH));
    let repo = Arc::new(FakeMediaRepo::default());
    let storage = Arc::new(FakeStorage::new(clock.clone()));
    Fixture {
        interactor: MediaInteractor::new(repo.clone(), storage.clone(), clock.clone()),
        repo,
        storage,
        clock,
    }
}

async fn upload(fixture: &Fixture, owner: Uuid) -> application::media::MediaDto {
    fixture
        .interactor
        .upload(
            &actor(owner, &["media.upload"]),
            UploadMediaCmd {
                file_name: "photo.png".into(),
                bytes: png_bytes(10, 10),
            },
        )
        .await
        .unwrap()
}

#[tokio::test]
async fn upload_requires_the_upload_permission_and_validates_content() {
    let fixture = fixture();
    let owner = Uuid::now_v7();

    let err = fixture
        .interactor
        .upload(
            &actor(owner, &["media.read"]),
            UploadMediaCmd {
                file_name: "x.png".into(),
                bytes: png_bytes(4, 4),
            },
        )
        .await
        .unwrap_err();
    assert!(matches!(err, UseCaseError::Forbidden), "实际：{err:?}");

    let err = fixture
        .interactor
        .upload(
            &actor(owner, &["media.upload"]),
            UploadMediaCmd {
                file_name: "x.svg".into(),
                bytes: b"<svg/>".to_vec(),
            },
        )
        .await
        .unwrap_err();
    assert!(matches!(err, UseCaseError::Invalid(_)), "实际：{err:?}");
    let (items, total) = fixture.repo.list(10, 0).await.unwrap();
    assert!(items.is_empty() && total == 0, "被拒的上传不得留下记录");

    let dto = upload(&fixture, owner).await;
    assert_eq!(dto.width, 10);
    assert_eq!(dto.version, 2, "staged → ready 递增一次版本");
    assert_eq!(dto.owner_id, owner);
    assert!(fixture.storage.has(&format!("objects/{}.png", dto.id)));
}

#[tokio::test]
async fn anonymous_read_requires_a_public_reference_while_preview_requires_permission() {
    let fixture = fixture();
    let owner = Uuid::now_v7();
    let dto = upload(&fixture, owner).await;

    // 未公开、无 media.read：按不存在处理，不泄漏资产存在性。
    let err = fixture.interactor.read(dto.id, None).await.unwrap_err();
    assert!(matches!(err, UseCaseError::NotFound(_)), "实际：{err:?}");
    let stranger = actor(Uuid::now_v7(), &["post.read"]);
    let err = fixture
        .interactor
        .read(dto.id, Some(&stranger))
        .await
        .unwrap_err();
    assert!(matches!(err, UseCaseError::NotFound(_)), "实际：{err:?}");

    // 后台预览：持有 media.read 即可，与被引用状态无关。
    let viewer = actor(owner, &["media.read"]);
    let content = fixture
        .interactor
        .read(dto.id, Some(&viewer))
        .await
        .unwrap();
    assert_eq!(content.mime, "image/png");
    assert!(
        !content.public_reference,
        "无公开引用时必须标记为非公开来源"
    );

    // 只有草稿引用：仍然不公开。
    fixture.repo.seed_reference(dto.id, "draft", false);
    let err = fixture.interactor.read(dto.id, None).await.unwrap_err();
    assert!(matches!(err, UseCaseError::NotFound(_)));

    // 出现公开引用：匿名可读，并被标记为公开来源（可重校验缓存）。
    fixture.repo.seed_reference(dto.id, "published", true);
    let content = fixture.interactor.read(dto.id, None).await.unwrap();
    assert!(content.public_reference);
    assert_eq!(content.bytes, png_bytes(10, 10));
}

#[tokio::test]
async fn delete_maps_reference_protection_and_permission_boundaries() {
    let fixture = fixture();
    let owner = Uuid::now_v7();
    let other = Uuid::now_v7();
    let dto = upload(&fixture, owner).await;
    fixture.repo.seed_reference(dto.id, "draft", false);

    let operator = actor(owner, &["media.delete"]);
    let err = fixture
        .interactor
        .delete(&operator, dto.id, dto.version)
        .await
        .unwrap_err();
    assert!(
        matches!(err, UseCaseError::MediaInUse(1)),
        "草稿引用同样占用：{err:?}"
    );
    assert_eq!(fixture.repo.status_of(dto.id), MediaStatus::Ready);

    // 非本人上传：需要 media.delete_any。
    let stranger = actor(other, &["media.delete"]);
    let err = fixture
        .interactor
        .delete(&stranger, dto.id, dto.version)
        .await
        .unwrap_err();
    assert!(matches!(err, UseCaseError::Forbidden), "实际：{err:?}");

    // 版本过期：必须先重新读取。
    let err = fixture
        .interactor
        .delete(&operator, dto.id, dto.version + 7)
        .await
        .unwrap_err();
    assert!(
        matches!(err, UseCaseError::VersionConflict),
        "实际：{err:?}"
    );
}

#[tokio::test]
async fn file_removal_failure_keeps_a_retryable_pending_state() {
    let fixture = fixture();
    let owner = Uuid::now_v7();
    let dto = upload(&fixture, owner).await;
    let operator = actor(owner, &["media.delete"]);

    fixture.storage.fail_deletes();
    let err = fixture
        .interactor
        .delete(&operator, dto.id, dto.version)
        .await
        .unwrap_err();
    assert!(matches!(err, UseCaseError::Repository(_)), "实际：{err:?}");
    assert_eq!(
        fixture.repo.status_of(dto.id),
        MediaStatus::PendingDeletion,
        "文件删除失败必须停在可重试状态，不能标记为已删除"
    );
    // 资产已从库中消失，也不再接受新引用。
    let view = fixture.repo.find_view(dto.id).await.unwrap();
    assert!(view.is_none());

    // 存储恢复后，回收流程完成删除并幂等重放。
    fixture.storage.heal();
    let report = fixture
        .interactor
        .reclaim(&actor(owner, &["media.delete_any"]))
        .await
        .unwrap();
    assert_eq!(report.deleted, 1);
    assert!(report.failures.is_empty());
    assert_eq!(fixture.repo.status_of(dto.id), MediaStatus::Deleted);
    assert!(!fixture.storage.has(&format!("objects/{}.png", dto.id)));

    let again = fixture
        .interactor
        .reclaim(&actor(owner, &["media.delete_any"]))
        .await
        .unwrap();
    assert_eq!(again.deleted, 0, "重放不得重复计数");
}

#[tokio::test]
async fn reclaim_reports_failures_and_discards_interrupted_uploads() {
    let fixture = fixture();
    let owner = Uuid::now_v7();
    let operator = actor(owner, &["media.delete_any"]);

    // 人工制造一条停在 staged 的资产（模拟上传在 promote 之前中断）。
    let info = domain::media::inspect_image(&png_bytes(4, 4)).unwrap();
    let id = Uuid::now_v7();
    let key = format!("objects/{id}.png");
    let staged = domain::media::Media::stage(
        id,
        owner,
        key.clone(),
        "half.png",
        info,
        26,
        "c".repeat(64),
        OffsetDateTime::UNIX_EPOCH,
    )
    .unwrap()
    .snapshot();
    fixture
        .storage
        .put_staged(&key, &png_bytes(4, 4))
        .await
        .unwrap();
    fixture.repo.insert_staged(&staged).await.unwrap();

    // 宽限期内：不得认领。
    let early = fixture.interactor.reclaim(&operator).await.unwrap();
    assert_eq!(early.abandoned_staged, 0, "宽限期内的上传不能被认领");
    assert_eq!(fixture.repo.status_of(id), MediaStatus::Staged);

    // 超过宽限期后认领；此时文件删除失败，状态必须停在可重试的 pending_deletion。
    fixture.clock.advance(STAGED_GRACE_SECS + 60);
    fixture.storage.fail_deletes();
    let report = fixture.interactor.reclaim(&operator).await.unwrap();
    assert_eq!(report.abandoned_staged, 1);
    assert_eq!(report.deleted, 0, "文件没删掉就不能确认删除");
    assert_eq!(report.failures.len(), 1);
    assert!(
        report.failures[0].contains(&key),
        "失败项要能定位到存储路径"
    );
    assert_eq!(fixture.repo.status_of(id), MediaStatus::PendingDeletion);

    // 存储恢复后重试完成删除，且重放幂等。
    fixture.storage.heal();
    let report = fixture.interactor.reclaim(&operator).await.unwrap();
    assert_eq!(report.abandoned_staged, 0, "已认领过的不再重复认领");
    assert_eq!(report.deleted, 1);
    assert_eq!(fixture.repo.status_of(id), MediaStatus::Deleted);
    assert!(!fixture.storage.has(&key));

    // 缺少 media.delete_any 的调用者不能驱动回收。
    let err = fixture
        .interactor
        .reclaim(&actor(owner, &["media.delete"]))
        .await
        .unwrap_err();
    assert!(matches!(err, UseCaseError::Forbidden));
}

#[tokio::test]
async fn listing_requires_media_read_and_pages_by_upload_time() {
    let fixture = fixture();
    let owner = Uuid::now_v7();
    for _ in 0..3 {
        upload(&fixture, owner).await;
    }

    let err = fixture
        .interactor
        .list(&actor(owner, &["post.read"]), 1)
        .await
        .unwrap_err();
    assert!(matches!(err, UseCaseError::Forbidden));

    let viewer = actor(owner, &["media.read"]);
    let page = fixture.interactor.list(&viewer, 1).await.unwrap();
    assert_eq!(page.total, 3);
    assert_eq!(page.page, 1);
    assert_eq!(page.items.len(), 3);
    // 倒序：最新上传在前（固定时钟下按插入顺序倒排）。
    let err = fixture.interactor.list(&viewer, 0).await.unwrap_err();
    assert!(matches!(err, UseCaseError::Invalid(_)), "页码 0 必须拒绝");
}

/// 使用位置必须按调用者的**内容**权限过滤：`media.read` 只授予浏览媒体库，
/// 不能顺带泄露他人草稿/私密文章的标题与 slug；引用计数仍然全局，决定能否删除。
///
/// 「公开可读的内容」不算泄露：它的标题与 slug 本来就能匿名访问，
/// 因此公开引用对任何持有 `media.read` 的人都可见，不会被误报成隐藏项。
#[tokio::test]
async fn usage_locations_are_filtered_by_content_permissions() {
    let fixture = fixture();
    let owner = Uuid::now_v7();
    let media = upload(&fixture, owner).await;

    fixture
        .repo
        .seed_owned_reference(media.id, "own-draft", false, owner);
    fixture
        .repo
        .seed_owned_reference(media.id, "others-draft", false, Uuid::now_v7());
    fixture
        .repo
        .seed_owned_reference(media.id, "others-public", true, Uuid::now_v7());
    fixture
        .repo
        .seed_reference_of_kind(media.id, "about", MediaContentKind::Page);

    // ① 只有 media.read（其他内容的任何读权限都没有）：只看得到公开可见的那一条。
    let bare = actor(owner, &["media.read"]);
    let view = fixture.interactor.detail(&bare, media.id).await.unwrap();
    let slugs: Vec<&str> = view.references.iter().map(|r| r.slug.as_str()).collect();
    assert_eq!(
        slugs,
        vec!["others-public"],
        "没有内容读权限时，只剩「本来就能匿名读到」的那条：{slugs:?}"
    );
    assert_eq!(view.hidden_references, 3);
    assert_eq!(
        view.media.reference_count, 4,
        "引用计数必须保持全局：它决定能否删除"
    );

    // ② 作者 own：看得到自己的草稿与已公开的文章，看不到他人草稿。
    let author = actor(owner, &["media.read", "post.read"]);
    let view = fixture.interactor.detail(&author, media.id).await.unwrap();
    let mut slugs: Vec<&str> = view.references.iter().map(|r| r.slug.as_str()).collect();
    slugs.sort_unstable();
    assert_eq!(
        slugs,
        vec!["others-public", "own-draft"],
        "own 看不到他人草稿，但公开可见的内容不算隐藏：{slugs:?}"
    );
    assert_eq!(view.hidden_references, 2, "隐藏的是他人草稿与无权读的页面");

    // ③ 编辑 read_any：看得到全部文章，但 Page 仍要 page.read。
    let editor = actor(Uuid::now_v7(), &["media.read", "post.read_any"]);
    let view = fixture.interactor.detail(&editor, media.id).await.unwrap();
    assert_eq!(view.references.len(), 3, "三篇文章的引用都可见");
    assert!(
        view.references.iter().all(|r| r.kind == "post"),
        "没有 page.read 时不得看到页面使用位置"
    );
    assert_eq!(view.hidden_references, 1);

    // ④ 加上 page.read：全部可见，没有隐藏项。
    let editor = actor(
        Uuid::now_v7(),
        &["media.read", "post.read_any", "page.read"],
    );
    let view = fixture.interactor.detail(&editor, media.id).await.unwrap();
    assert_eq!(view.references.len(), 4);
    assert_eq!(view.hidden_references, 0);

    // ⑤ 删除保护仍然按**全部**引用判定：作者即使看不到他人引用也删不掉。
    let err = fixture
        .interactor
        .delete(&actor(owner, &["media.delete"]), media.id, media.version)
        .await
        .unwrap_err();
    assert!(
        matches!(err, UseCaseError::MediaInUse(4)),
        "引用保护必须计算全部引用（含不可见项）：{err:?}"
    );

    // ⑥ 没有 media.read 就连详情都读不到。
    let stranger = actor(owner, &["post.read"]);
    let err = fixture
        .interactor
        .detail(&stranger, media.id)
        .await
        .unwrap_err();
    assert!(matches!(err, UseCaseError::Forbidden));
}

/// 没有数据库记录的暂存残留由回收的暂存清扫按宽限期处理。
#[tokio::test]
async fn reclaim_sweeps_staging_files_that_have_no_database_row() {
    let fixture = fixture();
    let owner = Uuid::now_v7();
    let operator = actor(owner, &["media.delete_any"]);

    // 模拟「文件写入成功、插入行失败」：只有文件，没有行。
    let orphan = format!("objects/{}.png", Uuid::now_v7());
    fixture
        .storage
        .put_staged(&orphan, &png_bytes(4, 4))
        .await
        .unwrap();

    // 宽限期内保留（可能属于正在进行的上传）。
    let report = fixture.interactor.reclaim(&operator).await.unwrap();
    assert_eq!(report.orphaned_staging_files, 0);
    assert!(fixture.storage.has(&format!("staging/{orphan}")));

    // 超期后清扫。
    fixture.clock.advance(STAGED_GRACE_SECS + 60);
    let report = fixture.interactor.reclaim(&operator).await.unwrap();
    assert_eq!(report.orphaned_staging_files, 1);
    assert!(!fixture.storage.has(&format!("staging/{orphan}")));

    // 幂等重放。
    let again = fixture.interactor.reclaim(&operator).await.unwrap();
    assert_eq!(again.orphaned_staging_files, 0);
}
