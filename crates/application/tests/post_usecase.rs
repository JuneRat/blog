//! 文章用例测试：内存 fake 仓储验证用例、归属与失败路径，不依赖生产 infrastructure。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use application::content::{CreatePostCmd, EditPostCmd, PostInteractor};
use application::error::UseCaseError;
use application::identity::{Actor, CreateUserCmd, UserInteractor};
use application::ports::{Clock, PostRepository, SaveOutcome, UserRepository};
use domain::content::post::{PostSnapshot, Visibility};
use domain::identity::UserSnapshot;
use time::OffsetDateTime;

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
            return Err(UseCaseError::Conflict("slug".into()));
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
            return Err(UseCaseError::Conflict("username".into()));
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
}

// ---------------------------------------------------------------------------
// 测试装配
// ---------------------------------------------------------------------------

struct Fixture {
    posts: Arc<PostInteractor>,
    users: Arc<UserInteractor>,
    author: Actor,
    other: Actor,
}

async fn fixture() -> Fixture {
    let post_repo = Arc::new(FakePostRepo::new());
    let user_repo = Arc::new(FakeUserRepo::new());
    let clock = Arc::new(FixedClock);

    let users = Arc::new(UserInteractor::new(user_repo, clock.clone()));
    let posts = Arc::new(PostInteractor::new(post_repo, clock));

    users
        .create_user(CreateUserCmd {
            username: "author".into(),
            email: None,
            display_name: Some("作者甲".into()),
        })
        .await
        .unwrap();
    users
        .create_user(CreateUserCmd {
            username: "other".into(),
            email: None,
            display_name: None,
        })
        .await
        .unwrap();

    let author = users.actor_for_username("author").await.unwrap();
    let other = users.actor_for_username("other").await.unwrap();

    Fixture {
        posts,
        users,
        author,
        other,
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

    let shown = f.posts.find("parallel").await.unwrap();
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
    let shown = f.posts.find("shown").await.unwrap();
    assert_eq!(shown.id, created.id);
    // deleted_at 过滤行为由 infrastructure 集成测试覆盖（M1 未开放删除用例）。
}

#[tokio::test]
async fn actor_resolution_rejects_unknown_user() {
    let f = fixture().await;
    let err = f.users.actor_for_username("ghost").await.unwrap_err();
    assert!(matches!(err, UseCaseError::NotFound(_)));
}
