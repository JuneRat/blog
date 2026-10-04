//! Revocation and business commits must have a single cross-connection order.
mod common;

use application::{
    UseCaseError,
    identity::{
        Actor, ActorChannel, BUILTIN_ROLES, PERMISSION_REGISTRY, UserInteractor, UserStores,
    },
    ports::{
        AccountAdministration, ContentRenderer, MediaRepository, PageRepository, PostRepository,
        RbacStore, RenderedContent, SettingsStore, SiteSettingsValue, UserQuery,
    },
};
use async_trait::async_trait;
use domain::{
    content::{Page, Post, PostPatch, Slug, Visibility},
    identity::UserId,
    media::{ImageFormat, ImageInfo, Media},
};
use infrastructure::{
    PostgresMediaRepository, PostgresPageRepository, PostgresPostRepository, PostgresRbacStore,
    PostgresSettingsStore, PostgresUserRepository, RenderingRuntime, SystemClock,
};
use sqlx::PgPool;
use std::{sync::Arc, time::Duration};
use time::OffsetDateTime;
use tokio::sync::Notify;
use uuid::Uuid;

async fn setup(name: &str) -> (PgPool, Actor) {
    let pool = common::fresh_database(name).await;
    let rbac = PostgresRbacStore::new(common::database(pool.clone()));
    rbac.sync_permission_registry(PERMISSION_REGISTRY)
        .await
        .unwrap();
    rbac.sync_builtin_roles(BUILTIN_ROLES).await.unwrap();
    let id = common::seed_user(&pool, "writer").await;
    rbac.assign_role(id, "admin", None.into()).await.unwrap();
    let actor = resolve(&pool, id).await;
    assert!(actor.audit_context().authorization.is_some());
    (pool, actor)
}
async fn resolve(pool: &PgPool, id: Uuid) -> Actor {
    let database = common::database(pool.clone());
    let users = Arc::new(PostgresUserRepository::new(database.clone()));
    UserInteractor::new(
        UserStores {
            query: users.clone(),
            profiles: users.clone(),
            accounts: users,
        },
        Arc::new(PostgresRbacStore::new(database.clone())),
        Arc::new(SystemClock),
        Arc::new(PostgresMediaRepository::new(database)),
    )
    .actor_for_user_id_with_channel(id, ActorChannel::Session)
    .await
    .unwrap()
}
fn post(actor: &Actor, slug: &str) -> Post {
    Post::create_draft(
        UserId(actor.user_id.0),
        Slug::new(slug).unwrap(),
        "Before".into(),
        None,
        "Original body".into(),
        Visibility::Public,
        OffsetDateTime::now_utc(),
    )
    .unwrap()
}
async fn count(pool: &PgPool, table: &str) -> i64 {
    sqlx::query_scalar(&format!("SELECT count(*) FROM {table}"))
        .fetch_one(pool)
        .await
        .unwrap()
}
async fn remove_role(pool: &PgPool, id: Uuid) {
    PostgresRbacStore::new(common::database(pool.clone()))
        .remove_role(id, "admin", None.into())
        .await
        .unwrap();
}

