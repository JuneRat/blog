//! 媒体用例边界：独立公开链接、回收站权限，以及文件与数据库提交失败。
use application::{
    error::UseCaseError,
    identity::{Actor, ActorChannel},
    media::{MediaInteractor, STAGED_GRACE_SECS, UploadMediaCmd},
    ports::{
        Clock, MediaChangeOutcome, MediaContentKind, MediaRepository, MediaStorage, MediaUsageRow,
        MediaUsageSource, MediaWithUsage,
    },
};
use async_trait::async_trait;
use domain::{
    identity::{PermissionSet, UserId},
    media::{Media, MediaSnapshot},
};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
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
    /// media_id → 来源内容，public 只影响使用位置展示。
    usage: HashMap<Uuid, Vec<MediaUsageRow>>,
}

#[derive(Default)]
struct FakeMediaRepo {
    state: Mutex<RepoState>,
    fail_insert: Mutex<bool>,
    commit_then_fail: Mutex<bool>,
}

impl FakeMediaRepo {
    /// 指定归属作者的使用位置（用于验证按 own/any 过滤）。
    fn seed_owned_reference(&self, media_id: Uuid, slug: &str, public: bool, owner: Uuid) {
        self.state
            .lock()
            .unwrap()
            .usage
            .entry(media_id)
            .or_default()
            .push(MediaUsageRow {
                source: MediaUsageSource::Post(if public {
                    domain::content::post::PostStatus::Published
                } else {
                    domain::content::post::PostStatus::Draft
                }),
                content_id: Uuid::now_v7(),
                author_id: Some(owner),
                slug: slug.into(),
                title: format!("文章 {slug}"),
                visibility: domain::content::Visibility::Public,
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
                source: match kind {
                    MediaContentKind::Post => {
                        MediaUsageSource::Post(domain::content::post::PostStatus::Draft)
                    }
                    MediaContentKind::Page => {
                        MediaUsageSource::Page(domain::content::page::PageStatus::Draft)
                    }
                    MediaContentKind::User => {
                        MediaUsageSource::User(domain::identity::UserStatus::Active)
                    }
                    MediaContentKind::Series => MediaUsageSource::Series,
                    MediaContentKind::Site => MediaUsageSource::Site,
                    MediaContentKind::Theme => MediaUsageSource::Theme,
                    MediaContentKind::Revision => MediaUsageSource::PostRevision,
                },
                content_id: Uuid::now_v7(),
                author_id: None,
                slug: slug.into(),
                title: format!("内容 {slug}"),
                visibility: domain::content::Visibility::Public,
                deleted: false,
                public: false,
            });
    }
}

