//! 审计与业务提交同生共死，操作者不能被被修改用户替代。
mod common;

use application::identity::{BUILTIN_ROLES, PERMISSION_REGISTRY, PermissionDescriptor};
use application::ports::{
    AccountAdministration, CategoryRepository, ClearPasswordOutcome, OAuthAccountStore,
    OAuthConfigStore, PasswordCredentialStore, ProviderConfig, ProviderKind, RbacStore,
    SaveOutcome, SessionStore, SettingsStore, SiteSettingsValue, ThemeSettingsStore, UserQuery,
};
use domain::content::{Category, Slug};
use domain::identity::User;
use infrastructure::{
    PostgresCategoryRepository, PostgresOAuthAccountStore, PostgresOAuthConfigStore,
    PostgresRbacStore, PostgresSessionStore, PostgresSettingsStore, PostgresUserRepository,
};
use serde_json::{Value, json};
use sqlx::PgPool;
use time::OffsetDateTime;
use uuid::Uuid;

static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn context(actor: Uuid) -> application::audit::AuditContext {
    application::audit::AuditContext {
        actor_id: Some(actor),
        ip_address: Some("2001:db8::42".parse().unwrap()),
    }
}

async fn count(pool: &PgPool) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM audit_logs")
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn block_audit(pool: &PgPool) {
    sqlx::raw_sql("CREATE FUNCTION fail_audit() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'audit unavailable'; END $$; CREATE TRIGGER fail_audit BEFORE INSERT ON audit_logs FOR EACH ROW EXECUTE FUNCTION fail_audit()")
        .execute(pool).await.unwrap();
}

async fn unblock_audit(pool: &PgPool) {
    sqlx::raw_sql("DROP TRIGGER fail_audit ON audit_logs; DROP FUNCTION fail_audit()")
        .execute(pool)
        .await
        .unwrap();
}

async fn assert_actor(pool: &PgPool, action: &str, target: &str, actor: Uuid) -> Value {
    let rows: Vec<(Option<Uuid>, Value, Option<String>)> =
        sqlx::query_as("SELECT actor_id,metadata,host(ip_address) FROM audit_logs WHERE action=$1 AND target_id=$2")
            .bind(action)
            .bind(target)
            .fetch_all(pool)
            .await
            .unwrap();
    assert!(!rows.is_empty(), "missing {action}");
    for (actual, _, ip) in &rows {
        assert_eq!(ip.as_deref(), Some("2001:db8::42"), "{action}");
        assert_eq!(*actual, Some(actor), "{action}");
    }
    rows[0].1.clone()
}

