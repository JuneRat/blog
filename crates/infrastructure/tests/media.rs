//! 新媒体表的事务边界：引用保留/保护、公开来源元数据、并发与审计回滚。
mod common;
use application::{
    error::UseCaseError,
    ports::{
        MediaChangeOutcome, MediaRefGuard, MediaRepository, PageRepository, SettingsStore,
        SiteSettingsValue, UserProfileStore, UserQuery,
    },
};
use domain::{
    content::{Page, PagePatch, Slug, Visibility},
    media::{ImageFormat, ImageInfo, Media},
};
use infrastructure::{
    PostgresMediaRepository, PostgresPageRepository, PostgresSettingsStore, PostgresUserRepository,
};
use sqlx::PgPool;
use std::sync::Arc;
use time::OffsetDateTime;
use uuid::Uuid;

fn image(owner: Option<Uuid>) -> Media {
    let id = Uuid::now_v7();
    Media::uploaded(
        id,
        owner,
        format!("objects/{id}.png"),
        "photo.png",
        ImageInfo::new(ImageFormat::Png, 10, 10).unwrap(),
        29,
        "a".repeat(64),
        OffsetDateTime::now_utc(),
    )
    .unwrap()
}
async fn seed(pool: &PgPool) -> (PostgresMediaRepository, Media, Uuid) {
    let owner = common::seed_user(pool, "author").await;
    let repo = PostgresMediaRepository::new(common::database(pool.clone()));
    let media = image(Some(owner));
    repo.insert(&media, Some(owner).into()).await.unwrap();
    (repo, media, owner)
}
fn pages(pool: &PgPool) -> PostgresPageRepository {
    PostgresPageRepository::new(
        common::database(pool.clone()),
        Arc::new(infrastructure::RenderingRuntime::default()),
    )
}
fn page(slug: &str, content: String) -> Page {
    Page::create_draft(
        Slug::new(slug).unwrap(),
        "Page".into(),
        content,
        Visibility::Private,
        OffsetDateTime::now_utc(),
    )
    .unwrap()
}
fn body(id: Uuid) -> String {
    format!("![image](/media/{id})\n\n![again](/media/{id})")
}
async fn refs(pool: &PgPool, id: Uuid) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM media_refs WHERE media_id=$1")
        .bind(id)
        .fetch_one(pool)
        .await
        .unwrap()
}

#[tokio::test]
async fn active_and_trash_lists_support_nullable_uploaders_and_stable_pages() {
    let pool = common::fresh_database("blog_media_lists").await;
    let (repo, first, owner) = seed(&pool).await;
    let second = image(None);
    repo.insert(&second, None.into()).await.unwrap();
    let (rows, total) = repo.list(1, 0, false, None).await.unwrap();
    assert_eq!(total, 2);
    assert_eq!(rows[0].snapshot.id, second.id());
    assert_eq!(rows[0].owner_display, "未知上传者");
    assert!(rows[0].snapshot.owner_id.is_none());
    assert_eq!(
        repo.list(1, 1, false, None).await.unwrap().0[0].snapshot.id,
        first.id()
    );
    assert_eq!(
        repo.set_deleted(
            first.id(),
            1,
            true,
            OffsetDateTime::now_utc(),
            Some(owner).into()
        )
        .await
        .unwrap(),
        MediaChangeOutcome::Updated
    );
    assert_eq!(repo.list(24, 0, false, None).await.unwrap().1, 1);
    assert_eq!(
        repo.list(24, 0, true, None).await.unwrap().0[0].snapshot.id,
        first.id()
    );
    assert!(!repo.is_attachable(first.id()).await.unwrap());
    assert!(!repo.is_attachable(Uuid::now_v7()).await.unwrap());
    assert!(repo.find_by_id(first.id()).await.unwrap().is_some());
    pool.close().await;
}

