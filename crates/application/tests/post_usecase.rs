//! 文章用例测试：内存 fake 仓储验证用例、归属与失败路径，不依赖生产 infrastructure。

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use application::content::{CreatePostCmd, EditPostCmd, PostInteractor};
use application::error::{ConflictKind, UseCaseError};
use application::identity::{
    Actor, ActorChannel, CreateUserCmd, PermissionDescriptor, RoleInteractor, UserInteractor,
};
use application::ports::{Clock, PostRepository, RbacStore, RoleDto, SaveOutcome, UserRepository};
use domain::content::post::{PostSnapshot, Visibility};
use domain::identity::{PermissionSet, UserSnapshot};
use time::OffsetDateTime;
use uuid::Uuid;

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
}

impl FakePostRepo {
    fn new() -> Self {
        Self {
            posts: Mutex::new(HashMap::new()),
        }
    }
}

#[async_trait::async_trait]
impl PostRepository for FakePostRepo {
    async fn find_by_slug(&self, slug: &str) -> Result<Option<PostSnapshot>, UseCaseError> {
        Ok(self.posts.lock().unwrap().get(slug).cloned())
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

    async fn list_by_author(
        &self,
        author_id: uuid::Uuid,
    ) -> Result<Vec<PostSnapshot>, UseCaseError> {
        Ok(self
            .posts
            .lock()
            .unwrap()
            .values()
            .filter(|p| p.author_id == author_id)
            .cloned()
            .collect())
    }

    async fn insert(&self, snapshot: &PostSnapshot) -> Result<(), UseCaseError> {
        let mut posts = self.posts.lock().unwrap();
        if posts.contains_key(&snapshot.slug) {
            return Err(UseCaseError::Conflict(ConflictKind::Slug));
        }
        posts.insert(snapshot.slug.clone(), snapshot.clone());
        Ok(())
    }

    async fn save(
        &self,
        snapshot: &PostSnapshot,
        expected_version: i64,
        now: OffsetDateTime,
    ) -> Result<SaveOutcome, UseCaseError> {
        let mut posts = self.posts.lock().unwrap();
        let current = match posts.get_mut(&snapshot.slug) {
            Some(p) if p.id == snapshot.id => p,
            _ => return Ok(SaveOutcome::Gone),
        };
        if current.deleted_at.is_some() {
            return Ok(SaveOutcome::Gone);
        }
        if current.version != expected_version {
            return Ok(SaveOutcome::StaleConflict);
        }
        current.title = snapshot.title.clone();
        current.excerpt = snapshot.excerpt.clone();
        current.content = snapshot.content.clone();
        current.status = snapshot.status;
        current.visibility = snapshot.visibility;
        current.published_at = snapshot.published_at;
        current.updated_at = now;
        current.version += 1;
        Ok(SaveOutcome::Saved {
            new_version: current.version,
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
impl UserRepository for FakeUserRepo {
    async fn insert(&self, snapshot: &UserSnapshot) -> Result<(), UseCaseError> {
        let mut users = self.users.lock().unwrap();
        if users.contains_key(&snapshot.username) {
            return Err(UseCaseError::Conflict(ConflictKind::Username));
        }
        users.insert(snapshot.username.clone(), snapshot.clone());
        Ok(())
    }

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
                deleted: u.deleted_at.is_some(),
                password_enabled: false,
                external_identities: 0,
            })
            .collect())
    }

    // 文章用例不涉及本地密码；保持显式失败以便误用时立刻暴露。
    async fn set_password_hash(&self, _user_id: Uuid, _phc_hash: &str) -> Result<(), UseCaseError> {
        unimplemented!("post 用例不使用密码凭据")
    }

    async fn compare_and_set_password_hash(
        &self,
        _user_id: Uuid,
        _expected: Option<&str>,
        _new_hash: &str,
    ) -> Result<Option<i64>, UseCaseError> {
        unimplemented!("post 用例不使用密码凭据")
    }

    async fn clear_password_hash_guarded(
        &self,
        _user_id: Uuid,
    ) -> Result<application::ports::ClearPasswordOutcome, UseCaseError> {
        unimplemented!("post 用例不使用密码凭据")
    }

    async fn clear_password_hash(&self, _user_id: Uuid) -> Result<(), UseCaseError> {
        unimplemented!("post 用例不使用密码凭据")
    }

    async fn find_password_credential(
        &self,
        _username: &str,
    ) -> Result<Option<application::ports::PasswordCredential>, UseCaseError> {
        unimplemented!("post 用例不使用密码凭据")
    }

    async fn password_hash_of(&self, _user_id: Uuid) -> Result<Option<String>, UseCaseError> {
        unimplemented!("post 用例不使用密码凭据")
    }
}

// ---------------------------------------------------------------------------
// 测试装配
// ---------------------------------------------------------------------------

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
            .filter(|(_, slug)| slug == "owner")
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