#[tokio::test]
async fn stale_permissions_reject_post_page_media_settings_and_identity_without_partial_changes() {
    let (pool, actor) = setup("blog_write_authz_stale").await;
    let db = common::database(pool.clone());
    let renderer = Arc::new(RenderingRuntime::default());
    let posts = PostgresPostRepository::new(db.clone(), renderer.clone());
    let pages = PostgresPageRepository::new(db.clone(), renderer);
    let media = PostgresMediaRepository::new(db.clone());
    let settings = PostgresSettingsStore::new(db.clone());
    let users = PostgresUserRepository::new(db);
    let mut original = post(&actor, "existing");
    posts
        .insert_post(&original, &[], actor.audit_context())
        .await
        .unwrap();
    let initial = posts.find_by_id(original.id().0).await.unwrap().unwrap();
    let page = Page::create_draft(
        Slug::new("about").unwrap(),
        "About".into(),
        "Body".into(),
        Visibility::Public,
        OffsetDateTime::now_utc(),
    )
    .unwrap();
    let id = Uuid::now_v7();
    let image = Media::uploaded(
        id,
        Some(actor.user_id.0),
        format!("objects/{id}.png"),
        "image.png",
        ImageInfo::new(ImageFormat::Png, 1, 1).unwrap(),
        1,
        "a".repeat(64),
        OffsetDateTime::now_utc(),
    )
    .unwrap();
    remove_role(&pool, actor.user_id.0).await;
    // Role changes deliberately keep authentication valid; permission recheck
    // must detect them without relying on users.auth_version.
    let before = users.find_by_id(actor.user_id.0).await.unwrap().unwrap();
    assert_eq!(
        before.auth_version,
        actor.audit_context().authorization.unwrap().auth_version
    );
    let audit_count = count(&pool, "audit_logs").await;
    let revisions = count(&pool, "content_revisions").await;
    original
        .edit(PostPatch {
            title: Some("Should not commit".into()),
            ..Default::default()
        })
        .unwrap();
    assert!(matches!(
        posts
            .commit_post(
                &original,
                initial.version,
                OffsetDateTime::now_utc(),
                None,
                actor.audit_context()
            )
            .await,
        Err(UseCaseError::Forbidden)
    ));
    assert!(matches!(
        posts
            .insert_post(&post(&actor, "new"), &[], actor.audit_context())
            .await,
        Err(UseCaseError::Forbidden)
    ));
    assert!(matches!(
        pages.insert_page(&page, actor.audit_context()).await,
        Err(UseCaseError::Forbidden)
    ));
    assert!(matches!(
        media.insert(&image, actor.audit_context()).await,
        Err(UseCaseError::Forbidden)
    ));
    assert!(matches!(
        settings
            .save_site(
                &SiteSettingsValue {
                    home_page_size: Some(20),
                    navigation: vec![],
                    time_zone: None,
                    title: Some("Site".into()),
                    description: None,
                    logo_media_id: None
                },
                0,
                OffsetDateTime::now_utc(),
                actor.audit_context()
            )
            .await,
        Err(UseCaseError::Forbidden)
    ));
    // Identity writers already take the exclusive lock and must not upgrade
    // from a shared lock or trust the old actor's management permissions.
    assert!(matches!(
        PostgresRbacStore::new(common::database(pool.clone()))
            .assign_role(actor.user_id.0, "admin", actor.audit_context())
            .await,
        Err(UseCaseError::Forbidden)
    ));
    assert_eq!(
        posts.find_by_id(original.id().0).await.unwrap().unwrap(),
        initial
    );
    assert_eq!(count(&pool, "posts").await, 1);
    assert_eq!(count(&pool, "pages").await, 0);
    assert_eq!(count(&pool, "media").await, 0);
    assert_eq!(count(&pool, "media_refs").await, 0);
    assert_eq!(count(&pool, "content_revisions").await, revisions);
    assert_eq!(count(&pool, "audit_logs").await, audit_count);
    assert!(settings.find_site().await.unwrap().is_none());
    // A new request sees the reduced permissions and can still update its own
    // profile; revocation does not silently disable the account.
    let fresh = resolve(&pool, actor.user_id.0).await;
    assert!(!fresh.has_permission("post.create"));
    pool.close().await;
}

struct PausedRenderer {
    entered: Notify,
    release: Notify,
}
#[async_trait]
impl ContentRenderer for PausedRenderer {
    async fn render_content(&self, source: &str) -> Result<RenderedContent, UseCaseError> {
        self.entered.notify_one();
        self.release.notified().await;
        RenderingRuntime::default().render_content(source).await
    }
}
#[tokio::test]
async fn revocation_during_render_rejects_the_previously_authorized_write() {
    let (pool, actor) = setup("blog_write_authz_render").await;
    let renderer = Arc::new(PausedRenderer {
        entered: Notify::new(),
        release: Notify::new(),
    });
    let repo = PostgresPostRepository::new(common::database(pool.clone()), renderer.clone());
    let draft = post(&actor, "rendering");
    let id = actor.user_id.0;
    let writer =
        tokio::spawn(async move { repo.insert_post(&draft, &[], actor.audit_context()).await });
    tokio::time::timeout(Duration::from_secs(10), renderer.entered.notified())
        .await
        .unwrap();
    remove_role(&pool, id).await;
    let audits = count(&pool, "audit_logs").await;
    renderer.release.notify_one();
    assert!(matches!(
        writer.await.unwrap(),
        Err(UseCaseError::Forbidden)
    ));
    assert_eq!(count(&pool, "posts").await, 0);
    assert_eq!(count(&pool, "content_revisions").await, 0);
    assert_eq!(count(&pool, "audit_logs").await, audits);
    pool.close().await;
}

async fn wait_identity_lock(pool: &PgPool, mode: &str, granted: bool) {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_locks WHERE locktype='advisory' AND classid=2048001 AND objid=1 AND mode=$1 AND granted=$2 AND database=(SELECT oid FROM pg_database WHERE datname=current_database()))")
                .bind(mode).bind(granted).fetch_one(pool).await.unwrap();
            if exists { break; }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }).await.expect("expected identity lock did not appear");
}
#[tokio::test]
async fn a_write_that_wins_the_identity_lock_commits_before_revocation_returns() {
    let (pool, actor) = setup("blog_write_authz_order").await;
    let mut blocker = pool.begin().await.unwrap();
    sqlx::query("SELECT pg_advisory_xact_lock(1129270868,1)")
        .execute(&mut *blocker)
        .await
        .unwrap();
    let repo = PostgresPostRepository::new(
        common::database(pool.clone()),
        Arc::new(RenderingRuntime::default()),
    );
    let draft = post(&actor, "first-writer");
    let context = actor.audit_context();
    let writer = tokio::spawn(async move { repo.insert_post(&draft, &[], context).await });
    wait_identity_lock(&pool, "ShareLock", true).await;
    let other = pool.clone();
    let id = actor.user_id.0;
    let revoker = tokio::spawn(async move { remove_role(&other, id).await });
    wait_identity_lock(&pool, "ExclusiveLock", false).await;
    assert!(!writer.is_finished());
    assert!(!revoker.is_finished());
    blocker.commit().await.unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        writer.await.unwrap().unwrap();
        revoker.await.unwrap();
    })
    .await
    .unwrap();
    assert_eq!(count(&pool, "posts").await, 1);
    let repo = PostgresPostRepository::new(
        common::database(pool.clone()),
        Arc::new(RenderingRuntime::default()),
    );
    assert!(matches!(
        repo.insert_post(&post(&actor, "too-late"), &[], actor.audit_context())
            .await,
        Err(UseCaseError::Forbidden)
    ));
    pool.close().await;
}