#[tokio::test]
async fn credential_changes_record_the_operator_and_rollback_sessions_on_audit_failure() {
    let _guard = SERIAL.lock().await;
    let pool = common::fresh_database("blog_business_audit_test").await;
    let actor = common::seed_user(&pool, "operator").await;
    let user = User::new(
        "target",
        Some("private@example.com".into()),
        None,
        OffsetDateTime::now_utc(),
    )
    .unwrap();
    let id = user.snapshot().id;
    let target = id.to_string();
    let users = PostgresUserRepository::new(common::database(pool.clone()));
    let accounts = PostgresOAuthAccountStore::new(common::database(pool.clone()));
    let sessions = PostgresSessionStore::with_defaults(common::database(pool.clone()));
    users.insert(&user, context(actor)).await.unwrap();
    users
        .set_password_hash(id, "$argon2id$private-hash", context(actor))
        .await
        .unwrap();
    accounts
        .bind(
            id,
            "github",
            "private-subject",
            Some("private@example.com".into()),
            context(actor),
        )
        .await
        .unwrap();
    let before = users.find_by_id(id).await.unwrap().unwrap();
    let token = sessions.create(id, before.auth_version).await.unwrap();
    let audit_count = count(&pool).await;
    block_audit(&pool).await;
    let rejected = User::new("rejected", None, None, OffsetDateTime::now_utc()).unwrap();
    assert!(users.insert(&rejected, context(actor)).await.is_err());
    assert!(
        users
            .set_password_hash(id, "replacement", context(actor))
            .await
            .is_err()
    );
    assert!(
        users
            .compare_and_set_password_hash(
                id,
                Some("$argon2id$private-hash"),
                "replacement",
                context(actor)
            )
            .await
            .is_err()
    );
    assert!(users.clear_password_hash(id, context(actor)).await.is_err());
    assert!(
        users
            .clear_password_hash_guarded(id, context(actor))
            .await
            .is_err()
    );
    assert!(
        users
            .revoke_authentication(id, context(actor))
            .await
            .is_err()
    );
    assert!(
        accounts
            .bind(id, "other", "another-subject", None, context(actor))
            .await
            .is_err()
    );
    assert!(
        accounts
            .unbind(id, "github", "private-subject", context(actor))
            .await
            .is_err()
    );
    assert!(users.find_by_username("rejected").await.unwrap().is_none());
    assert_eq!(users.find_by_id(id).await.unwrap().unwrap(), before);
    assert_eq!(
        users.password_hash_of(id).await.unwrap().as_deref(),
        Some("$argon2id$private-hash")
    );
    assert!(sessions.validate(&token).await.unwrap().is_some());
    assert_eq!(accounts.list_for_user(id).await.unwrap().len(), 1);
    assert_eq!(count(&pool).await, audit_count);
    // Failed CAS and absent binding are legitimate no-ops even with auditing unavailable.
    assert_eq!(
        users
            .compare_and_set_password_hash(id, Some("stale"), "replacement", context(actor))
            .await
            .unwrap(),
        None
    );
    accounts
        .unbind(id, "github", "absent", context(actor))
        .await
        .unwrap();
    unblock_audit(&pool).await;
    assert_eq!(
        users
            .clear_password_hash_guarded(id, context(actor))
            .await
            .unwrap(),
        ClearPasswordOutcome::Cleared
    );
    assert!(sessions.validate(&token).await.unwrap().is_none());
    let audit_count = count(&pool).await;
    assert_eq!(
        users
            .clear_password_hash_guarded(id, context(actor))
            .await
            .unwrap(),
        ClearPasswordOutcome::NoPassword
    );
    assert_eq!(count(&pool).await, audit_count);
    users
        .compare_and_set_password_hash(id, None, "$argon2id$new-private-hash", context(actor))
        .await
        .unwrap()
        .unwrap();
    accounts
        .unbind(id, "github", "private-subject", context(actor))
        .await
        .unwrap();
    users
        .revoke_authentication(id, context(actor))
        .await
        .unwrap();
    for action in [
        "user.create",
        "user.password.set",
        "user.password.clear",
        "user.oauth.bind",
        "user.oauth.unbind",
        "user.sessions.revoke",
    ] {
        assert_actor(&pool, action, &target, actor).await;
    }
    let metadata: Vec<Value> = sqlx::query_scalar("SELECT metadata FROM audit_logs")
        .fetch_all(&pool)
        .await
        .unwrap();
    let all = json!(metadata).to_string();
    for sensitive in [
        "private@example.com",
        "private-subject",
        "private-hash",
        "replacement",
    ] {
        assert!(!all.contains(sensitive), "audit leaked {sensitive}");
    }
}