    async fn assign_role(&self, user_id: Uuid, role_slug: &str) -> Result<(), UseCaseError> {
        if !self.roles.lock().unwrap().contains_key(role_slug) {
            return Err(UseCaseError::NotFound(format!("角色 {role_slug}")));
        }
        self.assignments
            .lock()
            .unwrap()
            .insert((user_id, role_slug.to_string()));
        Ok(())
    }

    async fn remove_role(&self, user_id: Uuid, role_slug: &str) -> Result<(), UseCaseError> {
        if role_slug == "owner" && self.owners() <= 1 {
            return Err(UseCaseError::LastOwnerProtected);
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

    async fn loginable_owner_count(&self) -> Result<i64, UseCaseError> {
        // Fake 身份没有登录方式概念：owner 分配数即「可登录 Owner」数。
        Ok(self.owners())
    }
}

struct Fixture {
    posts: Arc<PostInteractor>,
    users: Arc<UserInteractor>,
    roles: Arc<RoleInteractor>,
    author: Actor,
    author2: Actor,
    other: Actor,
    editor: Actor,
}

async fn fixture() -> Fixture {
    let post_repo = Arc::new(FakePostRepo::new());
    let user_repo = Arc::new(FakeUserRepo::new());
    let rbac = Arc::new(FakeRbacStore::new());
    let clock = Arc::new(FixedClock);

    let users = Arc::new(UserInteractor::new(
        user_repo.clone(),
        rbac.clone(),
        clock.clone(),
    ));
    let roles = Arc::new(RoleInteractor::new(rbac.clone(), user_repo));
    let posts = Arc::new(PostInteractor::new(post_repo, clock));

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
        users,
        roles,
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
    }
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
                target_slug: "first-post".into(),
                title: Some("第一篇（改）".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(edited.version, 2, "有实际变化才递增 version");

    let published = f
        .posts
        .publish(&f.author, "first-post", None)
        .await
        .unwrap();
    assert_eq!(published.status, "published");
    assert!(published.published_at.is_some());
    assert_eq!(published.version, 3);

    // 重复发布幂等：无变化不递增版本。
    let again = f
        .posts
        .publish(&f.author, "first-post", None)
        .await
        .unwrap();
    assert_eq!(again.version, 3);

    let withdrawn = f
        .posts
        .withdraw(&f.author, "first-post", None)
        .await
        .unwrap();
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
                target_slug: slug.into(),
                content: Some("另一位编辑的新正文".into()),
                expected_version: Some(original.version),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let published = f
        .posts
        .publish(&f.author, slug, Some(edited.version))
        .await
        .unwrap();

    // 另一位编辑已发布：旧页面再点发布必须冲突，不能取得新版本后覆盖旧正文。
    assert!(matches!(
        f.posts
            .publish(&f.author, slug, Some(original.version))
            .await,
        Err(UseCaseError::VersionConflict)
    ));
    let again = f
        .posts
        .publish(&f.author, slug, Some(published.version))
        .await
        .unwrap();
    assert_eq!(again.version, published.version);
    assert_eq!(again.content, edited.content);

    let withdrawn = f
        .posts
        .withdraw(&f.author, slug, Some(published.version))
        .await
        .unwrap();
    assert!(matches!(
        f.posts
            .withdraw(&f.author, slug, Some(published.version))
            .await,
        Err(UseCaseError::VersionConflict)
    ));
    assert_eq!(
        f.posts
            .withdraw(&f.author, slug, Some(withdrawn.version))
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
                    target_slug: slug.into(),
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
                target_slug: slug.into(),
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
    f.posts.create(&f.author, draft_cmd("race")).await.unwrap();

    // 两个调用方都基于 version=1 提交编辑。
    let a = f
        .posts
        .edit(
            &f.author,
            EditPostCmd {
                target_slug: "race".into(),
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
                target_slug: "race".into(),
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
    f.posts
        .create(&f.author, draft_cmd("parallel"))
        .await
        .unwrap();

    // 两个调用真正同时进行（join!），都基于 version=1。
    let edit = |content: &'static str| {
        f.posts.edit(
            &f.author,
            EditPostCmd {
                target_slug: "parallel".into(),
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

    let shown = f.posts.find(&f.author, "parallel").await.unwrap();
    assert_eq!(shown.version, 2, "恰好一次版本递增");
    // 胜者只改了 content，标题/摘要保持原样。
    assert_eq!(shown.title, "第一篇");
}

#[tokio::test]
async fn ownership_check_rejects_non_author() {
    let f = fixture().await;
    f.posts
        .create(&f.author, draft_cmd("own-post"))
        .await
        .unwrap();

    let err = f
        .posts
        .publish(&f.other, "own-post", None)
        .await
        .unwrap_err();
    assert!(matches!(err, UseCaseError::Forbidden));
}

#[tokio::test]
async fn publish_requires_content() {
    let f = fixture().await;
    f.posts
        .create(
            &f.author,
            CreatePostCmd {
                slug: Some("empty".into()),
                title: String::new(),
                excerpt: None,
                content: String::new(),
                visibility: Visibility::Public,
            },
        )
        .await
        .unwrap();

    let err = f.posts.publish(&f.author, "empty", None).await.unwrap_err();
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
    let shown = f.posts.find(&f.author, "shown").await.unwrap();
    assert_eq!(shown.id, created.id);
    // deleted_at 过滤行为由 infrastructure 集成测试覆盖（M1 未开放删除用例）。
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
    f.posts
        .create(&f.author, draft_cmd("editors-view"))
        .await
        .unwrap();

    // editor 无 own 授权，但 update_any 覆盖 own。
    let edited = f
        .posts
        .edit(
            &f.editor,
            EditPostCmd {
                target_slug: "editors-view".into(),
                title: Some("编辑改写".into()),
                ..Default::default()
            },
        )
        .await;
    assert!(edited.is_ok(), "update_any 应允许编辑他人文章：{edited:?}");

    let shown = f.posts.find(&f.editor, "editors-view").await.unwrap();
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
    f.posts
        .create(&f.author, draft_cmd("secret-draft"))
        .await
        .unwrap();

    // other 无 read/read_any：既不能读也不能改。
    let err = f.posts.find(&f.other, "secret-draft").await.unwrap_err();
    assert!(matches!(err, UseCaseError::Forbidden));
    let err = f
        .posts
        .edit(
            &f.other,
            EditPostCmd {
                target_slug: "secret-draft".into(),
                title: Some("越权修改".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap_err();
    assert!(matches!(err, UseCaseError::Forbidden));

    // author2 有 own 权限（post.read/post.update）但不是作者：
    // 这里测的是「不是本人」，而不是「没有权限」。
    let err = f.posts.find(&f.author2, "secret-draft").await.unwrap_err();
    assert!(
        matches!(err, UseCaseError::Forbidden),
        "own 权限不得跨作者读取：{err:?}"
    );
    let err = f
        .posts
        .edit(
            &f.author2,
            EditPostCmd {
                target_slug: "secret-draft".into(),
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
    assert!(f.posts.find(&f.author, "secret-draft").await.is_ok());
    // editor 有 read_any，可读他人草稿。
    assert!(f.posts.find(&f.editor, "secret-draft").await.is_ok());
}

#[tokio::test]
async fn list_by_author_requires_read_permission_for_own_posts_too() {
    let f = fixture().await;
    f.posts
        .create(&f.author, draft_cmd("listed-draft"))
        .await
        .unwrap();

    // other 无任何角色：列表与单篇必须一致地拒绝，不能「单篇 403、列表 200」。
    let err = f
        .posts
        .list_by_author(&f.other, f.other.user_id)
        .await
        .unwrap_err();
    assert!(
        matches!(err, UseCaseError::Forbidden),
        "无 post.read 不得列出本人文章：{err:?}"
    );
    assert!(matches!(
        f.posts.find(&f.other, "listed-draft").await.unwrap_err(),
        UseCaseError::Forbidden
    ));

    // author 有 post.read(own)：列自己的文章正常。
    let own = f
        .posts
        .list_by_author(&f.author, f.author.user_id)
        .await
        .unwrap();
    assert_eq!(own.len(), 1);
    assert_eq!(own[0].slug, "listed-draft");

    // author2 有 post.read(own) 但不是作者，也无 read_any → 拒绝跨作者列表。
    let err = f
        .posts
        .list_by_author(&f.author2, f.author.user_id)
        .await
        .unwrap_err();
    assert!(matches!(err, UseCaseError::Forbidden));

    // editor 有 read_any，可列他人文章。
    let others = f
        .posts
        .list_by_author(&f.editor, f.author.user_id)
        .await
        .unwrap();
    assert_eq!(others.len(), 1);
}

#[tokio::test]
async fn author_cannot_publish_others_posts_without_any() {
    let f = fixture().await;
    let author2 = f.users.actor_for_username("author2").await.unwrap();
    f.posts
        .create(&author2, draft_cmd("author2-owns"))
        .await
        .unwrap();

    // author 有 post.publish(own)，但文章属于 author2 → Forbidden。
    let err = f
        .posts
        .publish(&f.author, "author2-owns", None)
        .await
        .unwrap_err();
    assert!(matches!(err, UseCaseError::Forbidden));

    // editor 有 publish_any，可发布同一篇。
    assert!(
        f.posts
            .publish(&f.editor, "author2-owns", None)
            .await
            .is_ok()
    );
}

#[tokio::test]
async fn last_owner_cannot_be_removed() {
    let f = fixture().await;
    let bootstrap = Actor::bootstrap_cli();
    f.roles
        .assign_to_username(&bootstrap, "author", "owner")
        .await
        .unwrap();

    // 唯一 Owner：移除被拒（存储侧最后 Owner 保护）。
    let err = f
        .roles
        .remove_from_username(&bootstrap, "author", "owner")
        .await
        .unwrap_err();
    assert!(
        matches!(err, UseCaseError::LastOwnerProtected),
        "最后 Owner 保护：{err:?}"
    );

    // 出现第二个 Owner 后，移除其中一个允许。
    f.roles
        .assign_to_username(&bootstrap, "editor", "owner")
        .await
        .unwrap();
    assert!(
        f.roles
            .remove_from_username(&bootstrap, "author", "owner")
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
    // 除 ownership.manage 外持有全部已注册权限的会话：仍不能授予 Owner。
    let without_ownership: Vec<&'static str> = all_registered_permissions()
        .into_iter()
        .filter(|key| *key != "ownership.manage")
        .collect();
    let err = f
        .roles
        .assign_to_username(&session_actor_with(without_ownership), "author", "owner")
        .await
        .unwrap_err();
    assert!(
        matches!(err, UseCaseError::Forbidden),
        "普通角色分配不能授予 Owner：{err:?}"
    );

    // 持有 ownership.manage 后允许。
    assert!(
        f.roles
            .assign_to_username(
                &session_actor_with(all_registered_permissions()),
                "author",
                "owner"
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