#[async_trait]
impl MediaRepository for FakeMediaRepo {
    async fn insert(
        &self,
        aggregate: &Media,
        _actor: application::audit::AuditContext,
    ) -> Result<(), UseCaseError> {
        if *self.fail_insert.lock().unwrap() {
            return Err(UseCaseError::Repository("insert failed".into()));
        }
        let s = aggregate.snapshot();
        let mut state = self.state.lock().unwrap();
        state.created.push(s.id);
        state.items.insert(s.id, s);
        if *self.commit_then_fail.lock().unwrap() {
            return Err(UseCaseError::Repository(
                "commit acknowledgement lost".into(),
            ));
        }
        Ok(())
    }
    async fn find_by_id(&self, id: Uuid) -> Result<Option<MediaSnapshot>, UseCaseError> {
        Ok(self.state.lock().unwrap().items.get(&id).cloned())
    }
    async fn find_view(&self, id: Uuid) -> Result<Option<MediaWithUsage>, UseCaseError> {
        let state = self.state.lock().unwrap();
        Ok(state.items.get(&id).map(|s| MediaWithUsage {
            snapshot: s.clone(),
            owner_display: "Uploader".into(),
            reference_count: state.usage.get(&id).map_or(0, |refs| refs.len() as i64),
        }))
    }
    async fn list(
        &self,
        limit: i64,
        offset: i64,
        trash: bool,
        q: Option<&str>,
    ) -> Result<(Vec<MediaWithUsage>, i64), UseCaseError> {
        let state = self.state.lock().unwrap();
        let rows: Vec<_> = state
            .created
            .iter()
            .rev()
            .filter_map(|id| {
                let s = &state.items[id];
                (s.deleted_at.is_some() == trash
                    && q.is_none_or(|q| s.original_name.to_lowercase().contains(&q.to_lowercase())))
                .then(|| MediaWithUsage {
                    snapshot: s.clone(),
                    owner_display: "Uploader".into(),
                    reference_count: state.usage.get(id).map_or(0, |r| r.len() as i64),
                })
            })
            .collect();
        let total = rows.len() as i64;
        Ok((
            rows.into_iter()
                .skip(offset as usize)
                .take(limit as usize)
                .collect(),
            total,
        ))
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
    async fn set_deleted(
        &self,
        id: Uuid,
        version: i64,
        deleted: bool,
        now: OffsetDateTime,
        _actor: application::audit::AuditContext,
    ) -> Result<MediaChangeOutcome, UseCaseError> {
        let mut state = self.state.lock().unwrap();
        let Some(s) = state.items.get_mut(&id) else {
            return Ok(MediaChangeOutcome::Gone);
        };
        if s.version != version {
            return Ok(MediaChangeOutcome::StaleVersion);
        }
        if s.deleted_at.is_some() == deleted {
            return Ok(MediaChangeOutcome::Unchanged);
        }
        s.deleted_at = deleted.then_some(now);
        s.version += 1;
        s.updated_at = now;
        Ok(MediaChangeOutcome::Updated)
    }
}

struct FakeStorage {
    objects: Mutex<HashMap<String, Vec<u8>>>,
    /// 暂存文件的写入时间；孤儿清扫的宽限期按它判定。
    staged_at: Mutex<HashMap<String, OffsetDateTime>>,
    /// 模拟文件系统故障：删除返回错误，用于验证「保留可重试状态」。
    fail_promote: Mutex<bool>,
    /// 与用例共用同一时钟，使「文件写入时间」与「现在」的关系可控。
    clock: Arc<TestClock>,
}

impl FakeStorage {
    fn new(clock: Arc<TestClock>) -> Self {
        Self {
            objects: Mutex::new(HashMap::new()),
            staged_at: Mutex::new(HashMap::new()),
            fail_promote: Mutex::new(false),
            clock,
        }
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
        if *self.fail_promote.lock().unwrap() {
            return Err(UseCaseError::Repository("promote failed".into()));
        }
        let mut objects = self.objects.lock().unwrap();
        if let Some(bytes) = objects.remove(&format!("staging/{key}")) {
            objects.insert(key.to_string(), bytes);
        }
        Ok(())
    }

    async fn open(
        &self,
        key: &str,
    ) -> Result<Option<application::ports::OpenedMedia>, UseCaseError> {
        Ok(self.objects.lock().unwrap().get(key).cloned().map(|bytes| {
            application::ports::OpenedMedia {
                byte_size: bytes.len() as u64,
                reader: Box::new(FakeReader { bytes, position: 0 }),
            }
        }))
    }

