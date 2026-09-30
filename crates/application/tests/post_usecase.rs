//! 文章用例测试：内存 fake 仓储验证用例、归属与失败路径，不依赖生产 infrastructure。

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use application::content::{CreatePostCmd, EditPostCmd, PostInteractor};
use application::error::{ConflictKind, UseCaseError};
use application::identity::{
    Actor, ActorChannel, CreateUserCmd, PermissionDescriptor, RoleInteractor, UserInteractor,
};
use application::ports::{
    Clock, PostRepository, RbacStore, RoleDto, SaveOutcome, TagDeleteOutcome, TagRepository,
    TagWithUsage,
};
use domain::content::TagSnapshot;
use domain::content::{PostSnapshot, Visibility};
use domain::identity::{PermissionSet, UserSnapshot};
use time::OffsetDateTime;
use uuid::Uuid;

mod common;

// ---------------------------------------------------------------------------
// Fakes
// ---------------------------------------------------------------------------

struct FixedClock;

impl Clock for FixedClock {
    fn now(&self) -> OffsetDateTime {
        time::macros::datetime!(2026-09-21 12:00:00 UTC)
    }
}

struct FakePostRepo {
    posts: Mutex<HashMap<String, PostSnapshot>>, // key: slug
    /// post_id → 标签 id 集合（已排序），模拟 post_tags。
    tags: Mutex<HashMap<Uuid, Vec<Uuid>>>,
}

impl FakePostRepo {
    fn new() -> Self {
        Self {
            posts: Mutex::new(HashMap::new()),
            tags: Mutex::new(HashMap::new()),
        }
    }

    fn commit_record(
        &self,
        post: &domain::content::Post,
        expected: i64,
        now: OffsetDateTime,
        tag_ids: Option<&[Uuid]>,
        lifecycle: bool,
    ) -> Result<application::ports::PostCommitOutcome, UseCaseError> {
        use application::ports::{PostCommitOutcome, PostRecord};
        let mut snapshot = post.snapshot();
        let mut posts = self.posts.lock().unwrap();
        let mut tags = self.tags.lock().unwrap();
        let Some(current) = posts.values().find(|s| s.id == snapshot.id).cloned() else {
            return Ok(PostCommitOutcome::Gone);
        };
        if current.version != expected {
            return Ok(PostCommitOutcome::StaleConflict);
        }
        if (!lifecycle && current.deleted_at.is_some())
            || (lifecycle && current.deleted_at.is_some() == snapshot.deleted_at.is_some())
        {
            return Ok(PostCommitOutcome::Gone);
        }
        if posts
            .get(&snapshot.slug)
            .is_some_and(|s| s.id != snapshot.id)
        {
            return Err(UseCaseError::Conflict(ConflictKind::Slug));
        }
        snapshot.version = expected + 1;
        snapshot.updated_at = now;
        posts.remove(&current.slug);
        posts.insert(snapshot.slug.clone(), snapshot.clone());
        if let Some(ids) = tag_ids {
            tags.insert(snapshot.id, ids.to_vec());
        }
        let tag_ids = tags.get(&snapshot.id).cloned().unwrap_or_default();
        Ok(PostCommitOutcome::Saved(Box::new(PostRecord {
            snapshot,
            tag_ids,
        })))
    }
}

#[async_trait::async_trait]
impl PostRepository for FakePostRepo {
    async fn find_record_by_id(
        &self,
        id: Uuid,
    ) -> Result<Option<application::ports::PostRecord>, UseCaseError> {
        let posts = self.posts.lock().unwrap();
        let tags = self.tags.lock().unwrap();
        Ok(posts
            .values()
            .find(|s| s.id == id)
            .map(|snapshot| application::ports::PostRecord {
                snapshot: snapshot.clone(),
                tag_ids: tags.get(&id).cloned().unwrap_or_default(),
            }))
    }

    async fn insert_post(
        &self,
        post: &domain::content::Post,
        tag_ids: &[Uuid],
        _actor_id: application::audit::AuditContext,
    ) -> Result<application::ports::PostRecord, UseCaseError> {
        let snapshot = post.snapshot();
        let mut posts = self.posts.lock().unwrap();
        let mut tags = self.tags.lock().unwrap();
        if posts.contains_key(&snapshot.slug) {
            return Err(UseCaseError::Conflict(ConflictKind::Slug));
        }
        posts.insert(snapshot.slug.clone(), snapshot.clone());
        tags.insert(snapshot.id, tag_ids.to_vec());
        Ok(application::ports::PostRecord {
            snapshot,
            tag_ids: tag_ids.to_vec(),
        })
    }

    async fn commit_post(
        &self,
        post: &domain::content::Post,
        expected: i64,
        now: OffsetDateTime,
        tag_ids: Option<&[Uuid]>,
        _actor_id: application::audit::AuditContext,
    ) -> Result<application::ports::PostCommitOutcome, UseCaseError> {
        self.commit_record(post, expected, now, tag_ids, false)
    }

    async fn commit_lifecycle(
        &self,
        post: &domain::content::Post,
        expected: i64,
        now: OffsetDateTime,
        _actor_id: application::audit::AuditContext,
    ) -> Result<application::ports::PostCommitOutcome, UseCaseError> {
        self.commit_record(post, expected, now, None, true)
    }

    async fn find_by_id(&self, id: uuid::Uuid) -> Result<Option<PostSnapshot>, UseCaseError> {
        Ok(self
            .posts
            .lock()
            .unwrap()
            .values()
            .find(|p| p.id == id)
            .cloned())
    }

    async fn purge(
        &self,
        id: Uuid,
        expected_version: i64,
        _actor_id: application::audit::AuditContext,
    ) -> Result<SaveOutcome, UseCaseError> {
        let mut posts = self.posts.lock().unwrap();
        let Some(post) = posts
            .values()
            .find(|p| p.id == id && p.deleted_at.is_some())
        else {
            return Ok(SaveOutcome::Gone);
        };
        if post.version != expected_version {
            return Ok(SaveOutcome::StaleConflict);
        }
        let key = post.slug.clone();
        posts.remove(&key);
        self.tags.lock().unwrap().remove(&id);
        Ok(SaveOutcome::Saved {
            new_version: expected_version + 1,
        })
    }
}

struct FakeUserRepo {
    users: Mutex<HashMap<String, UserSnapshot>>, // key: username
}

impl FakeUserRepo {
    fn new() -> Self {
        Self {
            users: Mutex::new(HashMap::new()),
        }
    }
}

#[async_trait::async_trait]
impl application::ports::UserQuery for FakeUserRepo {
    async fn find_by_id(&self, id: uuid::Uuid) -> Result<Option<UserSnapshot>, UseCaseError> {
        Ok(self
            .users
            .lock()
            .unwrap()
            .values()
            .find(|u| u.id == id)
            .cloned())
    }

    async fn find_by_username(&self, username: &str) -> Result<Option<UserSnapshot>, UseCaseError> {
        Ok(self.users.lock().unwrap().get(username).cloned())
    }

