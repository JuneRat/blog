//! 标签管理用例测试：权限边界、slug 冲突、并发版本与引用保护。
//! 复用 post_usecase 的 fake 模式；不依赖生产 infrastructure。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use application::error::UseCaseError;
use application::identity::{Actor, CreateUserCmd, RoleInteractor, UserInteractor};
use application::ports::{
    Clock, RbacStore, RoleDto, TagDeleteOutcome, TagRepository, TagWithUsage, UserRepository,
};
use application::tag::{CreateTagCmd, TagInteractor};
use domain::content::TagSnapshot;
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
        time::macros::datetime!(2026-09-22 12:00:00 UTC)
    }
}

/// 标签目录 fake：tags 按 slug 索引；references 模拟 post_tags 占用
/// （含草稿/私密/回收站——引用保护不过滤可见性）。
struct FakeTagRepo {
    tags: Mutex<HashMap<String, TagSnapshot>>,
    references: Mutex<HashMap<Uuid, i64>>,
}

impl FakeTagRepo {
    fn new() -> Self {
        Self {
            tags: Mutex::new(HashMap::new()),
            references: Mutex::new(HashMap::new()),
        }
    }
}

#[async_trait::async_trait]
impl TagRepository for FakeTagRepo {
    async fn insert(&self, aggregate: &domain::content::Tag) -> Result<(), UseCaseError> {
        let snapshot = aggregate.snapshot();
        let mut tags = self.tags.lock().unwrap();
        if tags.contains_key(&snapshot.slug) {
            return Err(UseCaseError::Conflict(
                application::error::ConflictKind::Slug,
            ));
        }
        tags.insert(snapshot.slug.clone(), snapshot.clone());
        Ok(())
    }

    async fn find_by_slug(&self, slug: &str) -> Result<Option<TagSnapshot>, UseCaseError> {
        Ok(self.tags.lock().unwrap().get(slug).cloned())
    }