    async fn delete(&self, key: &str) -> Result<(), UseCaseError> {
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

struct FakeReader {
    bytes: Vec<u8>,
    position: usize,
}

#[async_trait]
impl application::ports::MediaReader for FakeReader {
    async fn read_chunk(&mut self, max_bytes: usize) -> Result<Option<Vec<u8>>, UseCaseError> {
        if self.position == self.bytes.len() {
            return Ok(None);
        }
        let end = self
            .position
            .saturating_add(max_bytes)
            .min(self.bytes.len());
        let chunk = self.bytes[self.position..end].to_vec();
        self.position = end;
        Ok(Some(chunk))
    }
}

struct FakeInspector;
impl application::ports::ImageInspector for FakeInspector {
    fn inspect(&self, bytes: &[u8]) -> Result<domain::media::ImageInfo, domain::media::MediaError> {
        // Only the fixture payload is accepted; binary parsing is tested by the adapter.
        if bytes == png_bytes(10, 10) || bytes == png_bytes(4, 4) {
            domain::media::ImageInfo::new(domain::media::ImageFormat::Png, 10, 10)
        } else {
            Err(domain::media::MediaError::UnsupportedFormat)
        }
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
        interactor: MediaInteractor::new(
            Arc::new(FakeInspector),
            repo.clone(),
            storage.clone(),
            clock.clone(),
        ),
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

async fn read(fixture: &Fixture, id: Uuid) -> Vec<u8> {
    let metadata = fixture.interactor.read_metadata(id).await.unwrap();
    fixture.interactor.read(&metadata).await.unwrap()
}

#[tokio::test]
async fn open_checks_file_presence_and_length_before_streaming() {
    let f = fixture();
    let dto = upload(&f, Uuid::now_v7()).await;
    let metadata = f.interactor.read_metadata(dto.id).await.unwrap();
    let opened = f.interactor.open(&metadata).await.unwrap();
    assert_eq!(opened.byte_size, dto.byte_size as u64);
    drop(opened);
    let key = format!("objects/{}.png", dto.id);
    f.storage
        .objects
        .lock()
        .unwrap()
        .get_mut(&key)
        .unwrap()
        .push(0);
    assert!(matches!(
        f.interactor.open(&metadata).await,
        Err(UseCaseError::Repository(_))
    ));
    f.storage.objects.lock().unwrap().remove(&key);
    assert!(matches!(
        f.interactor.open(&metadata).await,
        Err(UseCaseError::Repository(_))
    ));
}

#[tokio::test]
async fn upload_validates_permissions_and_content_before_registering_an_available_image() {
    let f = fixture();
    let owner = Uuid::now_v7();
    for (keys, bytes) in [
        (vec!["media.read"], png_bytes(10, 10)),
        (vec!["media.upload"], b"<svg/>".to_vec()),
    ] {
        assert!(
            f.interactor
                .upload(
                    &actor(owner, &keys),
                    UploadMediaCmd {
                        file_name: "x.png".into(),
                        bytes
                    }
                )
                .await
                .is_err()
        );
    }
    assert!(f.repo.state.lock().unwrap().items.is_empty());
    assert!(f.storage.objects.lock().unwrap().is_empty());
    let dto = upload(&f, owner).await;
    assert_eq!(dto.version, 1);
    assert_eq!(dto.owner_id, Some(owner));
    assert!(dto.deleted_at.is_none());
    assert!(f.storage.has(&format!("objects/{}.png", dto.id)));
}

#[tokio::test]
async fn failed_promotion_never_registers_a_missing_file() {
    let f = fixture();
    *f.storage.fail_promote.lock().unwrap() = true;
    let result = f
        .interactor
        .upload(
            &actor(Uuid::now_v7(), &["media.upload"]),
            UploadMediaCmd {
                file_name: "x.png".into(),
                bytes: png_bytes(10, 10),
            },
        )
        .await;
    assert!(result.is_err());
    assert!(f.repo.state.lock().unwrap().items.is_empty());
    assert!(
        f.storage
            .objects
            .lock()
            .unwrap()
            .keys()
            .all(|key| key.starts_with("staging/"))
    );
}

#[tokio::test]
async fn database_failure_never_removes_a_formal_object_even_if_commit_succeeded() {
    for committed in [false, true] {
        let f = fixture();
        *f.repo.fail_insert.lock().unwrap() = !committed;
        *f.repo.commit_then_fail.lock().unwrap() = committed;
        assert!(
            f.interactor
                .upload(
                    &actor(Uuid::now_v7(), &["media.upload"]),
                    UploadMediaCmd {
                        file_name: "x.png".into(),
                        bytes: png_bytes(10, 10)
                    }
                )
                .await
                .is_err()
        );
        let keys: Vec<_> = f.storage.objects.lock().unwrap().keys().cloned().collect();
        assert_eq!(keys.len(), 1);
        assert!(keys[0].starts_with("objects/"));
        if committed {
            let id = f.repo.state.lock().unwrap().created[0];
            assert_eq!(read(&f, id).await, png_bytes(10, 10));
        }
    }
}

#[tokio::test]
async fn independent_reads_and_soft_deletion_preserve_files_and_references() {
    let f = fixture();
    let owner = Uuid::now_v7();
    let dto = upload(&f, owner).await;
    assert_eq!(read(&f, dto.id).await, png_bytes(10, 10));
    f.repo
        .seed_owned_reference(dto.id, "private-draft", false, Uuid::now_v7());
    let author = actor(owner, &["media.delete", "media.read"]);
    assert!(matches!(
        f.interactor
            .set_deleted(&actor(Uuid::now_v7(), &["media.delete"]), dto.id, 1, true)
            .await,
        Err(UseCaseError::Forbidden)
    ));
    f.interactor
        .set_deleted(&author, dto.id, 1, true)
        .await
        .unwrap();
    assert_eq!(read(&f, dto.id).await, png_bytes(10, 10));
    assert_eq!(
        f.interactor
            .list(&author, 1, false, None)
            .await
            .unwrap()
            .total,
        0
    );
    assert_eq!(
        f.interactor
            .list(&author, 1, true, None)
            .await
            .unwrap()
            .total,
        1
    );
    assert!(matches!(
        f.interactor.set_deleted(&author, dto.id, 1, false).await,
        Err(UseCaseError::VersionConflict)
    ));
    f.interactor
        .set_deleted(
            &actor(Uuid::now_v7(), &["media.delete_any"]),
            dto.id,
            2,
            false,
        )
        .await
        .unwrap();
    assert_eq!(
        f.interactor
            .detail(&author, dto.id)
            .await
            .unwrap()
            .media
            .reference_count,
        1
    );
    assert_eq!(
        f.interactor
            .list(&author, 1, false, None)
            .await
            .unwrap()
            .total,
        1
    );
    assert!(matches!(
        f.interactor.read_metadata(Uuid::now_v7()).await,
        Err(UseCaseError::NotFound(_))
    ));
}

#[tokio::test]
async fn library_requires_permission_and_valid_page() {
    let f = fixture();
    let owner = Uuid::now_v7();
    upload(&f, owner).await;
    assert!(matches!(
        f.interactor.list(&actor(owner, &[]), 1, false, None).await,
        Err(UseCaseError::Forbidden)
    ));
    for page in [0, -1, i64::MAX] {
        assert!(matches!(
            f.interactor
                .list(&actor(owner, &["media.read"]), page, false, None)
                .await,
            Err(UseCaseError::Invalid(_))
        ));
    }
}

#[tokio::test]
async fn cleanup_only_removes_stale_staging_files_and_requires_operator_permission() {
    let f = fixture();
    let owner = Uuid::now_v7();
    let dto = upload(&f, owner).await;
    f.storage.put_staged("orphan", b"partial").await.unwrap();
    assert_eq!(
        f.interactor
            .cleanup_staging(&actor(owner, &["media.delete_any"]))
            .await
            .unwrap(),
        0
    );
    f.clock.advance(STAGED_GRACE_SECS + 1);
    f.storage.put_staged("recent", b"inflight").await.unwrap();
    assert!(matches!(
        f.interactor
            .cleanup_staging(&actor(owner, &["media.delete"]))
            .await,
        Err(UseCaseError::Forbidden)
    ));
    assert_eq!(
        f.interactor
            .cleanup_staging(&actor(owner, &["media.delete_any"]))
            .await
            .unwrap(),
        1
    );
    assert!(f.storage.has("staging/recent"));
    assert_eq!(read(&f, dto.id).await, png_bytes(10, 10));
}

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
    assert_eq!(view.media.reference_count, 4, "引用计数必须保持全局");

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
        view.references
            .iter()
            .all(|r| r.source.kind() == MediaContentKind::Post),
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

    // 软删除保留所有引用，不要求调用者能看到每个来源。
    fixture
        .interactor
        .set_deleted(
            &actor(owner, &["media.delete"]),
            media.id,
            media.version,
            true,
        )
        .await
        .unwrap();
    assert_eq!(
        fixture
            .interactor
            .detail(&author, media.id)
            .await
            .unwrap()
            .media
            .reference_count,
        4
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