    async fn list_admin(
        &self,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<application::ports::AdminUserRow>, UseCaseError> {
        let mut users: Vec<UserSnapshot> = self.users.lock().unwrap().values().cloned().collect();
        users.sort_by(|a, b| a.username.cmp(&b.username));
        Ok(users
            .into_iter()
            .skip(offset.max(0) as usize)
            .take(limit.max(0) as usize)
            .map(|u| application::ports::AdminUserRow {
                id: u.id,
                username: u.username,
                email: u.email,
                display_name: u.display_name,
                status: u.status,
                version: u.version,
                deleted: u.deleted_at.is_some(),
                password_enabled: false,
                external_identities: 0,
            })
            .collect())
    }
}

#[async_trait::async_trait]
impl application::ports::UserProfileStore for FakeUserRepo {
    async fn save_profile(
        &self,
        user: &domain::identity::User,
        expected_version: i64,
        now: time::OffsetDateTime,
        _audit: application::audit::AuditContext,
    ) -> Result<UserSnapshot, UseCaseError> {
        let snapshot = user.snapshot();
        let mut users = self.users.lock().unwrap();
        let current = users
            .values_mut()
            .find(|u| u.id == snapshot.id)
            .ok_or_else(|| UseCaseError::NotFound("用户".into()))?;
        if current.version != expected_version || !current.is_active() {
            return Err(UseCaseError::VersionConflict);
        }
        current.display_name = snapshot.display_name;
        current.bio = snapshot.bio;
        current.version += 1;
        current.updated_at = now;
        Ok(current.clone())
    }

    async fn set_avatar(
        &self,
        user_id: uuid::Uuid,
        avatar_media_id: Option<uuid::Uuid>,
        expected_version: i64,
        now: time::OffsetDateTime,
        _audit: application::audit::AuditContext,
    ) -> Result<UserSnapshot, UseCaseError> {
        let mut users = self.users.lock().unwrap();
        let Some(user) = users.values_mut().find(|u| u.id == user_id) else {
            return Err(UseCaseError::NotFound("用户".into()));
        };
        if !user.is_active() || user.version != expected_version {
            return Err(UseCaseError::VersionConflict);
        }
        user.version += 1;
        user.avatar_media_id = avatar_media_id;
        user.updated_at = now;
        Ok(user.clone())
    }
}

#[async_trait::async_trait]
impl application::ports::AccountAdministration for FakeUserRepo {
    async fn insert(
        &self,
        aggregate: &domain::identity::User,
        _audit_actor: application::audit::AuditContext,
    ) -> Result<(), UseCaseError> {
        let snapshot = aggregate.snapshot();
        let mut users = self.users.lock().unwrap();
        if users.contains_key(&snapshot.username) {
            return Err(UseCaseError::Conflict(ConflictKind::Username));
        }
        users.insert(snapshot.username.clone(), snapshot.clone());
        Ok(())
    }

    async fn change_status(
        &self,
        _user_id: uuid::Uuid,
        _status: domain::identity::UserStatus,
        _expected_version: i64,
        _now: time::OffsetDateTime,
        _actor: &application::identity::Actor,
    ) -> Result<UserSnapshot, UseCaseError> {
        unimplemented!("此用例不修改账号状态")
    }

    async fn revoke_authentication(
        &self,
        user_id: Uuid,
        _audit_actor: application::audit::AuditContext,
    ) -> Result<(), UseCaseError> {
        let mut users = self.users.lock().unwrap();
        let user = users
            .values_mut()
            .find(|u| u.id == user_id)
            .ok_or_else(|| UseCaseError::NotFound("用户".into()))?;
        user.auth_version += 1;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// 测试装配
// ---------------------------------------------------------------------------

/// 标签目录 fake：文章用例只依赖 existing_ids（存在性校验）。
struct FakeTagRepo {
    tags: Mutex<Vec<TagSnapshot>>,
}

impl FakeTagRepo {
    fn new() -> Self {
        Self {
            tags: Mutex::new(Vec::new()),
        }
    }

    fn add(&self, name: &str, slug: &str) -> Uuid {
        let snapshot = TagSnapshot {
            id: Uuid::now_v7(),
            name: name.into(),
            slug: slug.into(),
            version: 1,
            created_at: OffsetDateTime::now_utc(),
        };
        let id = snapshot.id;
        self.tags.lock().unwrap().push(snapshot);
        id
    }
}

#[async_trait::async_trait]
impl TagRepository for FakeTagRepo {
    async fn insert(
        &self,
        aggregate: &domain::content::Tag,
        _actor_id: application::audit::AuditContext,
    ) -> Result<(), UseCaseError> {
        let snapshot = aggregate.snapshot();
        self.tags.lock().unwrap().push(snapshot.clone());
        Ok(())
    }

    async fn find_by_slug(&self, slug: &str) -> Result<Option<TagSnapshot>, UseCaseError> {
        Ok(self
            .tags
            .lock()
            .unwrap()
            .iter()
            .find(|t| t.slug == slug)
            .cloned())
    }

    async fn list(&self) -> Result<Vec<TagWithUsage>, UseCaseError> {
        let mut rows: Vec<TagWithUsage> = self
            .tags
            .lock()
            .unwrap()
            .iter()
            .map(|snapshot| TagWithUsage {
                snapshot: snapshot.clone(),
                public_post_count: 0,
            })
            .collect();
        rows.sort_by(|a, b| a.snapshot.slug.cmp(&b.snapshot.slug));
        Ok(rows)
    }

    async fn rename(
        &self,
        id: Uuid,
        new_name: &str,
        expected_version: i64,
        _actor_id: application::audit::AuditContext,
    ) -> Result<Option<TagWithUsage>, UseCaseError> {
        let mut tags = self.tags.lock().unwrap();
        let Some(tag) = tags.iter_mut().find(|t| t.id == id) else {
            return Ok(None);
        };
        if tag.version != expected_version {
            return Ok(None);
        }
        if tag.name != new_name {
            tag.name = new_name.into();
            tag.version += 1;
        }
        Ok(Some(TagWithUsage {
            snapshot: tag.clone(),
            public_post_count: 0,
        }))
    }

    async fn delete(
        &self,
        id: Uuid,
        expected_version: i64,
        _actor_id: application::audit::AuditContext,
    ) -> Result<TagDeleteOutcome, UseCaseError> {
        let mut tags = self.tags.lock().unwrap();
        let Some(tag) = tags.iter().find(|t| t.id == id) else {
            return Ok(TagDeleteOutcome::Gone);
        };
        if tag.version != expected_version {
            return Ok(TagDeleteOutcome::StaleVersion);
        }
        tags.retain(|t| t.id != id);
        Ok(TagDeleteOutcome::Deleted)
    }
}

/// 文章关联查找恒报存在；系列更新可提供固定的已提交结果。
#[derive(Default)]
struct FakeSeriesRepo {
    lookups: Mutex<Vec<Vec<Uuid>>>,
    current: Option<domain::content::SeriesSnapshot>,
    committed: Option<application::ports::SeriesWithUsage>,
}

#[async_trait::async_trait]
impl application::ports::SeriesRepository for FakeSeriesRepo {
    async fn insert(
        &self,
        aggregate: &domain::content::Series,
        _actor_id: application::audit::AuditContext,
    ) -> Result<(), UseCaseError> {
        let _snapshot = aggregate.snapshot();
        Ok(())
    }
    async fn find_by_slug(
        &self,
        _slug: &str,
    ) -> Result<Option<domain::content::SeriesSnapshot>, UseCaseError> {
        Ok(self.current.clone())
    }
    async fn list(&self) -> Result<Vec<application::ports::SeriesWithUsage>, UseCaseError> {
        Err(UseCaseError::Repository("目录查询不可用".into()))
    }
    async fn update(
        &self,
        _id: uuid::Uuid,
        _name: &str,
        _description: Option<&str>,
        _cover_media_id: Option<uuid::Uuid>,
        _expected_version: i64,
        _actor_id: application::audit::AuditContext,
    ) -> Result<Option<application::ports::SeriesWithUsage>, UseCaseError> {
        Ok(self.committed.clone())
    }
    async fn delete(
        &self,
        _id: uuid::Uuid,
        _expected_version: i64,
        _actor_id: application::audit::AuditContext,
    ) -> Result<application::ports::SeriesDeleteOutcome, UseCaseError> {
        Ok(application::ports::SeriesDeleteOutcome::Gone)
    }

    async fn members_of(
        &self,
        _series_id: uuid::Uuid,
    ) -> Result<Vec<application::ports::SeriesMember>, UseCaseError> {
        Ok(Vec::new())
    }
    async fn reorder(
        &self,
        _series_id: uuid::Uuid,
        _expected: i64,
        _ordered: &[uuid::Uuid],
        _actor_id: application::audit::AuditContext,
    ) -> Result<application::ports::ReorderOutcome, UseCaseError> {
        Ok(application::ports::ReorderOutcome::Reordered { new_version: 1 })
    }
}

/// 文章关联查找恒报存在；分类更新可提供固定的已提交结果。
#[derive(Default)]
struct FakeCategoryRepo {
    current: Option<domain::content::CategorySnapshot>,
    committed: Option<application::ports::CategoryWithUsage>,
}

#[async_trait::async_trait]
impl application::ports::CategoryRepository for FakeCategoryRepo {
    async fn insert(
        &self,
        aggregate: &domain::content::Category,
        _audit_actor: application::audit::AuditContext,
    ) -> Result<(), UseCaseError> {
        let _snapshot = aggregate.snapshot();
        Ok(())
    }
    async fn find_by_slug(
        &self,
        _slug: &str,
    ) -> Result<Option<domain::content::CategorySnapshot>, UseCaseError> {
        Ok(self.current.clone())
    }
    async fn list(&self) -> Result<Vec<application::ports::CategoryWithUsage>, UseCaseError> {
        Err(UseCaseError::Repository("目录查询不可用".into()))
    }
    async fn update(
        &self,
        _id: uuid::Uuid,
        _name: &str,
        _description: Option<&str>,
        _parent_id: Option<uuid::Uuid>,
        _expected_version: i64,
        _audit_actor: application::audit::AuditContext,
    ) -> Result<Option<application::ports::CategoryWithUsage>, UseCaseError> {
        Ok(self.committed.clone())
    }
    async fn delete(
        &self,
        _id: uuid::Uuid,
        _expected_version: i64,
        _audit_actor: application::audit::AuditContext,
    ) -> Result<application::ports::CategoryDeleteOutcome, UseCaseError> {
        Ok(application::ports::CategoryDeleteOutcome::Gone)
    }
}

struct FakeRbacStore {
    roles: std::sync::Mutex<HashMap<String, Vec<&'static str>>>,
    assignments: std::sync::Mutex<HashSet<(Uuid, String)>>,
}

impl FakeRbacStore {
    fn new() -> Self {
        Self {
            roles: std::sync::Mutex::new(HashMap::new()),
            assignments: std::sync::Mutex::new(HashSet::new()),
        }
    }

    fn owners(&self) -> i64 {
        self.assignments
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, slug)| slug == "admin")
            .count() as i64
    }
}

#[async_trait::async_trait]
impl RbacStore for FakeRbacStore {
    async fn sync_permission_registry(
        &self,
        _entries: &[PermissionDescriptor],
    ) -> Result<(), UseCaseError> {
        Ok(())
    }