#[tokio::test]
async fn trash_keeps_existing_references_and_restore_allows_new_sources() {
    let pool = common::fresh_database("blog_media_ref_lifecycle").await;
    let (repo, media, owner) = seed(&pool).await;
    let pages = pages(&pool);
    let mut original = page("original", body(media.id()));
    pages.insert_page(&original, None.into()).await.unwrap();
    assert_eq!(refs(&pool, media.id()).await, 1);
    repo.set_deleted(
        media.id(),
        1,
        true,
        OffsetDateTime::now_utc(),
        Some(owner).into(),
    )
    .await
    .unwrap();
    assert_eq!(refs(&pool, media.id()).await, 1);
    original
        .edit(PagePatch {
            title: Some("Updated title".into()),
            ..Default::default()
        })
        .unwrap();
    pages
        .commit_page(&original, 1, OffsetDateTime::now_utc(), None.into())
        .await
        .unwrap();
    let another = page("another", body(media.id()));
    assert!(matches!(
        pages.insert_page(&another, None.into()).await,
        Err(UseCaseError::Invalid(_))
    ));
    assert!(pages.find_by_id(another.id().0).await.unwrap().is_none());
    assert!(
        sqlx::query("DELETE FROM media WHERE id=$1")
            .bind(media.id())
            .execute(&pool)
            .await
            .is_err(),
        "外键必须阻止仍有引用时物理删除元数据"
    );
    repo.set_deleted(
        media.id(),
        2,
        false,
        OffsetDateTime::now_utc(),
        Some(owner).into(),
    )
    .await
    .unwrap();
    pages.insert_page(&another, None.into()).await.unwrap();
    assert_eq!(refs(&pool, media.id()).await, 2);
    original
        .edit(PagePatch {
            content: Some("No image".into()),
            ..Default::default()
        })
        .unwrap();
    pages
        .commit_page(&original, 2, OffsetDateTime::now_utc(), None.into())
        .await
        .unwrap();
    assert_eq!(refs(&pool, media.id()).await, 1);
    pool.close().await;
}

#[tokio::test]
async fn invalid_new_reference_rolls_back_content_html_version_and_reference_changes() {
    let pool = common::fresh_database("blog_media_ref_rollback").await;
    let (_, media, _) = seed(&pool).await;
    let pages = pages(&pool);
    let mut original = page("original", body(media.id()));
    pages.insert_page(&original, None.into()).await.unwrap();
    let before: (String, String, i64) =
        sqlx::query_as("SELECT content,content_html,version FROM pages WHERE id=$1")
            .bind(original.id().0)
            .fetch_one(&pool)
            .await
            .unwrap();
    original
        .edit(PagePatch {
            content: Some(body(Uuid::now_v7())),
            ..Default::default()
        })
        .unwrap();
    assert!(matches!(
        pages
            .commit_page(&original, 1, OffsetDateTime::now_utc(), None.into())
            .await,
        Err(UseCaseError::Invalid(_))
    ));
    let after: (String, String, i64) =
        sqlx::query_as("SELECT content,content_html,version FROM pages WHERE id=$1")
            .bind(original.id().0)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(before, after);
    assert_eq!(refs(&pool, media.id()).await, 1);
    pool.close().await;
}

