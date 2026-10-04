mod common;
use application::{
    identity::{BUILTIN_ROLES, PERMISSION_REGISTRY},
    ports::{RbacStore, UserQuery},
    registration::{AccessPolicy, RegisterAccount, RegistrationInteractor, RegistrationStore},
};
use infrastructure::{PostgresRbacStore, PostgresRegistrationStore, PostgresUserRepository};
use std::sync::Arc;
use uuid::Uuid;

#[tokio::test]
async fn owner_migration_preserves_existing_administrators_and_grants() {
    let pool = common::fresh_database("blog_registration_migration_test").await;
    let old_owner = common::seed_user(&pool, "founder").await;
    let admin_user = common::seed_user(&pool, "administrator").await;
    let owner_role = Uuid::now_v7();
    let admin_role = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO roles(id,code,name) VALUES($1,'owner','Owner'),($2,'admin','Administrator')",
    )
    .bind(owner_role)
    .bind(admin_role)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::raw_sql("INSERT INTO permissions(code,name) VALUES('ownership.manage','Ownership'),('post.create','Create post');")
        .execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO role_permissions(role_id,permission_code) VALUES($1,'ownership.manage'),($1,'post.create')")
        .bind(owner_role).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO user_roles(user_id,role_id) VALUES($1,$3),($1,$4),($2,$4)")
        .bind(old_owner)
        .bind(admin_user)
        .bind(owner_role)
        .bind(admin_role)
        .execute(&pool)
        .await
        .unwrap();
    let mut tx = pool.begin().await.unwrap();
    sqlx::raw_sql(include_str!(
        "../../../migrations/postgres/0003_reader_registration.sql"
    ))
    .execute(&mut *tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let rbac = PostgresRbacStore::new(common::database(pool.clone()));
    assert!(matches!(
        rbac.permissions_of_role("owner").await,
        Err(application::UseCaseError::NotFound(_))
    ));
    for user in [old_owner, admin_user] {
        let permissions = rbac.permissions_of_user(user).await.unwrap();
        assert!(permissions.has("admin.manage"));
        assert!(permissions.has("post.create"));
    }
    rbac.sync_permission_registry(PERMISSION_REGISTRY)
        .await
        .unwrap();
    rbac.sync_builtin_roles(BUILTIN_ROLES).await.unwrap();
    assert!(
        !rbac
            .permissions_of_role("reader")
            .await
            .unwrap()
            .has("post.create")
    );
    assert!(
        rbac.permissions_of_role("editor")
            .await
            .unwrap()
            .has("post.create")
    );
}

#[tokio::test]
async fn registration_requires_email_and_accepts_an_omitted_nickname() {
    let pool = common::fresh_database("blog_registration_fields_test").await;
    let database = common::database(pool.clone());
    let rbac = PostgresRbacStore::new(database.clone());
    rbac.sync_permission_registry(PERMISSION_REGISTRY)
        .await
        .unwrap();
    rbac.sync_builtin_roles(BUILTIN_ROLES).await.unwrap();
    let store = Arc::new(PostgresRegistrationStore::new(database.clone()));
    store
        .save_policy(
            AccessPolicy {
                registration_enabled: true,
                ..Default::default()
            },
            None.into(),
        )
        .await
        .unwrap();
    let registration = RegistrationInteractor::new(
        store,
        Arc::new(infrastructure::Argon2PasswordHasher::with_defaults()),
        Arc::new(infrastructure::SystemClock),
    );
    let input = |email: &str| RegisterAccount {
        username: "reader".into(),
        email: email.into(),
        display_name: None,
        password: "harbor-lantern-2026".into(),
    };
    assert!(matches!(
        registration.register(input(""), None).await,
        Err(application::UseCaseError::Invalid(_))
    ));
    registration
        .register(input("reader@example.com"), None)
        .await
        .unwrap();
    let user = PostgresUserRepository::new(database)
        .find_by_username("reader")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(user.email.as_deref(), Some("reader@example.com"));
    assert_eq!(user.display_name, None);
    assert!(
        !rbac
            .permissions_of_user(user.id)
            .await
            .unwrap()
            .has("post.create")
    );
}