#[tokio::test]
async fn role_and_registry_audits_are_atomic_and_idempotent() {
    let _guard = SERIAL.lock().await;
    let pool = common::fresh_database("blog_business_audit_test").await;
    let actor = common::seed_user(&pool, "role-operator").await;
    let target = common::seed_user(&pool, "role-target").await;
    let rbac = PostgresRbacStore::new(common::database(pool.clone()));
    rbac.sync_permission_registry(PERMISSION_REGISTRY)
        .await
        .unwrap();
    rbac.sync_builtin_roles(BUILTIN_ROLES).await.unwrap();
    let initial_count = count(&pool).await;
    rbac.sync_permission_registry(PERMISSION_REGISTRY)
        .await
        .unwrap();
    rbac.sync_builtin_roles(BUILTIN_ROLES).await.unwrap();
    assert_eq!(
        count(&pool).await,
        initial_count,
        "startup no-op must stay quiet"
    );
    rbac.assign_role(target, "author", context(actor))
        .await
        .unwrap();
    let assigned_count = count(&pool).await;
    rbac.assign_role(target, "author", context(actor))
        .await
        .unwrap();
    assert_eq!(count(&pool).await, assigned_count);
    let before: Value = sqlx::query_scalar("SELECT to_jsonb(u) FROM users u WHERE id=$1")
        .bind(target)
        .fetch_one(&pool)
        .await
        .unwrap();
    block_audit(&pool).await;
    assert!(
        rbac.assign_role(target, "editor", context(actor))
            .await
            .is_err()
    );
    assert!(
        rbac.remove_role(target, "author", context(actor))
            .await
            .is_err()
    );
    let mut permission: PermissionDescriptor = PERMISSION_REGISTRY[0].clone();
    permission.name = "Changed label";
    assert!(rbac.sync_permission_registry(&[permission]).await.is_err());
    let mut role = BUILTIN_ROLES
        .iter()
        .find(|r| r.slug == "author")
        .unwrap()
        .clone();
    role.permissions = &["post.read"];
    assert!(rbac.sync_builtin_roles(&[role]).await.is_err());
    // Exact original registries remain no-ops: failed synchronization changed nothing.
    rbac.sync_permission_registry(PERMISSION_REGISTRY)
        .await
        .unwrap();
    rbac.sync_builtin_roles(BUILTIN_ROLES).await.unwrap();
    let after: Value = sqlx::query_scalar("SELECT to_jsonb(u) FROM users u WHERE id=$1")
        .bind(target)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(before, after);
    assert_eq!(rbac.roles_of_user(target).await.unwrap(), vec!["author"]);
    assert_eq!(count(&pool).await, assigned_count);
    unblock_audit(&pool).await;
    rbac.remove_role(target, "author", context(actor))
        .await
        .unwrap();
    let removed_count = count(&pool).await;
    rbac.remove_role(target, "author", context(actor))
        .await
        .unwrap();
    assert_eq!(count(&pool).await, removed_count);
    for action in ["user.role.assign", "user.role.remove"] {
        assert_actor(&pool, action, &target.to_string(), actor).await;
    }
    let system_actors: i64 = sqlx::query_scalar("SELECT count(*) FROM audit_logs WHERE action IN ('permissions.sync','role.create') AND actor_id IS NOT NULL").fetch_one(&pool).await.unwrap();
    assert_eq!(system_actors, 0);
}