    async fn sync_builtin_roles(
        &self,
        defs: &[application::identity::BuiltinRoleDef],
    ) -> Result<(), UseCaseError> {
        let mut roles = self.roles.lock().unwrap();
        for def in defs {
            roles.insert(def.slug.to_string(), def.permissions.to_vec());
        }
        Ok(())
    }

    async fn permissions_of_user(
        &self,
        user_id: Uuid,
    ) -> Result<domain::identity::PermissionSet, UseCaseError> {
        let roles = self.roles.lock().unwrap();
        let assignments = self.assignments.lock().unwrap();
        let mut keys = Vec::new();
        for (uid, slug) in assignments.iter() {
            if *uid == user_id
                && let Some(perms) = roles.get(slug)
            {
                keys.extend(perms.iter().copied());
            }
        }
        Ok(domain::identity::PermissionSet::from_keys(keys))
    }

    async fn permissions_of_role(
        &self,
        role_slug: &str,
    ) -> Result<domain::identity::PermissionSet, UseCaseError> {
        let roles = self.roles.lock().unwrap();
        let perms = roles
            .get(role_slug)
            .ok_or_else(|| UseCaseError::NotFound(format!("角色 {role_slug}")))?;
        Ok(domain::identity::PermissionSet::from_keys(
            perms.iter().copied(),
        ))
    }

    async fn assign_role(
        &self,
        user_id: Uuid,
        role_slug: &str,
        _audit_actor: application::audit::AuditContext,
    ) -> Result<(), UseCaseError> {
        if !self.roles.lock().unwrap().contains_key(role_slug) {
            return Err(UseCaseError::NotFound(format!("角色 {role_slug}")));
        }
        self.assignments
            .lock()
            .unwrap()
            .insert((user_id, role_slug.to_string()));
        Ok(())
    }

    async fn remove_role(
        &self,
        user_id: Uuid,
        role_slug: &str,
        _audit_actor: application::audit::AuditContext,
    ) -> Result<(), UseCaseError> {
        if role_slug == "admin" && self.owners() <= 1 {
            return Err(UseCaseError::LastAdminProtected);
        }
        self.assignments
            .lock()
            .unwrap()
            .remove(&(user_id, role_slug.to_string()));
        Ok(())
    }

    async fn list_roles(&self) -> Result<Vec<RoleDto>, UseCaseError> {
        let roles = self.roles.lock().unwrap();
        Ok(roles
            .iter()
            .map(|(slug, perms)| RoleDto {
                slug: slug.clone(),
                name: format!("{slug}-name"),
                description: None,
                builtin: true,
                permission_count: perms.len() as i64,
            })
            .collect())
    }

    async fn roles_of_user(&self, user_id: Uuid) -> Result<Vec<String>, UseCaseError> {
        let assignments = self.assignments.lock().unwrap();
        Ok(assignments
            .iter()
            .filter(|(uid, _)| *uid == user_id)
            .map(|(_, slug)| slug.clone())
            .collect())
    }

    async fn roles_of_users(&self, user_ids: &[Uuid]) -> Result<Vec<(Uuid, String)>, UseCaseError> {
        let assignments = self.assignments.lock().unwrap();
        let mut rows: Vec<(Uuid, String)> = assignments
            .iter()
            .filter(|(uid, _)| user_ids.contains(uid))
            .map(|(uid, slug)| (*uid, slug.clone()))
            .collect();
        rows.sort();
        Ok(rows)
    }