#[tokio::test]
async fn concurrent_avatar_changes_commit_one_version_with_matching_refs_and_audit() {
    let pool = common::fresh_database("blog_media_avatar_cas").await;
    let (_, media, owner) = seed(&pool).await;
    let users = PostgresUserRepository::new(common::database(pool.clone()));
    let before = users.find_by_id(owner).await.unwrap().unwrap();
    let now = OffsetDateTime::now_utc();
    let (a, b) = tokio::join!(
        users.set_avatar(
            owner,
            Some(media.id()),
            before.version,
            now,
            Some(owner).into()
        ),
        users.set_avatar(owner, None, before.version, now, Some(owner).into()),
    );
    let (Ok(winner), Err(UseCaseError::VersionConflict)) = (match (a, b) {
        (a @ Ok(_), b) => (a, b),
        (a, b) => (b, a),
    }) else {
        panic!("exactly one avatar write must succeed");
    };
    let after = users.find_by_id(owner).await.unwrap().unwrap();
    assert_eq!(after.version, before.version + 1);
    assert_eq!(after.auth_version, before.auth_version);
    assert_eq!(after.version, winner.version);
    assert_eq!(after.avatar_media_id, winner.avatar_media_id);
    assert_eq!(
        refs(&pool, media.id()).await,
        i64::from(after.avatar_media_id.is_some())
    );
    let audits: i64 =
        sqlx::query_scalar("SELECT count(*) FROM audit_logs WHERE action='user.avatar.update'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(audits, 1);
    pool.close().await;
}

#[tokio::test]
async fn avatar_and_site_logo_preserve_old_trashed_refs_and_reject_new_ones() {
    let pool = common::fresh_database("blog_media_avatar_logo").await;
    let (repo, media, owner) = seed(&pool).await;
    let other = common::seed_user(&pool, "other").await;
    let users = PostgresUserRepository::new(common::database(pool.clone()));
    let settings = PostgresSettingsStore::new(common::database(pool.clone()));
    let now = OffsetDateTime::now_utc();
    let site = SiteSettingsValue {
        home_page_size: None,
        navigation: vec![],
        time_zone: None,
        title: Some("Site".into()),
        description: Some("Description".into()),
        logo_media_id: Some(media.id()),
    };
    users
        .set_avatar(owner, Some(media.id()), 1, now, Default::default())
        .await
        .unwrap();
    settings
        .save_site(&site, 0, now, None.into())
        .await
        .unwrap();
    assert_eq!(refs(&pool, media.id()).await, 2);
    let auth_version = users.find_by_id(owner).await.unwrap().unwrap().auth_version;
    repo.set_deleted(media.id(), 1, true, now, Some(owner).into())
        .await
        .unwrap();
    users
        .set_avatar(owner, Some(media.id()), 2, now, Default::default())
        .await
        .unwrap();
    settings
        .save_site(&site, 1, now, None.into())
        .await
        .unwrap();
    assert!(matches!(
        users
            .set_avatar(other, Some(media.id()), 1, now, Default::default())
            .await,
        Err(UseCaseError::Invalid(_))
    ));
    let other = users.find_by_id(other).await.unwrap().unwrap();
    assert!(other.avatar_media_id.is_none());
    assert_eq!(other.version, 1);
    assert_eq!(
        users.find_by_id(owner).await.unwrap().unwrap().auth_version,
        auth_version
    );
    users
        .set_avatar(owner, None, 3, now, Default::default())
        .await
        .unwrap();
    let empty_site = SiteSettingsValue {
        home_page_size: None,
        navigation: vec![],
        time_zone: None,
        logo_media_id: None,
        ..site.clone()
    };
    settings
        .save_site(&empty_site, 2, now, None.into())
        .await
        .unwrap();
    assert_eq!(refs(&pool, media.id()).await, 0);
    assert!(matches!(
        settings.save_site(&site, 3, now, None.into()).await,
        Err(UseCaseError::Invalid(_))
    ));
    assert_eq!(settings.find_site().await.unwrap().unwrap().version, 3);
    pool.close().await;
}

#[tokio::test]
async fn usage_metadata_respects_publish_time_visibility_and_soft_deleted_pages() {
    let pool = common::fresh_database("blog_media_usage").await;
    let (repo, media, owner) = seed(&pool).await;
    for kind in ["post", "page"] {
        for (slug, status, visibility, deleted, future) in [
            ("draft", "draft", "public", false, false),
            ("private", "published", "private", false, false),
            ("live", "published", "public", false, false),
            ("trash", "published", "public", true, false),
            ("future", "published", "public", false, true),
            ("scheduled", "scheduled", "public", false, true),
        ] {
            let id = Uuid::now_v7();
            let table = if kind == "post" { "posts" } else { "pages" };
            let mut tx = pool.begin().await.unwrap();
            let sql = format!(
                "INSERT INTO {table}(id,slug,title,content,content_html,content_render_version,status,visibility,published_at,deleted_at{}) VALUES($1,$2,'Title','Content','<p>Content</p>',1,$3,$4,now()+make_interval(secs => $5),CASE WHEN $6 THEN now() END{})",
                if kind == "post" { ",author_id" } else { "" },
                if kind == "post" { ",$7" } else { "" }
            );
            let query = sqlx::query(&sql)
                .bind(id)
                .bind(slug)
                .bind(status)
                .bind(visibility)
                .bind(if future { 3600f64 } else { -3600f64 })
                .bind(deleted);
            if kind == "post" {
                query.bind(owner).execute(&mut *tx).await.unwrap();
            } else {
                query.execute(&mut *tx).await.unwrap();
            }
            sqlx::query("INSERT INTO media_refs(media_id,source_type,source_id) VALUES($1,$2,$3)")
                .bind(media.id())
                .bind(kind)
                .bind(id)
                .execute(&mut *tx)
                .await
                .unwrap();
            tx.commit().await.unwrap();
        }
    }
    let usage = repo.usage_of(media.id()).await.unwrap();
    assert_eq!(usage.len(), 12);
    for row in usage {
        assert_eq!(
            row.public,
            row.slug == "live",
            "{} {}",
            row.source.kind().as_str(),
            row.slug
        );
        assert_eq!(row.deleted, row.slug == "trash");
    }
    pool.close().await;
}

#[tokio::test]
async fn media_changes_and_audits_commit_together_with_cas_and_noop_semantics() {
    let pool = common::fresh_database("blog_media_audit").await;
    let (repo, media, owner) = seed(&pool).await;
    let now = OffsetDateTime::now_utc();
    assert_eq!(
        repo.set_deleted(media.id(), 1, false, now, Some(owner).into())
            .await
            .unwrap(),
        MediaChangeOutcome::Unchanged
    );
    assert_eq!(
        repo.set_deleted(media.id(), 9, true, now, Some(owner).into())
            .await
            .unwrap(),
        MediaChangeOutcome::StaleVersion
    );
    assert_eq!(
        repo.set_deleted(Uuid::now_v7(), 1, true, now, Some(owner).into())
            .await
            .unwrap(),
        MediaChangeOutcome::Gone
    );
    repo.set_deleted(media.id(), 1, true, now, Some(owner).into())
        .await
        .unwrap();
    repo.set_deleted(media.id(), 2, false, now, Some(owner).into())
        .await
        .unwrap();
    let logs: Vec<(String, Option<Uuid>, serde_json::Value)> = sqlx::query_as(
        "SELECT action,actor_id,metadata FROM audit_logs WHERE target_id=$1 ORDER BY id",
    )
    .bind(media.id().to_string())
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(
        logs.iter().map(|l| l.0.as_str()).collect::<Vec<_>>(),
        ["media.upload", "media.trash", "media.restore"]
    );
    assert!(
        logs.iter()
            .all(|l| l.1 == Some(owner) && l.2.get("filename").is_none())
    );
    // 审计写入失败必须撤销业务写入，而不是让成功操作消失在日志外。
    sqlx::raw_sql("CREATE FUNCTION reject_media_audit() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'audit unavailable'; END $$; CREATE TRIGGER reject_media_audit BEFORE INSERT ON audit_logs FOR EACH ROW EXECUTE FUNCTION reject_media_audit();").execute(&pool).await.unwrap();
    assert!(
        repo.set_deleted(media.id(), 3, true, now, Some(owner).into())
            .await
            .is_err()
    );
    assert!(
        repo.find_by_id(media.id())
            .await
            .unwrap()
            .unwrap()
            .deleted_at
            .is_none()
    );
    let another = image(Some(owner));
    assert!(repo.insert(&another, Some(owner).into()).await.is_err());
    assert!(repo.find_by_id(another.id()).await.unwrap().is_none());
    pool.close().await;
}

#[tokio::test]
async fn concurrent_trash_and_new_reference_never_lose_a_committed_reference() {
    let pool = common::fresh_database("blog_media_ref_race").await;
    let (repo, first, owner) = seed(&pool).await;
    let pages = pages(&pool);
    for n in 0..8 {
        let media = if n == 0 {
            first.clone()
        } else {
            let m = image(Some(owner));
            repo.insert(&m, Some(owner).into()).await.unwrap();
            m
        };
        let page = page(&format!("race-{n}"), body(media.id()));
        let (write, trash) = tokio::join!(
            pages.insert_page(&page, None.into()),
            repo.set_deleted(
                media.id(),
                1,
                true,
                OffsetDateTime::now_utc(),
                Some(owner).into()
            )
        );
        assert_eq!(trash.unwrap(), MediaChangeOutcome::Updated);
        match write {
            Ok(_) => assert_eq!(refs(&pool, media.id()).await, 1),
            Err(UseCaseError::Invalid(_)) => {
                assert_eq!(refs(&pool, media.id()).await, 0);
                assert!(pages.find_by_id(page.id().0).await.unwrap().is_none());
            }
            other => panic!("unexpected write: {other:?}"),
        }
        assert!(
            repo.find_by_id(media.id())
                .await
                .unwrap()
                .unwrap()
                .deleted_at
                .is_some()
        );
    }
    pool.close().await;
}

#[tokio::test]
async fn html_reference_extraction_tracks_rendered_images_without_code_or_comment_ghosts() {
    let pool = common::fresh_database("blog_media_render_refs").await;
    let (repo, media, owner) = seed(&pool).await;
    let second = image(Some(owner));
    repo.insert(&second, Some(owner).into()).await.unwrap();
    let pages = pages(&pool);
    let content = format!(
        "<img alt=\">\" src=\"/media/{}\">\n\n{}\n\n<!-- <img src=\"/media/{}\"> -->\n\n```html\n<img src=\"/media/{}\">\n```",
        media.id(),
        body(media.id()),
        second.id(),
        second.id()
    );
    pages
        .insert_page(&page("html-images", content), None.into())
        .await
        .unwrap();
    assert_eq!(refs(&pool, media.id()).await, 1);
    assert_eq!(refs(&pool, second.id()).await, 0);
    pool.close().await;
}

#[tokio::test]
async fn series_cover_insert_and_update_synchronize_references_in_the_same_transaction() {
    use application::ports::SeriesRepository;
    let pool = common::fresh_database("blog_media_series_cover").await;
    let (repo, media, owner) = seed(&pool).await;
    let series_repo = infrastructure::PostgresSeriesRepository::new(common::database(pool.clone()));
    let mut series = domain::content::Series::new(
        "Series".into(),
        Slug::new("series").unwrap(),
        None,
        OffsetDateTime::now_utc(),
    )
    .unwrap();
    series
        .update("Series".into(), None, Some(Some(media.id())))
        .unwrap();
    series_repo.insert(&series, None.into()).await.unwrap();
    assert_eq!(refs(&pool, media.id()).await, 1);
    repo.set_deleted(
        media.id(),
        1,
        true,
        OffsetDateTime::now_utc(),
        Some(owner).into(),
    )
    .await
    .unwrap();
    series_repo
        .update(
            series.id(),
            "Renamed",
            None,
            Some(media.id()),
            1,
            None.into(),
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(refs(&pool, media.id()).await, 1);
    series_repo
        .update(series.id(), "Renamed", None, None, 2, None.into())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(refs(&pool, media.id()).await, 0);
    assert!(matches!(
        series_repo
            .update(
                series.id(),
                "Invalid",
                None,
                Some(media.id()),
                3,
                None.into()
            )
            .await,
        Err(UseCaseError::Invalid(_))
    ));
    assert_eq!(
        series_repo
            .find_by_slug("series")
            .await
            .unwrap()
            .unwrap()
            .name,
        "Renamed"
    );
    pool.close().await;
}

#[tokio::test]
async fn local_storage_cleanup_only_removes_expired_temporary_uploads() {
    use application::ports::MediaStorage;
    let dir = std::env::temp_dir().join(format!("blog-media-cleanup-{}", Uuid::now_v7()));
    let storage = infrastructure::LocalMediaStorage::new(&dir);
    storage.put_staged("objects/old.png", b"old").await.unwrap();
    storage.put_staged("objects/new.png", b"new").await.unwrap();
    storage
        .put_staged("objects/formal.png", b"formal")
        .await
        .unwrap();
    storage.promote("objects/formal.png").await.unwrap();
    let old = std::time::SystemTime::now() - std::time::Duration::from_secs(7200);
    std::fs::File::open(dir.join("staging/old.png"))
        .unwrap()
        .set_modified(old)
        .unwrap();
    std::fs::File::open(dir.join("objects/formal.png"))
        .unwrap()
        .set_modified(old)
        .unwrap();
    let cutoff = OffsetDateTime::now_utc() - time::Duration::hours(1);
    assert_eq!(storage.discard_orphaned_staging(cutoff).await.unwrap(), 1);
    assert!(dir.join("staging/new.png").exists());
    assert_eq!(
        storage.read("objects/formal.png").await.unwrap(),
        Some(b"formal".to_vec())
    );
    assert_eq!(storage.discard_orphaned_staging(cutoff).await.unwrap(), 0);
    tokio::fs::remove_dir_all(dir).await.unwrap();
}

#[tokio::test]
async fn migration_provides_ordered_indexes_for_admin_filters() {
    let pool = common::fresh_database("blog_admin_indexes_test").await;
    let mut tx = pool.begin().await.unwrap();
    sqlx::raw_sql("SET LOCAL enable_seqscan=off; SET LOCAL enable_bitmapscan=off; SET LOCAL plan_cache_mode=force_generic_plan")
        .execute(&mut *tx)
        .await
        .unwrap();
    // Check that the migrated schema offers ordered access, not planner cost choices on empty tables.
    for (query, index) in [
        (
            "EXPLAIN (COSTS OFF) SELECT id FROM media WHERE deleted_at IS NULL ORDER BY created_at DESC,id DESC LIMIT $1",
            "media_library_idx",
        ),
        (
            "EXPLAIN (COSTS OFF) SELECT id FROM media WHERE deleted_at IS NOT NULL ORDER BY created_at DESC,id DESC LIMIT $1",
            "media_trash_idx",
        ),
        (
            "EXPLAIN (COSTS OFF) SELECT id FROM audit_logs WHERE actor_id='00000000-0000-0000-0000-000000000001' ORDER BY created_at DESC,id DESC LIMIT $1",
            "audit_logs_actor_time_idx",
        ),
        (
            "EXPLAIN (COSTS OFF) SELECT id FROM audit_logs WHERE action='post.update' ORDER BY created_at DESC,id DESC LIMIT $1",
            "audit_logs_action_time_idx",
        ),
    ] {
        let plan: Vec<String> = sqlx::query_scalar(query)
            .bind(20i64)
            .fetch_all(&mut *tx)
            .await
            .unwrap();
        let plan = plan.join("\n");
        assert!(plan.contains(index), "{index}: {plan}");
        assert!(
            !plan.contains("Sort"),
            "ordered index must avoid a sort: {plan}"
        );
    }
}

#[tokio::test]
async fn filename_search_filters_before_paging_and_treats_wildcards_literally() {
    let pool = common::fresh_database("blog_media_filename_search").await;
    let repo = PostgresMediaRepository::new(common::database(pool.clone()));
    let mut ids = Vec::new();
    for name in [
        "old-SUNSET.jpg",
        "Sunset-river.png",
        "new-photo.png",
        "山景_100%.png",
        "山景X100X.png",
    ] {
        let mut snapshot = image(None).snapshot();
        snapshot.original_name = name.into();
        let media = Media::reconstitute(snapshot).unwrap();
        repo.insert(&media, None.into()).await.unwrap();
        ids.push(media.id());
    }
    let (matches, total) = repo.list(100, 0, false, Some("sunset")).await.unwrap();
    let (first, first_total) = repo.list(1, 0, false, Some("sunset")).await.unwrap();
    let (second, second_total) = repo.list(1, 1, false, Some("sunset")).await.unwrap();
    assert_eq!(
        matches.iter().map(|m| m.snapshot.id).collect::<Vec<_>>(),
        vec![ids[1], ids[0]]
    );
    assert_eq!(first[0].snapshot.id, ids[1]);
    assert_eq!(second[0].snapshot.id, ids[0]);
    assert_eq!(total, matches.len() as i64);
    assert_eq!((first_total, second_total), (total, total));
    let (literal, _) = repo.list(100, 0, false, Some("_100%")).await.unwrap();
    assert_eq!(
        literal.iter().map(|m| m.snapshot.id).collect::<Vec<_>>(),
        vec![ids[3]]
    );
    repo.set_deleted(ids[0], 1, true, OffsetDateTime::now_utc(), None.into())
        .await
        .unwrap();
    let (active, _) = repo.list(100, 0, false, Some("sunset")).await.unwrap();
    let (trash, _) = repo.list(100, 0, true, Some("sunset")).await.unwrap();
    assert_eq!(
        active.iter().map(|m| m.snapshot.id).collect::<Vec<_>>(),
        vec![ids[1]]
    );
    assert_eq!(
        trash.iter().map(|m| m.snapshot.id).collect::<Vec<_>>(),
        vec![ids[0]]
    );
    assert!(
        repo.list(100, 0, false, Some("absent"))
            .await
            .unwrap()
            .0
            .is_empty()
    );
    pool.close().await;
}