#[tokio::test]
async fn authentication_revocation_and_role_definition_changes_are_rechecked() {
    let (pool, actor) = setup("blog_write_authz_credentials").await;
    let db = common::database(pool.clone());
    let users = PostgresUserRepository::new(db.clone());
    let pages = PostgresPageRepository::new(db.clone(), Arc::new(RenderingRuntime::default()));
    let draft = Page::create_draft(
        Slug::new("credentials").unwrap(),
        "Title".into(),
        "Body".into(),
        Visibility::Public,
        OffsetDateTime::now_utc(),
    )
    .unwrap();
    users
        .revoke_authentication(actor.user_id.0, None.into())
        .await
        .unwrap();
    assert!(matches!(
        pages.insert_page(&draft, actor.audit_context()).await,
        Err(UseCaseError::Unauthenticated)
    ));
    let fresh = resolve(&pool, actor.user_id.0).await;
    // Changing a role's permission set need not touch user_roles or users.
    let mut admin = BUILTIN_ROLES
        .iter()
        .find(|r| r.slug == "admin")
        .unwrap()
        .clone();
    admin.permissions = &["page.read"];
    PostgresRbacStore::new(db)
        .sync_builtin_roles(&[admin])
        .await
        .unwrap();
    assert!(matches!(
        pages.insert_page(&draft, fresh.audit_context()).await,
        Err(UseCaseError::Forbidden)
    ));
    assert_eq!(count(&pool, "pages").await, 0);
    pool.close().await;
}

#[tokio::test]
async fn waiting_writer_reads_revocation_after_the_lock_even_with_repeatable_read_default() {
    let name = "blog_write_authz_snapshot";
    let (pool, actor) = setup(name).await;
    let url = common::test_db_url(&common::admin_url(), name);
    let writer_pool = sqlx::postgres::PgPoolOptions::new()
        .after_connect(|conn, _| {
            Box::pin(async move {
                sqlx::query("SET default_transaction_isolation='repeatable read'")
                    .execute(conn)
                    .await?;
                Ok(())
            })
        })
        .connect(&url)
        .await
        .unwrap();
    let mut revocation = pool.begin().await.unwrap();
    sqlx::query("SELECT pg_advisory_xact_lock(2048001,1)")
        .execute(&mut *revocation)
        .await
        .unwrap();
    sqlx::query("UPDATE users SET status='disabled',auth_version=auth_version+1 WHERE id=$1")
        .bind(actor.user_id.0)
        .execute(&mut *revocation)
        .await
        .unwrap();
    let repo = PostgresPostRepository::new(
        common::database(writer_pool.clone()),
        Arc::new(RenderingRuntime::default()),
    );
    let draft = post(&actor, "waiting");
    let writer =
        tokio::spawn(async move { repo.insert_post(&draft, &[], actor.audit_context()).await });
    wait_identity_lock(&pool, "ShareLock", false).await;
    revocation.commit().await.unwrap();
    let result = tokio::time::timeout(Duration::from_secs(10), writer)
        .await
        .unwrap()
        .unwrap();
    assert!(
        matches!(result, Err(UseCaseError::Unauthenticated)),
        "{result:?}"
    );
    assert_eq!(count(&pool, "posts").await, 0);
    writer_pool.close().await;
    pool.close().await;
}

#[tokio::test]
async fn account_edit_versions_and_added_roles_do_not_invalidate_authorization() {
    let (pool, actor) = setup("blog_write_authz_additions").await;
    PostgresRbacStore::new(common::database(pool.clone()))
        .assign_role(actor.user_id.0, "reader", None.into())
        .await
        .unwrap();
    sqlx::query("UPDATE users SET display_name='Renamed',version=version+1 WHERE id=$1")
        .bind(actor.user_id.0)
        .execute(&pool)
        .await
        .unwrap();
    let repo = PostgresPostRepository::new(
        common::database(pool.clone()),
        Arc::new(RenderingRuntime::default()),
    );
    repo.insert_post(
        &post(&actor, "still-authorized"),
        &[],
        actor.audit_context(),
    )
    .await
    .unwrap();
    assert_eq!(count(&pool, "posts").await, 1);
    pool.close().await;
}
