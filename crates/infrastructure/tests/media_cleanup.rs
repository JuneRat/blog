mod common;
use application::media_cleanup::*;
use infrastructure::{SystemClock, media_cleanup::*};
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};
use uuid::Uuid;

struct Fixture {
    pool: PgPool,
    root: PathBuf,
    service: Arc<MediaCleanup>,
    store: Arc<PostgresMediaPurgeStore>,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}
impl Fixture {
    async fn new(name: &str) -> Self {
        let pool = common::fresh_database(name).await;
        let root = std::env::temp_dir().join(format!("blog-purge-{}", Uuid::now_v7()));
        std::fs::create_dir_all(root.join("objects")).unwrap();
        let root = root.canonicalize().unwrap();
        let store = Arc::new(PostgresMediaPurgeStore::new(
            common::database(pool.clone()),
            None,
        ));
        let service = Arc::new(MediaCleanup::new(
            store.clone(),
            Arc::new(LocalMediaPurgeFiles::new(root.clone())),
            Arc::new(LocalMediaPurgePlans),
            Arc::new(SystemClock),
        ));
        Self {
            pool,
            root,
            service,
            store,
        }
    }
    async fn media(&self, path: &str, trashed: bool) -> Uuid {
        let id = Uuid::now_v7();
        std::fs::write(self.root.join(path), b"media bytes").unwrap();
        sqlx::query("INSERT INTO media(id,path,filename,mime_type,size,width,height,checksum_sha256,deleted_at) VALUES($1,$2,'test.png','image/png',11,1,1,$3,CASE WHEN $4 THEN now() ELSE NULL END)")
            .bind(id).bind(path).bind(format!("{:x}",Sha256::digest(b"media bytes"))).bind(trashed).execute(&self.pool).await.unwrap();
        id
    }
    async fn plan(&self, ids: Vec<Uuid>) -> PathBuf {
        let path = self.root.join(format!("plan-{}.json", Uuid::now_v7()));
        self.service.plan(ids, &path).await.unwrap();
        path
    }
    async fn apply(&self, path: &Path) -> Result<PurgeResult, application::UseCaseError> {
        self.service.apply(path, true, true).await
    }
}

#[tokio::test]
async fn plans_recheck_trash_versions_references_identity_and_recovery_isolation() {
    let f = Fixture::new("blog_test_purge_recheck").await;
    let mid = f.media("objects/object.png", false).await;
    assert!(
        f.service
            .plan(vec![mid], &f.root.join("active.json"))
            .await
            .is_err()
    );
    sqlx::query("UPDATE media SET deleted_at=now(),version=version+1 WHERE id=$1")
        .bind(mid)
        .execute(&f.pool)
        .await
        .unwrap();
    let stale = f.plan(vec![mid]).await;
    sqlx::query("UPDATE media SET version=version+1 WHERE id=$1")
        .bind(mid)
        .execute(&f.pool)
        .await
        .unwrap();
    assert!(
        f.apply(&stale)
            .await
            .unwrap_err()
            .to_string()
            .contains("stale media plan")
    );
    let plan = f.plan(vec![mid]).await;
    let user = common::seed_user(&f.pool, "purge-user").await;
    sqlx::query("UPDATE users SET avatar_media_id=$1 WHERE id=$2")
        .bind(mid)
        .bind(user)
        .execute(&f.pool)
        .await
        .unwrap();
    assert!(
        f.apply(&plan)
            .await
            .unwrap_err()
            .to_string()
            .contains("referenced")
    );
    assert!(
        f.service
            .plan(vec![mid], &f.root.join("referenced.json"))
            .await
            .is_err()
    );
    sqlx::query("UPDATE users SET avatar_media_id=NULL WHERE id=$1")
        .bind(user)
        .execute(&f.pool)
        .await
        .unwrap();
    std::fs::write(f.root.join("objects/object.png"), b"replacement").unwrap();
    assert!(
        f.apply(&plan)
            .await
            .unwrap_err()
            .to_string()
            .contains("file missing or changed")
    );
    std::fs::write(f.root.join("objects/object.png"), b"media bytes").unwrap();
    let mut loaded = LocalMediaPurgePlans.load(&plan).await.unwrap().plan;
    loaded.database.oid = "0".into();
    let wrong = f.root.join("wrong.json");
    LocalMediaPurgePlans.save(&wrong, &loaded).await.unwrap();
    assert!(
        f.apply(&wrong)
            .await
            .unwrap_err()
            .to_string()
            .contains("different database")
    );
    sqlx::query("COMMENT ON DATABASE blog_test_purge_recheck IS 'blog:recovery-isolated:test'")
        .execute(&f.pool)
        .await
        .unwrap();
    assert!(
        f.apply(&plan)
            .await
            .unwrap_err()
            .to_string()
            .contains("recovery-isolated")
    );
    assert!(f.root.join("objects/object.png").exists());
    f.pool.close().await;
}