#[tokio::test]
async fn settings_and_category_audit_failures_restore_values_versions_and_logo_refs() {
    let _guard = SERIAL.lock().await;
    let pool = common::fresh_database("blog_business_audit_test").await;
    let actor = common::seed_user(&pool, "settings-operator").await;
    let categories = PostgresCategoryRepository::new(common::database(pool.clone()));
    let settings = PostgresSettingsStore::new(common::database(pool.clone()));
    let oauth = PostgresOAuthConfigStore::new(common::database(pool.clone()));
    let now = OffsetDateTime::now_utc();
    let category = Category::new(
        "Category".into(),
        Slug::new("category").unwrap(),
        None,
        None,
        now,
    )
    .unwrap();
    let cid = category.snapshot().id;
    categories.insert(&category, context(actor)).await.unwrap();
    categories
        .update(cid, "Updated", None, None, 1, context(actor))
        .await
        .unwrap()
        .unwrap();
    let mid = Uuid::now_v7();
    sqlx::query("INSERT INTO media(id,path,filename,mime_type,size,width,height,checksum_sha256) VALUES($1,'objects/logo.png','logo.png','image/png',1,1,1,repeat('a',64))").bind(mid).execute(&pool).await.unwrap();
    let site = SiteSettingsValue {
        title: Some("Site".into()),
        description: None,
        logo_media_id: Some(mid),
    };
    settings
        .save_site(&site, 0, now, context(actor))
        .await
        .unwrap();
    settings
        .save_theme("default", 0, now, context(actor))
        .await
        .unwrap();
    let providers = [ProviderConfig {
        id: "idp".into(),
        name: None,
        kind: ProviderKind::Oidc,
        issuer: Some("https://idp.example".into()),
        client_id: "private-client".into(),
        secret_ref: "PRIVATE_SECRET".into(),
        scopes: vec![],
    }];
    oauth.save(&providers, context(actor)).await.unwrap();
    let snapshot = "SELECT jsonb_build_object('settings',(SELECT jsonb_agg(s ORDER BY key) FROM settings s),'categories',(SELECT jsonb_agg(c ORDER BY id) FROM categories c),'refs',(SELECT jsonb_agg(r ORDER BY media_id,source_type,source_id) FROM media_refs r))";
    let before: Value = sqlx::query_scalar(snapshot).fetch_one(&pool).await.unwrap();
    let audit_count = count(&pool).await;
    block_audit(&pool).await;
    let rejected = Category::new(
        "Rejected".into(),
        Slug::new("rejected").unwrap(),
        None,
        None,
        now,
    )
    .unwrap();
    assert!(categories.insert(&rejected, context(actor)).await.is_err());
    assert!(
        categories
            .update(cid, "Rejected", None, None, 2, context(actor))
            .await
            .is_err()
    );
    assert!(categories.delete(cid, 2, context(actor)).await.is_err());
    let without_logo = SiteSettingsValue {
        logo_media_id: None,
        ..site.clone()
    };
    assert!(
        settings
            .save_site(&without_logo, 1, now, context(actor))
            .await
            .is_err()
    );
    assert!(
        settings
            .save_theme("paper", 1, now, context(actor))
            .await
            .is_err()
    );
    assert!(oauth.save(&[], context(actor)).await.is_err());
    assert!(
        categories
            .update(cid, "Stale", None, None, 1, context(actor))
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        settings
            .save_site(&without_logo, 0, now, context(actor))
            .await
            .unwrap(),
        SaveOutcome::StaleConflict
    );
    assert_eq!(
        settings
            .save_theme("paper", 0, now, context(actor))
            .await
            .unwrap(),
        SaveOutcome::StaleConflict
    );
    oauth.save(&providers, context(actor)).await.unwrap();
    let after: Value = sqlx::query_scalar(snapshot).fetch_one(&pool).await.unwrap();
    assert_eq!(before, after);
    assert_eq!(count(&pool).await, audit_count);
    unblock_audit(&pool).await;
    categories.delete(cid, 2, context(actor)).await.unwrap();
    for action in ["category.create", "category.update", "category.purge"] {
        assert_actor(&pool, action, &cid.to_string(), actor).await;
    }
    for key in ["site", "theme", "oauth"] {
        let metadata = assert_actor(&pool, &format!("settings.{key}"), key, actor).await;
        assert!(!metadata.to_string().contains("private-client"));
        assert!(!metadata.to_string().contains("PRIVATE_SECRET"));
    }
}