    async fn loginable_admin_count(&self) -> Result<i64, UseCaseError> {
        // Fake 身份没有登录方式概念：owner 分配数即「可登录 Owner」数。
        Ok(self.owners())
    }
}

struct Fixture {
    posts: Arc<PostInteractor>,
    post_repo: Arc<FakePostRepo>,
    /// 供测试预置标签目录（文章-标签关联用例）。
    tags: Arc<FakeTagRepo>,
    series: Arc<FakeSeriesRepo>,
    users: Arc<UserInteractor>,
    roles: Arc<RoleInteractor>,
    /// 媒体附着授权 fake：测试登记资产的归属与公开性。
    media_guard: Arc<common::FakeMediaGuard>,
    author: Actor,
    author2: Actor,
    other: Actor,
    editor: Actor,
}

async fn fixture() -> Fixture {
    let post_repo = Arc::new(FakePostRepo::new());
    let tag_repo = Arc::new(FakeTagRepo::new());
    let series_repo = Arc::new(FakeSeriesRepo::default());
    let user_repo = Arc::new(FakeUserRepo::new());
    let rbac = Arc::new(FakeRbacStore::new());
    let clock = Arc::new(FixedClock);

    let media_guard = Arc::new(common::FakeMediaGuard::new());
    let users = Arc::new(UserInteractor::new(
        application::identity::UserStores {
            query: user_repo.clone(),
            profiles: user_repo.clone(),
            accounts: user_repo.clone(),
        },
        rbac.clone(),
        clock.clone(),
        media_guard.clone(),
    ));
    let roles = Arc::new(RoleInteractor::new(rbac.clone(), user_repo));
    let posts = Arc::new(PostInteractor::new(
        post_repo.clone(),
        tag_repo.clone(),
        Arc::new(FakeCategoryRepo::default()),
        series_repo.clone(),
        clock,
        media_guard.clone(),
    ));

    roles.sync_registry().await.unwrap();

    for username in ["author", "author2", "other", "editor"] {
        users
            .create_user(
                &Actor::bootstrap_cli(),
                CreateUserCmd {
                    username: username.into(),
                    email: None,
                    display_name: Some(format!("{username}的展示名")),
                },
            )
            .await
            .unwrap();
    }
    // author 获 author 角色；editor 获得 any 权限角色；other 无任何角色。
    let bootstrap = Actor::bootstrap_cli();
    roles
        .assign_to_username(&bootstrap, "author", "author")
        .await
        .unwrap();
    roles
        .assign_to_username(&bootstrap, "author2", "author")
        .await
        .unwrap();
    roles
        .assign_to_username(&bootstrap, "editor", "editor")
        .await
        .unwrap();

    let author = users.actor_for_username("author").await.unwrap();
    let author2 = users.actor_for_username("author2").await.unwrap();
    let other = users.actor_for_username("other").await.unwrap();
    let editor = users.actor_for_username("editor").await.unwrap();

    Fixture {
        posts,
        post_repo,
        tags: tag_repo,
        series: series_repo,
        users,
        roles,
        media_guard,
        author,
        author2,
        other,
        editor,
    }
}

fn draft_cmd(slug: &str) -> CreatePostCmd {
    CreatePostCmd {
        slug: Some(slug.into()),
        title: "第一篇".into(),
        excerpt: Some("摘要".into()),
        content: "# Hello\n\n正文内容".into(),
        visibility: Visibility::Public,
        tag_ids: Vec::new(),
        category_id: None,
        series: Vec::new(),
        cover_media_id: None,
    }
}

#[tokio::test]
async fn series_inputs_are_validated_before_one_batch_lookup() {
    use application::content::{MAX_SERIES_PER_POST, SeriesPlacement};
    let f = fixture().await;
    let base = f
        .posts
        .create(&f.author, draft_cmd("series-inputs"))
        .await
        .unwrap();
    let placement = SeriesPlacement {
        series_id: Uuid::now_v7(),
        position: 0,
    };
    let valid: Vec<_> = (0..MAX_SERIES_PER_POST)
        .map(|_| SeriesPlacement {
            series_id: Uuid::now_v7(),
            position: 0,
        })
        .collect();
    let mut oversized = valid.clone();
    oversized.push(placement);
    for invalid in [
        vec![placement, placement],
        vec![SeriesPlacement {
            position: -1,
            ..placement
        }],
        oversized,
    ] {
        let mut cmd = draft_cmd("invalid-series");
        cmd.series = invalid.clone();
        assert!(matches!(
            f.posts.create(&f.author, cmd).await,
            Err(UseCaseError::Invalid(_))
        ));
        assert!(matches!(
            f.posts
                .edit(
                    &f.author,
                    EditPostCmd {
                        id: base.id,
                        series: Some(invalid),
                        ..Default::default()
                    }
                )
                .await,
            Err(UseCaseError::Invalid(_))
        ));
    }
    assert!(f.series.lookups.lock().unwrap().is_empty());

    let mut cmd = draft_cmd("valid-series");
    cmd.series = valid.clone();
    assert_eq!(
        f.posts.create(&f.author, cmd).await.unwrap().series.len(),
        MAX_SERIES_PER_POST
    );
    assert_eq!(
        f.posts
            .edit(
                &f.author,
                EditPostCmd {
                    id: base.id,
                    series: Some(valid),
                    ..Default::default()
                }
            )
            .await
            .unwrap()
            .series
            .len(),
        MAX_SERIES_PER_POST
    );
    {
        let lookups = f.series.lookups.lock().unwrap();
        assert_eq!(lookups.len(), 2, "创建和编辑各查询一次完整集合");
        assert!(lookups.iter().all(|ids| ids.len() == MAX_SERIES_PER_POST));
    }

    // 历史文章可能超过新预算；编辑器反向重发同一集合仍应允许只改标题。
    let mut legacy_series: Vec<SeriesPlacement> = {
        let mut stored = f.post_repo.posts.lock().unwrap();
        let snapshot = stored.get_mut("series-inputs").unwrap();
        snapshot.series.push(placement.into());
        snapshot
            .series
            .iter()
            .rev()
            .copied()
            .map(Into::into)
            .collect()
    };
    let edited = f
        .posts
        .edit(
            &f.author,
            EditPostCmd {
                id: base.id,
                title: Some("Updated legacy title".into()),
                series: Some(legacy_series.clone()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(edited.title, "Updated legacy title");
    assert_eq!(edited.series.len(), MAX_SERIES_PER_POST + 1);
    // 改权重是真实关联变更，仍受预算约束，不能作为同值重发放行。
    legacy_series[0].position += 1;
    assert!(matches!(
        f.posts
            .edit(
                &f.author,
                EditPostCmd {
                    id: base.id,
                    series: Some(legacy_series),
                    ..Default::default()
                }
            )
            .await,
        Err(UseCaseError::Invalid(_))
    ));
    assert_eq!(f.series.lookups.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn catalog_updates_return_committed_records_without_catalog_reads() {
    let current = domain::content::Series::new(
        "Before".into(),
        domain::content::Slug::new("guide").unwrap(),
        None,
        FixedClock.now(),
    )
    .unwrap()
    .snapshot();
    let mut saved = current.clone();
    saved.name = "After".into();
    saved.version += 1;
    let repo = Arc::new(FakeSeriesRepo {
        current: Some(current),
        committed: Some(application::ports::SeriesWithUsage {
            snapshot: saved.clone(),
            post_count: 3,
            public_post_count: 1,
        }),
        ..Default::default()
    });
    let usecase = application::series::SeriesInteractor::new(
        repo,
        Arc::new(FixedClock),
        Arc::new(common::FakeMediaGuard::new()),
    );
    let result = usecase
        .update(
            &Actor::bootstrap_cli(),
            "guide",
            application::series::UpdateSeriesCmd {
                name: "After".into(),
                expected_version: Some(1),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!((result.name, result.version), (saved.name, saved.version));
    assert_eq!((result.post_count, result.public_post_count), (Some(3), 1));

    let current = domain::content::Category::new(
        "Before".into(),
        domain::content::Slug::new("category").unwrap(),
        None,
        None,
        FixedClock.now(),
    )
    .unwrap()
    .snapshot();
    let saved = current.clone();
    let categories = application::category::CategoryInteractor::new(
        Arc::new(FakeCategoryRepo {
            current: Some(current),
            committed: Some(application::ports::CategoryWithUsage {
                snapshot: saved.clone(),
                public_post_count: 2,
            }),
        }),
        Arc::new(FixedClock),
    );
    let result = categories
        .update(
            &Actor::bootstrap_cli(),
            "category",
            application::category::UpdateCategoryCmd {
                name: "Before".into(),
                expected_version: Some(1),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(
        (result.name, result.version, result.public_post_count),
        (saved.name, saved.version, 2)
    );
}

#[tokio::test]
async fn create_edit_publish_withdraw_flow() {
    let f = fixture().await;

    let created = f
        .posts
        .create(&f.author, draft_cmd("first-post"))
        .await
        .unwrap();
    assert_eq!(created.status, "draft");
    assert_eq!(created.version, 1);

    let edited = f
        .posts
        .edit(
            &f.author,
            EditPostCmd {
                id: created.id,
                title: Some("第一篇（改）".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(edited.version, 2, "有实际变化才递增 version");

    let published = f.posts.publish(&f.author, created.id, None).await.unwrap();
    assert_eq!(published.status, "published");
    assert!(published.published_at.is_some());
    assert_eq!(published.version, 3);

    // 重复发布幂等：无变化不递增版本。
    let again = f.posts.publish(&f.author, created.id, None).await.unwrap();
    assert_eq!(again.version, 3);

    let withdrawn = f.posts.withdraw(&f.author, created.id, None).await.unwrap();
    assert_eq!(withdrawn.status, "draft");
    assert_eq!(withdrawn.version, 4);
    assert!(withdrawn.published_at.is_some(), "保留首次发布时间");
}

#[tokio::test]
async fn no_op_writes_reject_stale_versions_without_exposing_a_new_baseline() {
    let f = fixture().await;
    let slug = "stale-noop";
    let original = f.posts.create(&f.author, draft_cmd(slug)).await.unwrap();
    let edited = f
        .posts
        .edit(
            &f.author,
            EditPostCmd {
                id: original.id,
                content: Some("另一位编辑的新正文".into()),
                expected_version: Some(original.version),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let published = f
        .posts
        .publish(&f.author, original.id, Some(edited.version))
        .await
        .unwrap();

    // 另一位编辑已发布：旧页面再点发布必须冲突，不能取得新版本后覆盖旧正文。
    assert!(matches!(
        f.posts
            .publish(&f.author, original.id, Some(original.version))
            .await,
        Err(UseCaseError::VersionConflict)
    ));
    let again = f
        .posts
        .publish(&f.author, original.id, Some(published.version))
        .await
        .unwrap();
    assert_eq!(again.version, published.version);
    assert_eq!(again.content, edited.content);

    let withdrawn = f
        .posts
        .withdraw(&f.author, original.id, Some(published.version))
        .await
        .unwrap();
    assert!(matches!(
        f.posts
            .withdraw(&f.author, original.id, Some(published.version))
            .await,
        Err(UseCaseError::VersionConflict)
    ));
    assert_eq!(
        f.posts
            .withdraw(&f.author, original.id, Some(withdrawn.version))
            .await
            .unwrap()
            .version,
        withdrawn.version
    );

    // 空 PATCH 同样校验版本；当前版本的空 PATCH 保持幂等。
    assert!(matches!(
        f.posts
            .edit(
                &f.author,
                EditPostCmd {
                    id: original.id,
                    expected_version: Some(original.version),
                    ..Default::default()
                }
            )
            .await,
        Err(UseCaseError::VersionConflict)
    ));
    let unchanged = f
        .posts
        .edit(
            &f.author,
            EditPostCmd {
                id: original.id,
                expected_version: Some(withdrawn.version),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(unchanged.version, withdrawn.version);
    assert_eq!(unchanged.content, edited.content);
}

#[tokio::test]
async fn concurrent_edit_detects_version_conflict() {
    let f = fixture().await;
    let created = f.posts.create(&f.author, draft_cmd("race")).await.unwrap();

    // 两个调用方都基于 version=1 提交编辑。
    let a = f
        .posts
        .edit(
            &f.author,
            EditPostCmd {
                id: created.id,
                content: Some("A 的修改".into()),
                expected_version: Some(1),
                ..Default::default()
            },
        )
        .await;

    let b = f
        .posts
        .edit(
            &f.author,
            EditPostCmd {
                id: created.id,
                content: Some("B 的修改".into()),
                expected_version: Some(1),
                ..Default::default()
            },
        )
        .await;

    assert!(a.is_ok());
    assert!(
        matches!(b, Err(UseCaseError::VersionConflict)),
        "第二个提交必须报版本冲突，而不是静默覆盖"
    );
}

#[tokio::test]
async fn truly_parallel_edits_exactly_one_wins() {
    let f = fixture().await;
    let created = f
        .posts
        .create(&f.author, draft_cmd("parallel"))
        .await
        .unwrap();

    // 两个调用真正同时进行（join!），都基于 version=1。
    let edit = |content: &'static str| {
        f.posts.edit(
            &f.author,
            EditPostCmd {
                id: created.id,
                content: Some(content.into()),
                expected_version: Some(1),
                ..Default::default()
            },
        )
    };
    let (a, b) = tokio::join!(edit("A 的并发修改"), edit("B 的并发修改"));

    let ok_count = usize::from(a.is_ok()) + usize::from(b.is_ok());
    assert_eq!(ok_count, 1, "并发提交恰好一个成功，实际 a={a:?} b={b:?}");
    let conflict_count = usize::from(matches!(a, Err(UseCaseError::VersionConflict)))
        + usize::from(matches!(b, Err(UseCaseError::VersionConflict)));
    assert_eq!(conflict_count, 1);

    let shown = f.posts.find(&f.author, created.id).await.unwrap();
    assert_eq!(shown.version, 2, "恰好一次版本递增");
    // 胜者只改了 content，标题/摘要保持原样。
    assert_eq!(shown.title, "第一篇");
}

#[tokio::test]
async fn ownership_check_rejects_non_author() {
    let f = fixture().await;
    let created = f
        .posts
        .create(&f.author, draft_cmd("own-post"))
        .await
        .unwrap();

    let err = f
        .posts
        .publish(&f.other, created.id, None)
        .await
        .unwrap_err();
    assert!(matches!(err, UseCaseError::Forbidden));
}

#[tokio::test]
async fn publish_requires_content() {
    let f = fixture().await;
    let created = f
        .posts
        .create(
            &f.author,
            CreatePostCmd {
                slug: Some("empty".into()),
                title: String::new(),
                excerpt: None,
                content: String::new(),
                visibility: Visibility::Public,
                tag_ids: Vec::new(),
                category_id: None,
                series: Vec::new(),
                cover_media_id: None,
            },
        )
        .await
        .unwrap();

    let err = f
        .posts
        .publish(&f.author, created.id, None)
        .await
        .unwrap_err();
    match err {
        UseCaseError::Invalid(msg) => assert!(msg.contains("标题")),
        other => panic!("期望 Invalid，得到 {other:?}"),
    }
}

#[tokio::test]
async fn slug_taken_maps_to_conflict() {
    let f = fixture().await;
    f.posts.create(&f.author, draft_cmd("dup")).await.unwrap();
    let err = f
        .posts
        .create(&f.author, draft_cmd("dup"))
        .await
        .unwrap_err();
    assert!(matches!(err, UseCaseError::Conflict(_)));
}

#[tokio::test]
async fn generated_slug_occupied_at_creation() {
    let f = fixture().await;
    let created = f
        .posts
        .create(
            &f.author,
            CreatePostCmd {
                slug: None,
                title: "自动 slug".into(),
                excerpt: None,
                content: "内容".into(),
                visibility: Visibility::Public,
                tag_ids: Vec::new(),
                category_id: None,
                series: Vec::new(),
                cover_media_id: None,
            },
        )
        .await
        .unwrap();
    assert!(
        created.slug.starts_with("draft-"),
        "生成临时唯一 slug：{}",
        created.slug
    );
}

#[tokio::test]
async fn find_returns_current_state_for_cli() {
    let f = fixture().await;
    let created = f.posts.create(&f.author, draft_cmd("shown")).await.unwrap();
    let shown = f.posts.find(&f.author, created.id).await.unwrap();
    assert_eq!(shown.id, created.id);
    // deleted_at 过滤行为由 infrastructure 集成测试覆盖。
}

#[tokio::test]
async fn actor_resolution_rejects_unknown_user() {
    let f = fixture().await;
    let err = f.users.actor_for_username("ghost").await.unwrap_err();
    assert!(matches!(err, UseCaseError::NotFound(_)));
}

// ---------------------------------------------------------------------------
// RBAC 行为
// ---------------------------------------------------------------------------

#[tokio::test]
async fn editor_with_any_permission_can_edit_others_posts() {
    let f = fixture().await;
    let created = f
        .posts
        .create(&f.author, draft_cmd("editors-view"))
        .await
        .unwrap();

    // editor 无 own 授权，但 update_any 覆盖 own。
    let edited = f
        .posts
        .edit(
            &f.editor,
            EditPostCmd {
                id: created.id,
                title: Some("编辑改写".into()),
                ..Default::default()
            },
        )
        .await;
    assert!(edited.is_ok(), "update_any 应允许编辑他人文章：{edited:?}");

    let shown = f.posts.find(&f.editor, created.id).await.unwrap();
    assert_eq!(shown.title, "编辑改写");
}

#[tokio::test]
async fn user_without_permission_cannot_create_posts() {
    let f = fixture().await;
    let err = f
        .posts
        .create(&f.other, draft_cmd("no-perm"))
        .await
        .unwrap_err();
    assert!(
        matches!(err, UseCaseError::Forbidden),
        "无 post.create 应拒绝：{err:?}"
    );
}

#[tokio::test]
async fn reader_scope_blocks_others_drafts() {
    let f = fixture().await;
    let created = f
        .posts
        .create(&f.author, draft_cmd("secret-draft"))
        .await
        .unwrap();

    // other 无 read/read_any：既不能读也不能改。
    let err = f.posts.find(&f.other, created.id).await.unwrap_err();
    assert!(matches!(err, UseCaseError::Forbidden));
    let err = f
        .posts
        .edit(
            &f.other,
            EditPostCmd {
                id: created.id,
                title: Some("越权修改".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap_err();
    assert!(matches!(err, UseCaseError::Forbidden));

    // author2 有 own 权限（post.read/post.update）但不是作者：
    // 这里测的是「不是本人」，而不是「没有权限」。
    let err = f.posts.find(&f.author2, created.id).await.unwrap_err();
    assert!(
        matches!(err, UseCaseError::Forbidden),
        "own 权限不得跨作者读取：{err:?}"
    );
    let err = f
        .posts
        .edit(
            &f.author2,
            EditPostCmd {
                id: created.id,
                title: Some("越权修改".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap_err();
    assert!(
        matches!(err, UseCaseError::Forbidden),
        "own 权限不得跨作者编辑：{err:?}"
    );
    // author 本人可读自己的草稿（own）。
    assert!(f.posts.find(&f.author, created.id).await.is_ok());
    // editor 有 read_any，可读他人草稿。
    assert!(f.posts.find(&f.editor, created.id).await.is_ok());
}

#[tokio::test]
async fn author_cannot_publish_others_posts_without_any() {
    let f = fixture().await;
    let author2 = f.users.actor_for_username("author2").await.unwrap();
    let created = f
        .posts
        .create(&author2, draft_cmd("author2-owns"))
        .await
        .unwrap();

    // author 有 post.publish(own)，但文章属于 author2 → Forbidden。
    let err = f
        .posts
        .publish(&f.author, created.id, None)
        .await
        .unwrap_err();
    assert!(matches!(err, UseCaseError::Forbidden));

    // editor 有 publish_any，可发布同一篇。
    assert!(f.posts.publish(&f.editor, created.id, None).await.is_ok());
}

#[tokio::test]
async fn last_admin_cannot_be_removed() {
    let f = fixture().await;
    let bootstrap = Actor::bootstrap_cli();
    f.roles
        .assign_to_username(&bootstrap, "author", "admin")
        .await
        .unwrap();

    // 唯一 Owner：移除被拒（存储侧最后 Owner 保护）。
    let err = f
        .roles
        .remove_from_username(&bootstrap, "author", "admin")
        .await
        .unwrap_err();
    assert!(
        matches!(err, UseCaseError::LastAdminProtected),
        "最后 Owner 保护：{err:?}"
    );

    // 出现第二个 Owner 后，移除其中一个允许。
    f.roles
        .assign_to_username(&bootstrap, "editor", "admin")
        .await
        .unwrap();
    assert!(
        f.roles
            .remove_from_username(&bootstrap, "author", "admin")
            .await
            .is_ok()
    );
}

#[tokio::test]
async fn unknown_role_assignment_is_rejected() {
    let f = fixture().await;
    let err = f
        .roles
        .assign_to_username(&Actor::bootstrap_cli(), "author", "ghost-role")
        .await
        .unwrap_err();
    assert!(matches!(err, UseCaseError::NotFound(_)));
}

#[tokio::test]
async fn actor_permissions_are_union_of_roles() {
    let f = fixture().await;
    // author 再叠加 editor 角色 → 并集包含 own + any。
    f.roles
        .assign_to_username(&Actor::bootstrap_cli(), "author", "editor")
        .await
        .unwrap();
    let actor = f.users.actor_for_username("author").await.unwrap();
    assert!(actor.has_permission("post.create"), "own 动作仍在");
    assert!(actor.has_permission("post.update_any"), "any 动作并入");
}

// ---------------------------------------------------------------------------
// 身份写路径的 Actor 授权与委派上限（docs §3）
// ---------------------------------------------------------------------------

fn session_actor_with(keys: impl IntoIterator<Item = &'static str>) -> Actor {
    Actor::new(
        domain::identity::UserId::generate(),
        ActorChannel::Session,
        PermissionSet::from_keys(keys),
    )
}

fn all_registered_permissions() -> Vec<&'static str> {
    application::identity::PERMISSION_REGISTRY
        .iter()
        .map(|d| d.key)
        .collect()
}

#[tokio::test]
async fn create_user_requires_user_manage() {
    let f = fixture().await;
    let outsider = session_actor_with(["post.create"]);
    let err = f
        .users
        .create_user(
            &outsider,
            CreateUserCmd {
                username: "intruder".into(),
                email: None,
                display_name: None,
            },
        )
        .await
        .unwrap_err();
    assert!(
        matches!(err, UseCaseError::Forbidden),
        "缺 user.manage：{err:?}"
    );

    // 受控 CLI 引导身份持有全部已注册权限。
    assert!(
        f.users
            .create_user(
                &Actor::bootstrap_cli(),
                CreateUserCmd {
                    username: "invited".into(),
                    email: None,
                    display_name: None,
                },
            )
            .await
            .is_ok()
    );
}

#[tokio::test]
async fn assign_role_requires_role_manage() {
    let f = fixture().await;
    let outsider = session_actor_with(["post.create", "user.manage"]);
    let err = f
        .roles
        .assign_to_username(&outsider, "other", "author")
        .await
        .unwrap_err();
    assert!(
        matches!(err, UseCaseError::Forbidden),
        "缺 role.manage：{err:?}"
    );
}

#[tokio::test]
async fn owner_grant_requires_dedicated_ownership_permission() {
    let f = fixture().await;
    // 除 admin.manage 外持有全部已注册权限的会话：仍不能授予 Owner。
    let without_ownership: Vec<&'static str> = all_registered_permissions()
        .into_iter()
        .filter(|key| *key != "admin.manage")
        .collect();
    let err = f
        .roles
        .assign_to_username(&session_actor_with(without_ownership), "author", "admin")
        .await
        .unwrap_err();
    assert!(
        matches!(err, UseCaseError::Forbidden),
        "普通角色分配不能授予 Owner：{err:?}"
    );

    // 持有 admin.manage 后允许。
    assert!(
        f.roles
            .assign_to_username(
                &session_actor_with(all_registered_permissions()),
                "author",
                "admin"
            )
            .await
            .is_ok()
    );
}

#[tokio::test]
async fn delegation_ceiling_blocks_granting_unheld_permissions() {
    let f = fixture().await;
    // 只有 post.create + role.manage 的会话不能授予 author（后者含 post.publish 等）。
    let weak = session_actor_with(["post.create", "role.manage"]);
    let err = f
        .roles
        .assign_to_username(&weak, "other", "author")
        .await
        .unwrap_err();
    assert!(
        matches!(err, UseCaseError::Forbidden),
        "超出委派上限：{err:?}"
    );

    // 受控 CLI 引导身份持有全部已注册权限，可授予。
    assert!(
        f.roles
            .assign_to_username(&Actor::bootstrap_cli(), "other", "author")
            .await
            .is_ok()
    );
}

// ---------------------------------------------------------------------------
// 文章-标签关联：同事务语义、去重、存在性校验与授权
// ---------------------------------------------------------------------------

async fn tag_fixture() -> Fixture {
    let f = fixture().await;
    f.tags.add("Rust", "rust");
    f.tags.add("随笔", "essay");
    f
}

#[tokio::test]
async fn create_post_writes_initial_tags() {
    let f = tag_fixture().await;
    let rust_id = f.tags.find_by_slug("rust").await.unwrap().unwrap().id;
    let essay_id = f.tags.find_by_slug("essay").await.unwrap().unwrap().id;

    let dto = f
        .posts
        .create(
            &f.author,
            CreatePostCmd {
                tag_ids: vec![rust_id, essay_id],
                ..draft_cmd("with-tags")
            },
        )
        .await
        .unwrap();
    let mut got = dto.tag_ids.clone();
    got.sort();
    let mut want = vec![rust_id, essay_id];
    want.sort();
    assert_eq!(got, want, "返回全部初始标签");
}

#[tokio::test]
async fn editing_tags_only_still_bumps_version() {
    let f = tag_fixture().await;
    let created = f
        .posts
        .create(&f.author, draft_cmd("tags-only"))
        .await
        .unwrap();
    let rust_id = f.tags.find_by_slug("rust").await.unwrap().unwrap().id;
    let base_version = created.version;

    // 只改标签（正文不动）：posts.version 也必须 +1（同事务，docs §1）。
    let dto = f
        .posts
        .edit(
            &f.author,
            EditPostCmd {
                id: created.id,
                tag_ids: Some(vec![rust_id]),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(dto.tag_ids, vec![rust_id]);
    assert_eq!(dto.version, base_version + 1, "仅标签变化也递增版本");

    // 再次提交相同集合：幂等无操作，不递增版本。
    let again = f
        .posts
        .edit(
            &f.author,
            EditPostCmd {
                id: created.id,
                tag_ids: Some(vec![rust_id]),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(again.version, base_version + 1);

    // 清空标签同样是有效变化。
    let cleared = f
        .posts
        .edit(
            &f.author,
            EditPostCmd {
                id: created.id,
                tag_ids: Some(vec![]),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert!(cleared.tag_ids.is_empty());
    assert_eq!(cleared.version, base_version + 2);
}

#[tokio::test]
async fn duplicate_tag_ids_are_idempotent_not_errors() {
    let f = tag_fixture().await;
    let rust_id = f.tags.find_by_slug("rust").await.unwrap().unwrap().id;

    let dto = f
        .posts
        .create(
            &f.author,
            CreatePostCmd {
                tag_ids: vec![rust_id, rust_id, rust_id],
                ..draft_cmd("dup-tags")
            },
        )
        .await
        .unwrap();
    assert_eq!(dto.tag_ids, vec![rust_id], "重复 id 去重为一条关系");
}

#[tokio::test]
async fn unknown_tag_id_is_a_validation_error() {
    let f = tag_fixture().await;
    let ghost = Uuid::now_v7();
    let err = f
        .posts
        .create(
            &f.author,
            CreatePostCmd {
                tag_ids: vec![ghost],
                ..draft_cmd("ghost-tag")
            },
        )
        .await
        .unwrap_err();
    assert!(
        matches!(err, UseCaseError::Invalid(ref m) if m.contains("所选标签不存在")),
        "得到 {err:?}"
    );
}

#[tokio::test]
async fn tag_association_follows_post_authorization() {
    let f = tag_fixture().await;
    let created = f
        .posts
        .create(&f.author, draft_cmd("auth-tags"))
        .await
        .unwrap();
    let rust_id = f.tags.find_by_slug("rust").await.unwrap().unwrap().id;

    // other 无 post.update/update_any：连“只挂标签”也不行（标签挂在文章上）。
    let err = f
        .posts
        .edit(
            &f.other,
            EditPostCmd {
                id: created.id,
                tag_ids: Some(vec![rust_id]),
                ..Default::default()
            },
        )
        .await
        .unwrap_err();
    assert!(matches!(err, UseCaseError::Forbidden), "得到 {err:?}");

    // editor 持 update_any：可以给他人文章换标签。
    let dto = f
        .posts
        .edit(
            &f.editor,
            EditPostCmd {
                id: created.id,
                tag_ids: Some(vec![rust_id]),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(dto.tag_ids, vec![rust_id]);
}

#[tokio::test]
async fn tag_change_requires_fresh_version() {
    let f = tag_fixture().await;
    let created = f
        .posts
        .create(&f.author, draft_cmd("stale-tags"))
        .await
        .unwrap();
    let rust_id = f.tags.find_by_slug("rust").await.unwrap().unwrap().id;

    // 显式携带过期 expected_version：即使本次“只改标签”也必须报冲突，
    // 不能假装成功（幂等不等于忽略版本前提）。
    let err = f
        .posts
        .edit(
            &f.author,
            EditPostCmd {
                id: created.id,
                tag_ids: Some(vec![rust_id]),
                expected_version: Some(created.version - 1),
                ..Default::default()
            },
        )
        .await
        .unwrap_err();
    assert!(matches!(err, UseCaseError::VersionConflict), "得到 {err:?}");
}

#[tokio::test]
async fn trash_scope_versions_restore_and_purge_permissions() {
    let f = fixture().await;
    let created = f
        .posts
        .create(&f.author, draft_cmd("trash-cycle"))
        .await
        .unwrap();
    let published = f
        .posts
        .publish(&f.author, created.id, Some(created.version))
        .await
        .unwrap();
    assert!(matches!(
        f.posts
            .trash(&f.author2, created.id, Some(published.version))
            .await,
        Err(UseCaseError::Forbidden)
    ));
    let deleted = f
        .posts
        .trash(&f.author, created.id, Some(published.version))
        .await
        .unwrap();
    assert!(deleted.deleted);
    assert!(matches!(
        f.posts
            .restore(&f.author2, created.id, Some(deleted.version))
            .await,
        Err(UseCaseError::Forbidden)
    ));
    assert!(matches!(
        f.posts
            .restore(&f.author, created.id, Some(published.version))
            .await,
        Err(UseCaseError::VersionConflict)
    ));
    assert!(matches!(
        f.posts
            .purge(&f.author, created.id, Some(deleted.version))
            .await,
        Err(UseCaseError::Forbidden)
    ));
    let restored = f
        .posts
        .restore(&f.author, created.id, Some(deleted.version))
        .await
        .unwrap();
    assert_eq!(restored.status, "draft");
    assert!(!restored.deleted);
    assert!(restored.published_at.is_some());
    assert!(matches!(
        f.posts
            .purge(&f.author, created.id, Some(restored.version))
            .await,
        Err(UseCaseError::Forbidden)
    ));
}

// 新引用与历史引用：所有媒体链接公开，软删除仅限制新增附着。
#[tokio::test]
async fn avatar_rejects_invalid_and_stale_versions_without_changing_the_profile() {
    let f = fixture().await;
    let before = f.users.profile_of(&f.author).await.unwrap();
    assert!(matches!(
        f.users.set_own_avatar(&f.author, None, 0).await,
        Err(UseCaseError::Invalid(_))
    ));
    let updated = f
        .users
        .update_own_profile(&f.author, Some("New name".into()), None, before.version)
        .await
        .unwrap();
    assert!(matches!(
        f.users
            .set_own_avatar(&f.author, None, before.version)
            .await,
        Err(UseCaseError::VersionConflict)
    ));
    assert_eq!(
        f.users.profile_of(&f.author).await.unwrap().version,
        updated.version
    );
    let saved = f
        .users
        .set_own_avatar(&f.author, None, updated.version)
        .await
        .unwrap();
    assert_eq!(saved.version, updated.version + 1);
    assert_eq!(saved.display_name, updated.display_name);
}

#[tokio::test]
async fn avatar_accepts_shared_images_without_media_library_permission() {
    let f = fixture().await;
    let image = Uuid::now_v7();
    f.media_guard.allow(image);
    let profile = f
        .users
        .set_own_avatar(&f.other, Some(image), 1)
        .await
        .unwrap();
    assert_eq!(profile.avatar_media_id, Some(image));
}

#[tokio::test]
async fn avatar_preserves_a_trashed_current_image_but_rejects_new_unavailable_images() {
    let f = fixture().await;
    let image = Uuid::now_v7();
    f.media_guard.allow(image);
    f.users
        .set_own_avatar(&f.author, Some(image), 1)
        .await
        .unwrap();
    f.media_guard.trash(image);
    let profile = f
        .users
        .set_own_avatar(&f.author, Some(image), 2)
        .await
        .unwrap();
    assert_eq!(profile.avatar_media_id, Some(image));
    assert!(matches!(
        f.users.set_own_avatar(&f.other, Some(image), 1).await,
        Err(UseCaseError::Invalid(_))
    ));
    assert!(matches!(
        f.users
            .set_own_avatar(&f.author, Some(Uuid::now_v7()), 3)
            .await,
        Err(UseCaseError::Invalid(_))
    ));
}

#[tokio::test]
async fn post_cover_accepts_shared_images_and_preserves_trashed_existing_reference() {
    let f = fixture().await;
    let writer = Actor::new(
        f.other.user_id,
        application::identity::ActorChannel::Session,
        PermissionSet::from_keys(["post.create", "post.update"]),
    );
    let image = Uuid::now_v7();
    f.media_guard.allow(image);
    let mut cmd = draft_cmd("cover-shared");
    cmd.cover_media_id = Some(image);
    let created = f.posts.create(&writer, cmd).await.unwrap();
    f.media_guard.trash(image);
    let edited = f
        .posts
        .edit(
            &writer,
            EditPostCmd {
                id: created.id,
                expected_version: Some(created.version),
                cover_media_id: Some(Some(image)),
                content: Some("# 改动正文".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(edited.cover_media_id, Some(image));
    let mut unavailable = draft_cmd("cover-trashed");
    unavailable.cover_media_id = Some(image);
    assert!(matches!(
        f.posts.create(&writer, unavailable).await,
        Err(UseCaseError::Invalid(_))
    ));
}

#[async_trait::async_trait]
impl application::ports::TagLookup for FakeTagRepo {
    async fn existing_ids(&self, ids: &[Uuid]) -> Result<Vec<Uuid>, UseCaseError> {
        let tags = self.tags.lock().unwrap();
        let mut found: Vec<Uuid> = tags
            .iter()
            .map(|t| t.id)
            .filter(|id| ids.contains(id))
            .collect();
        found.sort();
        Ok(found)
    }
}

#[async_trait::async_trait]
impl application::ports::CategoryLookup for FakeCategoryRepo {
    async fn existing_id(&self, _id: uuid::Uuid) -> Result<bool, UseCaseError> {
        Ok(true)
    }
}

#[async_trait::async_trait]
impl application::ports::SeriesLookup for FakeSeriesRepo {
    async fn existing_ids(&self, ids: &[Uuid]) -> Result<Vec<Uuid>, UseCaseError> {
        self.lookups.lock().unwrap().push(ids.to_vec());
        let mut existing = ids.to_vec();
        existing.sort();
        existing.dedup();
        Ok(existing)
    }
}

// This fake intentionally has no catalog write/reorder capabilities.
struct ReadOnlyTaxonomy {
    tag: Uuid,
    category: Uuid,
    series: Uuid,
}

#[async_trait::async_trait]
impl application::ports::TagLookup for ReadOnlyTaxonomy {
    async fn existing_ids(&self, ids: &[Uuid]) -> Result<Vec<Uuid>, UseCaseError> {
        Ok(ids
            .contains(&self.tag)
            .then_some(self.tag)
            .into_iter()
            .collect())
    }
}

#[async_trait::async_trait]
impl application::ports::CategoryLookup for ReadOnlyTaxonomy {
    async fn existing_id(&self, id: Uuid) -> Result<bool, UseCaseError> {
        Ok(id == self.category)
    }
}

#[async_trait::async_trait]
impl application::ports::SeriesLookup for ReadOnlyTaxonomy {
    async fn existing_ids(&self, ids: &[Uuid]) -> Result<Vec<Uuid>, UseCaseError> {
        Ok(ids
            .contains(&self.series)
            .then_some(self.series)
            .into_iter()
            .collect())
    }
}

#[tokio::test]
async fn post_creation_accepts_taxonomy_lookup_only_dependencies() {
    let f = fixture().await;
    let taxonomy = Arc::new(ReadOnlyTaxonomy {
        tag: Uuid::now_v7(),
        category: Uuid::now_v7(),
        series: Uuid::now_v7(),
    });
    let posts = PostInteractor::new(
        f.post_repo,
        taxonomy.clone(),
        taxonomy.clone(),
        taxonomy.clone(),
        Arc::new(FixedClock),
        f.media_guard,
    );
    let mut cmd = draft_cmd("query-only-taxonomy");
    cmd.tag_ids = vec![taxonomy.tag, taxonomy.tag];
    cmd.category_id = Some(taxonomy.category);
    cmd.series = vec![application::content::SeriesPlacement {
        series_id: taxonomy.series,
        position: 3,
    }];
    let created = posts.create(&f.author, cmd).await.unwrap();
    assert_eq!(created.tag_ids, vec![taxonomy.tag]);
    assert_eq!(created.category_id, Some(taxonomy.category));
    assert_eq!(created.series[0].series_id, taxonomy.series);
    assert_eq!(created.series[0].position, 3);
}