#[tokio::test]
async fn audit_rollback_and_committed_receipts_allow_only_the_original_plan_to_resume() {
    let f = Fixture::new("blog_test_purge_receipts").await;
    let one = f.media("objects/one.png", true).await;
    let two = f.media("objects/有'引号.png", true).await;
    std::fs::write(f.root.join("objects/unregistered.png"), b"keep").unwrap();
    let plan = f.plan(vec![one, two]).await;
    sqlx::raw_sql("CREATE FUNCTION fail_media_purge() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'audit unavailable'; END $$; CREATE TRIGGER fail_media_purge BEFORE INSERT ON audit_logs FOR EACH ROW WHEN (NEW.action='media.purge') EXECUTE FUNCTION fail_media_purge()")
        .execute(&f.pool).await.unwrap();
    assert!(
        f.apply(&plan)
            .await
            .unwrap_err()
            .to_string()
            .contains("audit unavailable")
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM media")
            .fetch_one(&f.pool)
            .await
            .unwrap(),
        2
    );
    assert!(f.root.join("objects/one.png").exists());
    sqlx::raw_sql("DROP TRIGGER fail_media_purge ON audit_logs; DROP FUNCTION fail_media_purge()")
        .execute(&f.pool)
        .await
        .unwrap();
    let verified = LocalMediaPurgePlans.load(&plan).await.unwrap();
    // Model a lost commit acknowledgement: durable receipts exist, files have not been touched.
    f.store.commit(&verified).await.unwrap();
    assert!(f.root.join("objects/one.png").exists());
    let saved: Vec<serde_json::Value> =
        sqlx::query_scalar("SELECT to_jsonb(a) FROM audit_logs a WHERE action='media.purge'")
            .fetch_all(&f.pool)
            .await
            .unwrap();
    sqlx::query("DELETE FROM audit_logs WHERE action='media.purge'")
        .execute(&f.pool)
        .await
        .unwrap();
    assert!(
        f.apply(&plan)
            .await
            .unwrap_err()
            .to_string()
            .contains("missing media without this plan receipt")
    );
    for row in saved {
        sqlx::query(
            "INSERT INTO audit_logs SELECT * FROM jsonb_populate_record(NULL::audit_logs,$1)",
        )
        .bind(row)
        .execute(&f.pool)
        .await
        .unwrap();
    }
    let mut changed = verified.plan.clone();
    changed.operation_id = Uuid::now_v7();
    let different = f.root.join("different.json");
    LocalMediaPurgePlans
        .save(&different, &changed)
        .await
        .unwrap();
    assert!(f.apply(&different).await.is_err());
    std::fs::write(f.root.join("objects/one.png"), b"substituted").unwrap();
    assert!(f.apply(&plan).await.is_err());
    std::fs::write(f.root.join("objects/one.png"), b"media bytes").unwrap();
    // Simulate an earlier partial unlink; its receipt makes absence safe on retry.
    std::fs::remove_file(f.root.join("objects/one.png")).unwrap();
    let result = f.apply(&plan).await.unwrap();
    assert_eq!((result.files_deleted, result.files_already_absent), (1, 1));
    assert!(result.failures.is_empty());
    let result = f.apply(&plan).await.unwrap();
    assert_eq!((result.files_deleted, result.files_already_absent), (0, 2));
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM audit_logs WHERE action='media.purge'")
            .fetch_one(&f.pool)
            .await
            .unwrap(),
        2
    );
    assert!(f.root.join("objects/unregistered.png").exists());
    f.pool.close().await;
}

#[tokio::test]
async fn waiting_purge_observes_a_concurrent_restore_before_deleting_anything() {
    let f = Fixture::new("blog_test_purge_lock").await;
    let id = f.media("objects/object.png", true).await;
    let plan = f.plan(vec![id]).await;
    let mut tx = f.pool.begin().await.unwrap();
    let blocker: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    sqlx::query("UPDATE media SET deleted_at=NULL,version=version+1 WHERE id=$1")
        .bind(id)
        .execute(&mut *tx)
        .await
        .unwrap();
    let service = f.service.clone();
    let task = tokio::spawn(async move { service.apply(&plan, true, true).await });
    tokio::time::timeout(std::time::Duration::from_secs(3),async {
        loop {
            let waiting:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE datname=current_database() AND $1=ANY(pg_blocking_pids(pid)))").bind(blocker).fetch_one(&f.pool).await.unwrap();
            if waiting {break;}
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }).await.unwrap();
    tx.commit().await.unwrap();
    assert!(
        task.await
            .unwrap()
            .unwrap_err()
            .to_string()
            .contains("stale media plan")
    );
    assert!(f.root.join("objects/object.png").exists());
    f.pool.close().await;
}

#[tokio::test]
async fn private_exclusive_plans_detect_tampering_and_refuse_symbolic_links() {
    let f = Fixture::new("blog_test_purge_files").await;
    let mid = f.media("objects/object.png", true).await;
    let plan = f.plan(vec![mid]).await;
    let original = std::fs::read(&plan).unwrap();
    assert!(f.service.plan(vec![mid], &plan).await.is_err());
    assert_eq!(std::fs::read(&plan).unwrap(), original);
    #[cfg(unix)]
    {
        use std::os::unix::fs::{PermissionsExt, symlink};
        assert_eq!(
            std::fs::metadata(&plan).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let object = f.root.join("objects/object.png");
        let target = f.root.join("target");
        std::fs::rename(&object, &target).unwrap();
        symlink(&target, &object).unwrap();
        assert!(f.apply(&plan).await.is_err());
        std::fs::remove_file(&object).unwrap();
        std::fs::rename(&target, &object).unwrap();
        let objects = f.root.join("objects");
        let moved = f.root.join("moved");
        std::fs::rename(&objects, &moved).unwrap();
        symlink(&moved, &objects).unwrap();
        assert!(f.apply(&plan).await.is_err());
        std::fs::remove_file(&objects).unwrap();
        std::fs::rename(&moved, &objects).unwrap();
    }
    let mut document: serde_json::Value = serde_json::from_slice(&original).unwrap();
    document["plan"]["items"][0]["version"] = serde_json::json!(99);
    std::fs::write(&plan, serde_json::to_vec(&document).unwrap()).unwrap();
    assert!(
        f.apply(&plan)
            .await
            .unwrap_err()
            .to_string()
            .contains("damaged")
    );
    assert!(f.root.join("objects/object.png").exists());
    f.pool.close().await;
}