#[tokio::test]
async fn html_rebuild_audits_roll_back_derived_content_and_references() {
    let _guard = SERIAL.lock().await;
    let pool = common::fresh_database("blog_business_audit_test").await;
    let user = common::seed_user(&pool, "rebuild-author").await;
    let media = Uuid::now_v7();
    let post = Uuid::now_v7();
    let page = Uuid::now_v7();
    let comment = Uuid::now_v7();
    sqlx::query("INSERT INTO media(id,path,filename,mime_type,size,width,height,checksum_sha256) VALUES($1,'objects/rebuild.png','rebuild.png','image/png',1,1,1,repeat('b',64))")
        .bind(media).execute(&pool).await.unwrap();
    let source = format!("![Image](/media/{media})");
    sqlx::query("INSERT INTO posts(id,author_id,title,slug,content,content_html,content_render_version) VALUES($1,$2,'Post','rebuild-post',$3,'stale',2)")
        .bind(post).bind(user).bind(&source).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO pages(id,title,slug,content,content_html,content_render_version) VALUES($1,'Page','rebuild-page',$2,'stale',2)")
        .bind(page).bind(&source).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO comments(id,post_id,author_name,content,content_html,content_render_version) VALUES($1,$2,'Reader','**Private comment source**','stale',2)")
        .bind(comment).bind(post).execute(&pool).await.unwrap();
    use application::html_rebuild::{HtmlKind, HtmlRebuildStore};
    let renderer = std::sync::Arc::new(infrastructure::RenderingRuntime::default());
    let store = infrastructure::PostgresHtmlRebuildStore::new(
        common::database(pool.clone()),
        renderer.clone(),
        renderer,
    );
    let snapshot = "SELECT jsonb_build_array((SELECT to_jsonb(p) FROM posts p),(SELECT to_jsonb(p) FROM pages p),(SELECT to_jsonb(c) FROM comments c))";
    let before: Value = sqlx::query_scalar(snapshot).fetch_one(&pool).await.unwrap();
    block_audit(&pool).await;
    for kind in [HtmlKind::Post, HtmlKind::Page, HtmlKind::Comment] {
        let error = store.rebuild_batch(kind, None, 100).await.unwrap_err();
        assert_eq!(error.progress.rebuilt, 0);
        assert!(error.id.is_some());
    }
    let after: Value = sqlx::query_scalar(snapshot).fetch_one(&pool).await.unwrap();
    assert_eq!(before, after);
    let refs: i64 = sqlx::query_scalar("SELECT count(*) FROM media_refs")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(
        refs, 0,
        "failed rebuild must roll back newly extracted media refs"
    );
    assert_eq!(count(&pool).await, 0);
    unblock_audit(&pool).await;
    for kind in [HtmlKind::Post, HtmlKind::Page, HtmlKind::Comment] {
        assert_eq!(
            store.rebuild_batch(kind, None, 100).await.unwrap().rebuilt,
            1
        );
    }
    let after: Value = sqlx::query_scalar(snapshot).fetch_one(&pool).await.unwrap();
    for (before, after) in before
        .as_array()
        .unwrap()
        .iter()
        .zip(after.as_array().unwrap())
    {
        let mut expected = before.as_object().unwrap().clone();
        let mut actual = after.as_object().unwrap().clone();
        assert_ne!(
            expected.remove("content_html"),
            actual.remove("content_html")
        );
        expected.remove("content_render_version");
        actual.remove("content_render_version");
        assert_eq!(
            expected, actual,
            "business versions/timestamps and source stay unchanged"
        );
    }
    let audits: Vec<(Option<Uuid>, String, String, Value)> =
        sqlx::query_as("SELECT actor_id,action,target_id,metadata FROM audit_logs ORDER BY action")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(audits.len(), 3);
    for ((actor, action, target, metadata), (kind, id)) in
        audits
            .iter()
            .zip([("comment", comment), ("page", page), ("post", post)])
    {
        assert_eq!(*actor, None);
        assert_eq!(action, &format!("{kind}.html.rebuild"));
        assert_eq!(target, &id.to_string());
        assert_eq!(*metadata, json!({"version":1,"render_version":1}));
    }
    let refs: i64 = sqlx::query_scalar("SELECT count(*) FROM media_refs")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(refs, 2);
    for kind in [HtmlKind::Post, HtmlKind::Page, HtmlKind::Comment] {
        assert_eq!(
            store.rebuild_batch(kind, None, 100).await.unwrap().rebuilt,
            0
        );
    }
    assert_eq!(
        count(&pool).await,
        3,
        "unchanged render versions add no audit events"
    );
}