    async fn list(&self) -> Result<Vec<TagWithUsage>, UseCaseError> {
        let mut rows: Vec<TagWithUsage> = self
            .tags
            .lock()
            .unwrap()
            .values()
            .map(|snapshot| TagWithUsage {
                snapshot: snapshot.clone(),
                // 简化：fake 的公开计数等于引用数（可见性过滤由真实适配器测试覆盖）。
                public_post_count: *self
                    .references
                    .lock()
                    .unwrap()
                    .get(&snapshot.id)
                    .unwrap_or(&0),
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
    ) -> Result<Option<TagSnapshot>, UseCaseError> {
        let mut tags = self.tags.lock().unwrap();
        let Some(tag) = tags.values_mut().find(|t| t.id == id) else {
            return Ok(None);
        };
        if tag.version != expected_version {
            return Ok(None);
        }
        tag.name = new_name.into();
        tag.version += 1;
        Ok(Some(tag.clone()))
    }

    async fn delete(
        &self,
        id: Uuid,
        expected_version: i64,
    ) -> Result<TagDeleteOutcome, UseCaseError> {
        let mut tags = self.tags.lock().unwrap();
        let Some(tag) = tags.values().find(|t| t.id == id).cloned() else {
            return Ok(TagDeleteOutcome::Gone);
        };
        if tag.version != expected_version {
            return Ok(TagDeleteOutcome::StaleVersion);
        }
        let count = *self.references.lock().unwrap().get(&id).unwrap_or(&0);
        if count > 0 {
            return Ok(TagDeleteOutcome::Referenced { count });
        }
        tags.remove(&tag.slug);
        Ok(TagDeleteOutcome::Deleted)
    }

    async fn existing_ids(&self, ids: &[Uuid]) -> Result<Vec<Uuid>, UseCaseError> {
        let tags = self.tags.lock().unwrap();
        let mut found: Vec<Uuid> = tags
            .values()
            .map(|t| t.id)
            .filter(|id| ids.contains(id))
            .collect();
        found.sort();
        Ok(found)
    }

    async fn public_count(&self, id: Uuid) -> Result<i64, UseCaseError> {
        Ok(*self.references.lock().unwrap().get(&id).unwrap_or(&0))
    }
}

/// 最小 RBAC fake：只按内置角色给权限并集（来自注册表）。
struct FakeRbacStore {
    assignments: Mutex<HashMap<Uuid, Vec<&'static str>>>,
}

impl FakeRbacStore {
    fn new() -> Self {
        Self {
            assignments: Mutex::new(HashMap::new()),
        }
    }
}

const ROLE_KEYS: &[(&str, &[&str])] = &[
    (
        "owner",
        &[
            "tag.manage",
            "user.manage",
            "role.manage",
            "ownership.manage",
            "post.create",
            "post.read",
            "post.read_any",
            "post.update",
            "post.update_any",
        ],
    ),
    (
        "editor",
        &["tag.manage", "post.read_any", "post.update_any"],
    ),
    ("author", &["post.create", "post.read", "post.update"]),
];

#[async_trait::async_trait]
impl RbacStore for FakeRbacStore {
    async fn sync_permission_registry(
        &self,
        _entries: &[application::identity::PermissionDescriptor],
    ) -> Result<(), UseCaseError> {
        Ok(())
    }

    async fn sync_builtin_roles(
        &self,
        _defs: &[application::identity::BuiltinRoleDef],
    ) -> Result<(), UseCaseError> {
        Ok(())
    }

    async fn permissions_of_user(&self, user_id: Uuid) -> Result<PermissionSet, UseCaseError> {
        let assignments = self.assignments.lock().unwrap();
        let keys = assignments
            .get(&user_id)
            .into_iter()
            .flatten()
            .flat_map(|role| {
                ROLE_KEYS
                    .iter()
                    .find(|(slug, _)| slug == role)
                    .map(|(_, keys)| keys.iter().copied())
                    .unwrap_or_default()
            })
            .collect::<Vec<_>>();
        Ok(PermissionSet::from_keys(keys))
    }

    async fn permissions_of_role(&self, _role_slug: &str) -> Result<PermissionSet, UseCaseError> {
        Ok(PermissionSet::from_keys(Vec::<String>::new()))
    }

    async fn assign_role(&self, user_id: Uuid, role_slug: &str) -> Result<(), UseCaseError> {
        self.assignments
            .lock()
            .unwrap()
            .entry(user_id)
            .or_default()
            .push(match role_slug {
                "owner" => "owner",
                "editor" => "editor",
                _ => "author",
            });
        Ok(())
    }

    async fn remove_role(&self, _user_id: Uuid, _role_slug: &str) -> Result<(), UseCaseError> {
        Ok(())
    }

    async fn list_roles(&self) -> Result<Vec<RoleDto>, UseCaseError> {
        Ok(Vec::new())
    }

    async fn roles_of_user(&self, _user_id: Uuid) -> Result<Vec<String>, UseCaseError> {
        Ok(Vec::new())
    }

    async fn roles_of_users(
        &self,
        _user_ids: &[Uuid],
    ) -> Result<Vec<(Uuid, String)>, UseCaseError> {
        Ok(Vec::new())
    }

    async fn loginable_owner_count(&self) -> Result<i64, UseCaseError> {
        Ok(1)
    }
}

struct FakeUserRepo {
    users: Mutex<HashMap<String, UserSnapshot>>,
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
    async fn save_profile(
        &self,
        user: &domain::identity::User,
        expected_version: i64,
        now: time::OffsetDateTime,
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

    async fn revoke_authentication(&self, user_id: Uuid) -> Result<(), UseCaseError> {
        let mut users = self.users.lock().unwrap();
        let user = users
            .values_mut()
            .find(|u| u.id == user_id)
            .ok_or_else(|| UseCaseError::NotFound("用户".into()))?;
        user.auth_version += 1;
        Ok(())
    }

    /// 头像只走真实认证 HTTP 用例（server/tests）；本 fake 不实现，误用即失败。
    async fn set_avatar(
        &self,
        _user_id: uuid::Uuid,
        _avatar_media_id: Option<uuid::Uuid>,
        _now: time::OffsetDateTime,
    ) -> Result<(), UseCaseError> {
        unimplemented!("该用例不使用头像")
    }

    async fn insert(&self, aggregate: &domain::identity::User) -> Result<(), UseCaseError> {
        let snapshot = aggregate.snapshot();
        self.users
            .lock()
            .unwrap()
            .insert(snapshot.username.clone(), snapshot.clone());
        Ok(())
    }
    async fn find_by_id(&self, _id: Uuid) -> Result<Option<UserSnapshot>, UseCaseError> {
        Ok(None)
    }
    async fn find_by_username(&self, username: &str) -> Result<Option<UserSnapshot>, UseCaseError> {
        Ok(self.users.lock().unwrap().get(username).cloned())
    }
    async fn list_admin(
        &self,
        _limit: i64,
        _offset: i64,
    ) -> Result<Vec<application::ports::AdminUserRow>, UseCaseError> {
        Ok(Vec::new())
    }
    async fn set_password_hash(&self, _user_id: Uuid, _phc_hash: &str) -> Result<(), UseCaseError> {
        Ok(())
    }
    async fn compare_and_set_password_hash(
        &self,
        _user_id: Uuid,
        _expected: Option<&str>,
        _new_hash: &str,
    ) -> Result<Option<i64>, UseCaseError> {
        Ok(None)
    }
    async fn clear_password_hash(&self, _user_id: Uuid) -> Result<(), UseCaseError> {
        Ok(())
    }
    async fn clear_password_hash_guarded(
        &self,
        _user_id: Uuid,
    ) -> Result<application::ports::ClearPasswordOutcome, UseCaseError> {
        Ok(application::ports::ClearPasswordOutcome::NoPassword)
    }
    async fn find_password_credential(
        &self,
        _username: &str,
    ) -> Result<Option<application::ports::PasswordCredential>, UseCaseError> {
        Ok(None)
    }
    async fn password_hash_of(&self, _user_id: Uuid) -> Result<Option<String>, UseCaseError> {
        Ok(None)
    }
}

// ---------------------------------------------------------------------------
// Fixture
// ---------------------------------------------------------------------------

struct Fixture {
    tags: Arc<TagInteractor>,
    repo: Arc<FakeTagRepo>,
    editor: Actor,
    author: Actor,
}

async fn fixture() -> Fixture {
    let repo = Arc::new(FakeTagRepo::new());
    let user_repo = Arc::new(FakeUserRepo::new());
    let rbac = Arc::new(FakeRbacStore::new());
    let clock = Arc::new(FixedClock);
    let users = Arc::new(UserInteractor::new(
        user_repo.clone(),
        rbac.clone(),
        clock.clone(),
        Arc::new(common::FakeMediaGuard::new()),
    ));
    let roles = Arc::new(RoleInteractor::new(rbac.clone(), user_repo));

    for username in ["editor", "author"] {
        users
            .create_user(
                &Actor::bootstrap_cli(),
                CreateUserCmd {
                    username: username.into(),
                    email: None,
                    display_name: None,
                },
            )
            .await
            .unwrap();
        roles
            .assign_to_username(&Actor::bootstrap_cli(), username, username)
            .await
            .unwrap();
    }

    Fixture {
        tags: Arc::new(TagInteractor::new(repo.clone(), clock)),
        repo,
        editor: users.actor_for_username("editor").await.unwrap(),
        author: users.actor_for_username("author").await.unwrap(),
    }
}

fn cmd(name: &str, slug: &str) -> CreateTagCmd {
    CreateTagCmd {
        name: name.into(),
        slug: slug.into(),
    }
}

// ---------------------------------------------------------------------------
// 用例
// ---------------------------------------------------------------------------

#[tokio::test]
async fn editor_can_create_list_rename_and_delete_tags() {
    let f = fixture().await;
    let created = f.tags.create(&f.editor, cmd("Rust", "rust")).await.unwrap();
    assert_eq!(created.name, "Rust");
    assert_eq!(created.version, 1);

    f.tags
        .create(&f.editor, cmd("随笔", "essay"))
        .await
        .unwrap();
    let list = f.tags.list(&f.editor).await.unwrap();
    assert_eq!(list.len(), 2);
    assert_eq!(list[0].slug, "essay", "目录按 slug 排序");

    let renamed = f
        .tags
        .rename(&f.editor, "rust", "Rust 语言".into(), Some(1))
        .await
        .unwrap();
    assert_eq!(renamed.name, "Rust 语言");
    assert_eq!(renamed.version, 2, "改名递增版本");

    f.tags.delete(&f.editor, "essay", Some(1)).await.unwrap();
    assert_eq!(f.tags.list(&f.editor).await.unwrap().len(), 1);
}

#[tokio::test]
async fn author_cannot_manage_tag_catalog_but_can_read_it() {
    let f = fixture().await;
    f.tags.create(&f.editor, cmd("Rust", "rust")).await.unwrap();

    // 无 tag.manage：创建/改名/删除全部拒绝。
    assert!(matches!(
        f.tags
            .create(&f.author, cmd("别的", "other"))
            .await
            .unwrap_err(),
        UseCaseError::Forbidden
    ));
    assert!(matches!(
        f.tags
            .rename(&f.author, "rust", "改名".into(), None)
            .await
            .unwrap_err(),
        UseCaseError::Forbidden
    ));
    assert!(matches!(
        f.tags.delete(&f.author, "rust", None).await.unwrap_err(),
        UseCaseError::Forbidden
    ));

    // 目录读取不设权限：Author 编辑文章需要选择标签。
    let list = f.tags.list(&f.author).await.unwrap();
    assert_eq!(list.len(), 1);
}

#[tokio::test]
async fn duplicate_slug_is_a_conflict() {
    let f = fixture().await;
    f.tags.create(&f.editor, cmd("Rust", "rust")).await.unwrap();
    let err = f
        .tags
        .create(&f.editor, cmd("另一个 Rust", "rust"))
        .await
        .unwrap_err();
    assert!(matches!(
        err,
        UseCaseError::Conflict(application::error::ConflictKind::Slug)
    ));
}

#[tokio::test]
async fn invalid_name_and_slug_are_rejected() {
    let f = fixture().await;
    assert!(matches!(
        f.tags
            .create(&f.editor, cmd("  ", "blank"))
            .await
            .unwrap_err(),
        UseCaseError::Invalid(_)
    ));
    assert!(matches!(
        f.tags
            .create(&f.editor, cmd("斜杠", "a/b"))
            .await
            .unwrap_err(),
        UseCaseError::Invalid(_)
    ));
    let long = "长".repeat(101);
    assert!(matches!(
        f.tags
            .create(&f.editor, cmd(&long, "long"))
            .await
            .unwrap_err(),
        UseCaseError::Invalid(_)
    ));
}

#[tokio::test]
async fn rename_and_delete_check_expected_version() {
    let f = fixture().await;
    let created = f.tags.create(&f.editor, cmd("Rust", "rust")).await.unwrap();

    let err = f
        .tags
        .rename(&f.editor, "rust", "新名".into(), Some(created.version + 5))
        .await
        .unwrap_err();
    assert!(matches!(err, UseCaseError::VersionConflict));

    let err = f
        .tags
        .delete(&f.editor, "rust", Some(created.version + 5))
        .await
        .unwrap_err();
    assert!(matches!(err, UseCaseError::VersionConflict));

    // 版本正确时删除成功。
    f.tags
        .delete(&f.editor, "rust", Some(created.version))
        .await
        .unwrap();
}

#[tokio::test]
async fn referenced_tag_refuses_delete() {
    let f = fixture().await;
    let created = f.tags.create(&f.editor, cmd("Rust", "rust")).await.unwrap();
    // 模拟被两篇文章引用（可见性无关：草稿/私密同样占用）。
    f.repo.references.lock().unwrap().insert(created.id, 2);

    let err = f
        .tags
        .delete(&f.editor, "rust", Some(created.version))
        .await
        .unwrap_err();
    match err {
        UseCaseError::TagInUse(count) => assert_eq!(count, 2),
        other => panic!("期望 TagInUse，得到 {other:?}"),
    }

    // 解除引用后同一版本可删。
    f.repo.references.lock().unwrap().remove(&created.id);
    f.tags
        .delete(&f.editor, "rust", Some(created.version))
        .await
        .unwrap();
}

#[tokio::test]
async fn unknown_tag_is_not_found() {
    let f = fixture().await;
    assert!(matches!(
        f.tags
            .rename(&f.editor, "ghost", "新名".into(), None)
            .await
            .unwrap_err(),
        UseCaseError::NotFound(_)
    ));
    assert!(matches!(
        f.tags.delete(&f.editor, "ghost", None).await.unwrap_err(),
        UseCaseError::NotFound(_)
    ));
}

#[tokio::test]
async fn idempotent_rename_keeps_version() {
    let f = fixture().await;
    let created = f.tags.create(&f.editor, cmd("Rust", "rust")).await.unwrap();
    // 同名改名：无变化，不递增版本。
    let dto = f
        .tags
        .rename(&f.editor, "rust", "Rust".into(), Some(created.version))
        .await
        .unwrap();
    assert_eq!(dto.version, created.version);
}
