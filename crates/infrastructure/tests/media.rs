//! 媒体仓储集成测试：真实 PostgreSQL 上验证引用关系、公开可见性边界与回收流程。
//!
//! 这些断言覆盖「数据在活动存储上的真实状态」——引用表由保存内容的事务推导，
//! 公开读取的判定实时查询内容可见性，删除前的引用保护在行锁内完成。
//! 用真实数据库而不是假仓储，正是因为这些都是跨表/跨事务不变量。
//!
//! 使用独立测试库 `blog_media_test`（与 postgres.rs 的 `blog_test` 隔离，两个测试
//! 二进制并行运行时不会互相 DROP DATABASE）。

use std::sync::Arc;

use application::error::UseCaseError;
use application::media::{MediaInteractor, RECLAIM_BATCH, STAGED_GRACE_SECS, UploadMediaCmd};
use application::ports::{
    MediaDeleteOutcome, MediaRefGuard, MediaRepository, MediaStorage, PageRepository,
    PostRepository, SaveOutcome, UserRepository,
};
use domain::content::page::Page;
use domain::content::post::{Post, Slug, Visibility};
use domain::identity::{User, UserId};
use domain::media::{Media, MediaSnapshot};
use infrastructure::{
    LocalMediaStorage, PostgresMediaRepository, PostgresPageRepository, PostgresPostRepository,
    PostgresUserRepository, connect, migrate,
};
use sqlx::PgPool;
use time::OffsetDateTime;
use tokio::sync::Mutex;
use uuid::Uuid;

const MEDIA_DB: &str = "blog_media_test";

/// 测试里反复用到的宽限期（秒），与生产常量保持一致。
const STAGED_GRACE: i64 = STAGED_GRACE_SECS;

static SERIAL: Mutex<()> = Mutex::const_new(());

fn admin_url() -> String {
    std::env::var("BLOG_TEST_ADMIN_URL")
        .unwrap_or_else(|_| "postgres://blog:blog@127.0.0.1:5432/postgres".into())
}

fn test_db_url(admin: &str) -> String {
    let base = admin.trim_end_matches('/');
    let idx = base
        .rfind('/')
        .expect("管理 DSN 缺少路径段，形如 postgres://user:pass@host:port/postgres");
    format!("{}/{MEDIA_DB}", &base[..idx])
}

fn assert_loopback(admin: &str) {
    let after_scheme = admin.split("://").nth(1).unwrap_or_default();
    let host_port = after_scheme
        .rsplit_once('@')
        .map(|(_, rest)| rest)
        .unwrap_or(after_scheme);
    let host = host_port.split([':', '/']).next().unwrap_or_default();
    assert!(
        matches!(host, "127.0.0.1" | "::1" | "localhost"),
        "拒绝在非 loopback 主机 {host} 上执行破坏性测试"
    );
}

/// 重建媒体测试库（串行使用）。
async fn fresh_pool() -> PgPool {
    let admin_dsn = admin_url();
    assert_loopback(&admin_dsn);
    let test_dsn = test_db_url(&admin_dsn);
    let admin = connect(&admin_dsn).await.expect("连接管理库失败");
    sqlx::raw_sql(&format!("DROP DATABASE IF EXISTS {MEDIA_DB} WITH (FORCE)"))
        .execute(&admin)
        .await
        .unwrap();
    sqlx::raw_sql(&format!("CREATE DATABASE {MEDIA_DB}"))
        .execute(&admin)
        .await
        .unwrap();
    admin.close().await;
    let pool = connect(&test_dsn).await.expect("连接测试库失败");
    migrate(&pool, "../../migrations/postgres")
        .await
        .expect("迁移失败");
    pool
}

async fn seed_user(pool: &PgPool, username: &str) -> Uuid {
    let user = User::new(
        username,
        None,
        Some(format!("{username} 的展示名")),
        OffsetDateTime::now_utc(),
    )
    .expect("构造用户失败");
    PostgresUserRepository::new(pool.clone())
        .insert(&user.snapshot())
        .await
        .expect("写入用户失败");
    user.id().0
}

/// 最小合法 PNG 头：格式嗅探与尺寸解析只需要 IHDR。
fn png_bytes(width: u32, height: u32) -> Vec<u8> {
    let mut bytes = b"\x89PNG\r\n\x1a\n\x00\x00\x00\x0dIHDR".to_vec();
    bytes.extend_from_slice(&width.to_be_bytes());
    bytes.extend_from_slice(&height.to_be_bytes());
    bytes.extend_from_slice(&[8, 6, 0, 0, 0]);
    bytes
}

/// 以真实用户身份构造受控通道 Actor。
///
/// `Actor::bootstrap_cli()` 的 user_id 是占位 nil，不能用于任何会写 owner_id 的
/// 用例（media_assets.owner_id 对 users 有外键）——上传必须带真实身份。
fn cli_actor(user_id: Uuid) -> application::identity::Actor {
    use application::identity::{Actor, ActorChannel, PERMISSION_REGISTRY};
    use domain::identity::{PermissionSet, UserId as DomainUserId};
    Actor::new(
        DomainUserId(user_id),
        ActorChannel::ControlledCli,
        PermissionSet::from_keys(PERMISSION_REGISTRY.iter().map(|d| d.key)),
    )
}

fn media_dir(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("blog-infra-media-{name}-{}", Uuid::now_v7()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn repo(pool: &PgPool) -> PostgresMediaRepository {
    PostgresMediaRepository::new(pool.clone())
}

/// 直接落一条 `ready` 资产（不经上传用例，聚焦仓储语义）。
async fn ready_media(pool: &PgPool, owner: Uuid, width: u32, height: u32) -> MediaSnapshot {
    let info = domain::media::inspect_image(&png_bytes(width, height)).unwrap();
    let id = Uuid::now_v7();
    let key = format!("objects/{id}.png");
    let media = Media::stage(
        id,
        owner,
        key,
        "photo.png",
        info,
        png_bytes(width, height).len() as u64,
        "a".repeat(64),
        OffsetDateTime::now_utc(),
    )
    .unwrap()
    .snapshot();
    let store = repo(pool);
    store.insert_staged(&media).await.unwrap();
    assert!(
        store
            .mark_ready(media.id, OffsetDateTime::now_utc())
            .await
            .unwrap()
    );
    store.find_by_id(media.id).await.unwrap().unwrap()
}

fn markdown_with(ids: &[Uuid]) -> String {
    let mut text = String::from("正文\n\n");
    for id in ids {
        text.push_str(&format!("![替代文字](/media/{id})\n\n"));
    }
    text
}

async fn seeded_post(pool: &PgPool, author: Uuid, slug: &str, content: String) -> PostSnapshotRow {
    let post = Post::create_draft(
        UserId(author),
        Slug::new(slug).unwrap(),
        format!("文章 {slug}"),
        None,
        content,
        Visibility::Public,
        OffsetDateTime::now_utc(),
    )
    .unwrap();
    let snapshot = post.snapshot();
    PostgresPostRepository::new(pool.clone())
        .insert(&snapshot, &[])
        .await
        .expect("写入文章失败");
    PostSnapshotRow {
        id: snapshot.id,
        version: snapshot.version,
    }
}

struct PostSnapshotRow {
    id: Uuid,
    version: i64,
}

/// 用新正文保存文章（引用集合由仓储从正文重新推导）。
async fn save_post_content(
    pool: &PgPool,
    row: &PostSnapshotRow,
    content: String,
) -> Result<SaveOutcome, UseCaseError> {
    let current = PostgresPostRepository::new(pool.clone())
        .find_by_id(row.id)
        .await?
        .expect("文章应存在");
    let mut next = current;
    next.content = content;
    let expected = next.version;
    PostgresPostRepository::new(pool.clone())
        .save(&next, expected, OffsetDateTime::now_utc(), None)
        .await
}

async fn refs_of(pool: &PgPool, media_id: Uuid) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM content_media_refs WHERE media_id = $1")
        .bind(media_id)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn media_status(pool: &PgPool, id: Uuid) -> String {
    sqlx::query_scalar("SELECT status FROM media_assets WHERE id = $1")
        .bind(id)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn set_post_status(pool: &PgPool, id: Uuid, status: &str, visibility: &str, deleted: bool) {
    sqlx::query(
        "UPDATE posts SET status = $2, visibility = $3, deleted_at = CASE WHEN $4 THEN now() ELSE NULL END, \
         published_at = CASE WHEN $2 = 'published' THEN COALESCE(published_at, now()) ELSE published_at END \
         WHERE id = $1",
    )
    .bind(id)
    .bind(status)
    .bind(visibility)
    .bind(deleted)
    .execute(pool)
    .await
    .unwrap();
}

async fn set_page_status(pool: &PgPool, id: Uuid, status: &str, visibility: &str) {
    sqlx::query(
        "UPDATE pages SET status = $2, visibility = $3, \
         published_at = CASE WHEN $2 = 'published' THEN COALESCE(published_at, now()) ELSE published_at END \
         WHERE id = $1",
    )
    .bind(id)
    .bind(status)
    .bind(visibility)
    .execute(pool)
    .await
    .unwrap();
}

// ---------------------------------------------------------------------------
// 引用关系：由保存内容的同一事务推导
// ---------------------------------------------------------------------------

#[tokio::test]
async fn saving_content_records_replaces_and_validates_media_references() {
    let _g = SERIAL.lock().await;
    let pool = fresh_pool().await;
    let author = seed_user(&pool, "author").await;
    let first = ready_media(&pool, author, 40, 30).await;
    let second = ready_media(&pool, author, 10, 10).await;

    let row = seeded_post(&pool, author, "refs", markdown_with(&[first.id])).await;
    assert_eq!(refs_of(&pool, first.id).await, 1, "创建时即写入引用");
    assert_eq!(refs_of(&pool, second.id).await, 0);

    // 换一张图：旧引用被替换，而不是累积。
    save_post_content(&pool, &row, markdown_with(&[second.id]))
        .await
        .unwrap();
    assert_eq!(refs_of(&pool, first.id).await, 0, "旧引用必须被整体替换");
    assert_eq!(refs_of(&pool, second.id).await, 1);

    // 清空正文：引用全部解除。
    save_post_content(&pool, &row, "没有图片了".into())
        .await
        .unwrap();
    assert_eq!(refs_of(&pool, second.id).await, 0);
}

#[tokio::test]
async fn saving_content_rejects_unknown_or_unavailable_media_and_rolls_back() {
    let _g = SERIAL.lock().await;
    let pool = fresh_pool().await;
    let author = seed_user(&pool, "author").await;
    let known = ready_media(&pool, author, 20, 20).await;
    let ghost = Uuid::now_v7();

    let row = seeded_post(&pool, author, "rollback", markdown_with(&[known.id])).await;

    let err = save_post_content(&pool, &row, markdown_with(&[ghost]))
        .await
        .unwrap_err();
    assert!(
        matches!(err, UseCaseError::Invalid(_)),
        "引用不存在的媒体必须报参数错误，实际：{err:?}"
    );
    // 事务回滚：正文与引用都保持修改前的状态。
    let content: String = sqlx::query_scalar("SELECT content FROM posts WHERE id = $1")
        .bind(row.id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(
        content.contains(&known.id.to_string()),
        "失败保存不得改动正文"
    );
    assert_eq!(refs_of(&pool, known.id).await, 1, "失败保存不得改动引用");
}

#[tokio::test]
async fn references_cannot_be_added_to_a_media_that_is_pending_deletion() {
    let _g = SERIAL.lock().await;
    let pool = fresh_pool().await;
    let author = seed_user(&pool, "author").await;
    let media = ready_media(&pool, author, 20, 20).await;
    let row = seeded_post(&pool, author, "no-new-refs", "空的".into()).await;

    // 进入回收流程（无引用，因此允许）。
    let outcome = repo(&pool)
        .begin_delete(media.id, media.version, OffsetDateTime::now_utc())
        .await
        .unwrap();
    assert_eq!(outcome, MediaDeleteOutcome::Marked);

    // 此刻再引用必须失败：否则会写入指向已回收文件的破图引用。
    let err = save_post_content(&pool, &row, markdown_with(&[media.id]))
        .await
        .unwrap_err();
    assert!(matches!(err, UseCaseError::Invalid(_)), "实际：{err:?}");
    assert_eq!(refs_of(&pool, media.id).await, 0);
}

/// 正文里用**原始 HTML** 写的图片也必须建立引用。
///
/// 渲染器保留 `<img src="/media/…">`，如果提取只看 Markdown 图片语法，
/// 这类正文能保存却不会产生引用：图片对匿名读者 404，且可能在仍被展示时被删除。
#[tokio::test]
async fn raw_html_images_in_content_are_recorded_as_references() {
    let _g = SERIAL.lock().await;
    let pool = fresh_pool().await;
    let author = seed_user(&pool, "author").await;
    let media = ready_media(&pool, author, 16, 16).await;
    let store = repo(&pool);

    // 原始 HTML 图片 + Markdown 图片混用。
    let second = ready_media(&pool, author, 8, 8).await;
    let content = format!(
        "正文\n\n<img src=\"/media/{}\" alt=\"原始 HTML\">\n\n![Markdown 图片](/media/{})\n",
        media.id, second.id
    );
    let row = seeded_post(&pool, author, "html-image", content).await;

    assert_eq!(
        refs_of(&pool, media.id).await,
        1,
        "原始 HTML 图片必须建立引用"
    );
    assert_eq!(refs_of(&pool, second.id).await, 1);

    // 与 Markdown 图片同等对待：公开发布后匿名可读，撤回后停止。
    assert!(!store.has_public_reference(media.id).await.unwrap());
    set_post_status(&pool, row.id, "published", "public", false).await;
    assert!(store.has_public_reference(media.id).await.unwrap());
    set_post_status(&pool, row.id, "draft", "public", false).await;
    assert!(!store.has_public_reference(media.id).await.unwrap());

    // 代码块里的示例不建立引用，也不会因此阻止删除。
    let harmless = ready_media(&pool, author, 4, 4).await;
    let fenced = format!("```html\n<img src=\"/media/{}\">\n```\n", harmless.id);
    save_post_content(&pool, &row, fenced).await.unwrap();
    assert_eq!(refs_of(&pool, harmless.id).await, 0);
    assert_eq!(
        store
            .begin_delete(harmless.id, harmless.version, OffsetDateTime::now_utc())
            .await
            .unwrap(),
        MediaDeleteOutcome::Marked,
        "代码块里的示例不应妨碍删除"
    );
}

/// 回归：注释掉的 `<img>` 与属性值含 `>` 的 `<img>` 都必须与渲染结果一致。
///
/// 这两个输入是手写 HTML 扫描必然出错的形状：
/// - 注释里的图片不渲染，若仍建立引用，图片就永远删不掉（幽灵占用）；
/// - 属性值里的 `>` 不结束标签，图片照常渲染，若漏掉引用，图片仍可能被删除（破图）。
#[tokio::test]
async fn reference_extraction_matches_what_actually_renders() {
    let _g = SERIAL.lock().await;
    let pool = fresh_pool().await;
    let author = seed_user(&pool, "author").await;
    let store = repo(&pool);

    let commented = ready_media(&pool, author, 8, 8).await;
    let gt_in_attribute = ready_media(&pool, author, 8, 8).await;
    let kept = ready_media(&pool, author, 8, 8).await;

    // 注释掉的图片：不建立引用，因此可以正常删除。
    let row = seeded_post(
        &pool,
        author,
        "commented",
        format!("正文\n\n<!-- <img src=\"/media/{}\"> -->\n", commented.id),
    )
    .await;
    assert_eq!(
        refs_of(&pool, commented.id).await,
        0,
        "注释里的图片不会渲染，不得建立引用"
    );
    assert_eq!(
        store
            .begin_delete(commented.id, commented.version, OffsetDateTime::now_utc())
            .await
            .unwrap(),
        MediaDeleteOutcome::Marked,
        "未被引用的图片必须可以删除（否则注释里的图片永远删不掉）"
    );

    // 属性值里的 `>`：图片照常渲染，必须建立引用并据此阻止删除。
    save_post_content(
        &pool,
        &row,
        format!(
            "<img alt=\">\" src=\"/media/{}\">\n\n<img src=\"/media/{}\">\n",
            gt_in_attribute.id, kept.id
        ),
    )
    .await
    .unwrap();
    assert_eq!(
        refs_of(&pool, gt_in_attribute.id).await,
        1,
        "属性值含 `>` 的图片会渲染，必须建立引用（否则会被误删）"
    );
    assert_eq!(refs_of(&pool, kept.id).await, 1);
    assert_eq!(
        store
            .begin_delete(
                gt_in_attribute.id,
                gt_in_attribute.version,
                OffsetDateTime::now_utc()
            )
            .await
            .unwrap(),
        MediaDeleteOutcome::Referenced { count: 1 },
        "仍在展示的图片必须受引用保护"
    );
}

// ---------------------------------------------------------------------------
// 公开访问边界
// ---------------------------------------------------------------------------

#[tokio::test]
async fn anonymous_access_follows_the_referencing_content_visibility() {
    let _g = SERIAL.lock().await;
    let pool = fresh_pool().await;
    let author = seed_user(&pool, "author").await;
    let media = ready_media(&pool, author, 32, 32).await;
    let store = repo(&pool);

    // 未被任何内容引用的新上传：默认不公开。
    assert!(
        !store.has_public_reference(media.id).await.unwrap(),
        "新上传默认不公开"
    );

    let row = seeded_post(&pool, author, "visibility", markdown_with(&[media.id])).await;
    // 草稿引用不构成公开来源。
    assert!(!store.has_public_reference(media.id).await.unwrap());

    set_post_status(&pool, row.id, "published", "public", false).await;
    assert!(
        store.has_public_reference(media.id).await.unwrap(),
        "公开发布的文章使其引用匿名可读"
    );

    set_post_status(&pool, row.id, "published", "private", false).await;
    assert!(
        !store.has_public_reference(media.id).await.unwrap(),
        "改为 private 后退出匿名读取"
    );

    set_post_status(&pool, row.id, "published", "public", false).await;
    assert!(store.has_public_reference(media.id).await.unwrap());

    // 撤回：published → draft。
    set_post_status(&pool, row.id, "draft", "public", false).await;
    assert!(
        !store.has_public_reference(media.id).await.unwrap(),
        "撤回后不再公开"
    );

    // 移入回收站。
    set_post_status(&pool, row.id, "published", "public", true).await;
    assert!(
        !store.has_public_reference(media.id).await.unwrap(),
        "回收站内容不授予公开权限"
    );
}

#[tokio::test]
async fn a_second_public_reference_keeps_the_media_public_after_one_is_withdrawn() {
    let _g = SERIAL.lock().await;
    let pool = fresh_pool().await;
    let author = seed_user(&pool, "author").await;
    let media = ready_media(&pool, author, 8, 8).await;
    let store = repo(&pool);

    let first = seeded_post(&pool, author, "first", markdown_with(&[media.id])).await;
    let second = seeded_post(&pool, author, "second", markdown_with(&[media.id])).await;
    set_post_status(&pool, first.id, "published", "public", false).await;
    set_post_status(&pool, second.id, "published", "public", false).await;
    assert!(store.has_public_reference(media.id).await.unwrap());

    // 撤回其中一篇：另一篇仍是公开来源。
    set_post_status(&pool, first.id, "draft", "public", false).await;
    assert!(
        store.has_public_reference(media.id).await.unwrap(),
        "仍有其他公开引用时必须保持匿名可读"
    );

    set_post_status(&pool, second.id, "draft", "public", false).await;
    assert!(
        !store.has_public_reference(media.id).await.unwrap(),
        "最后一个公开来源撤回后停止匿名访问"
    );
}

#[tokio::test]
async fn page_references_follow_page_visibility() {
    let _g = SERIAL.lock().await;
    let pool = fresh_pool().await;
    let author = seed_user(&pool, "author").await;
    let media = ready_media(&pool, author, 8, 8).await;
    let store = repo(&pool);

    let page = Page::create_draft(
        Slug::new("about").unwrap(),
        "关于".into(),
        markdown_with(&[media.id]),
        Visibility::Public,
        OffsetDateTime::now_utc(),
    )
    .unwrap();
    let snapshot = page.snapshot();
    let pages = PostgresPageRepository::new(pool.clone());
    pages.insert(&snapshot).await.unwrap();
    assert!(
        !store.has_public_reference(media.id).await.unwrap(),
        "草稿页面引用不公开"
    );

    set_page_status(&pool, snapshot.id, "published", "public").await;
    assert!(store.has_public_reference(media.id).await.unwrap());

    set_page_status(&pool, snapshot.id, "published", "private").await;
    assert!(!store.has_public_reference(media.id).await.unwrap());

    // 页面没有回收站：物理删除同时清理引用。
    set_page_status(&pool, snapshot.id, "published", "public").await;
    assert!(store.has_public_reference(media.id).await.unwrap());
    let deleted = pages
        .delete(snapshot.id, snapshot.version)
        .await
        .expect("删除页面失败");
    assert_eq!(
        deleted,
        application::ports::PageDeleteOutcome::Deleted,
        "页面应被删除"
    );
    assert_eq!(refs_of(&pool, media.id).await, 0, "页面删除必须清理引用");
    assert!(!store.has_public_reference(media.id).await.unwrap());
}

// ---------------------------------------------------------------------------
// 删除保护与回收
// ---------------------------------------------------------------------------

#[tokio::test]
async fn delete_is_refused_while_any_content_references_the_media() {
    let _g = SERIAL.lock().await;
    let pool = fresh_pool().await;
    let author = seed_user(&pool, "author").await;
    let media = ready_media(&pool, author, 16, 16).await;
    let store = repo(&pool);
    let row = seeded_post(&pool, author, "protected", markdown_with(&[media.id])).await;

    // 草稿引用也算占用：不能靠级联静默改变草稿。
    assert_eq!(
        store
            .begin_delete(media.id, media.version, OffsetDateTime::now_utc())
            .await
            .unwrap(),
        MediaDeleteOutcome::Referenced { count: 1 }
    );

    // 解除引用后才允许进入回收。
    save_post_content(&pool, &row, "已移除图片".into())
        .await
        .unwrap();
    assert_eq!(
        store
            .begin_delete(media.id, media.version, OffsetDateTime::now_utc())
            .await
            .unwrap(),
        MediaDeleteOutcome::Marked
    );
    // 第二次调用是幂等的（上次回收可能停在文件删除之前）。
    assert_eq!(
        store
            .begin_delete(media.id, media.version, OffsetDateTime::now_utc())
            .await
            .unwrap(),
        MediaDeleteOutcome::Marked,
        "pending_deletion 重放必须幂等"
    );
    assert!(
        store
            .confirm_deleted(media.id, OffsetDateTime::now_utc())
            .await
            .unwrap()
    );
    assert!(
        !store
            .confirm_deleted(media.id, OffsetDateTime::now_utc())
            .await
            .unwrap()
    );
    assert_eq!(
        store
            .begin_delete(media.id, media.version, OffsetDateTime::now_utc())
            .await
            .unwrap(),
        MediaDeleteOutcome::Gone,
        "已删除的资产对调用方等同于不存在"
    );
}

#[tokio::test]
async fn delete_checks_version_and_reports_unknown_ids() {
    let _g = SERIAL.lock().await;
    let pool = fresh_pool().await;
    let author = seed_user(&pool, "author").await;
    let media = ready_media(&pool, author, 16, 16).await;
    let store = repo(&pool);

    assert_eq!(
        store
            .begin_delete(media.id, media.version + 5, OffsetDateTime::now_utc())
            .await
            .unwrap(),
        MediaDeleteOutcome::StaleVersion
    );
    assert_eq!(
        store
            .begin_delete(Uuid::now_v7(), 1, OffsetDateTime::now_utc())
            .await
            .unwrap(),
        MediaDeleteOutcome::Gone
    );
    assert_eq!(
        media_status(&pool, media.id).await,
        "ready",
        "失败的删除请求不得改变状态"
    );
}

#[tokio::test]
async fn purge_clears_post_references_before_the_media_can_be_deleted() {
    let _g = SERIAL.lock().await;
    let pool = fresh_pool().await;
    let author = seed_user(&pool, "author").await;
    let media = ready_media(&pool, author, 16, 16).await;
    let posts = PostgresPostRepository::new(pool.clone());
    let store = repo(&pool);

    let row = seeded_post(&pool, author, "purgeme", markdown_with(&[media.id])).await;
    // 永久删除只对回收站文章生效。
    let trashed = posts
        .trash(row.id, row.version, OffsetDateTime::now_utc())
        .await
        .unwrap();
    let SaveOutcome::Saved { new_version } = trashed else {
        panic!("移入回收站应成功，实际：{trashed:?}");
    };
    // 回收站中的引用仍然是保留引用（内容可恢复）。
    assert_eq!(
        store
            .begin_delete(media.id, media.version, OffsetDateTime::now_utc())
            .await
            .unwrap(),
        MediaDeleteOutcome::Referenced { count: 1 }
    );

    let purged = posts.purge(row.id, new_version).await.unwrap();
    assert!(
        matches!(purged, SaveOutcome::Saved { .. }),
        "实际：{purged:?}"
    );
    assert_eq!(refs_of(&pool, media.id).await, 0, "永久删除必须清理引用");
    assert_eq!(
        store
            .begin_delete(media.id, media.version, OffsetDateTime::now_utc())
            .await
            .unwrap(),
        MediaDeleteOutcome::Marked
    );
}

/// 把文件的修改时间回拨，模拟「很久以前写完、随后被放弃」的暂存文件。
///
/// 回收的宽限期按文件的真实修改时间判定，因此测超期对象必须改时间，
/// 而不是靠一个生产代码里的测试开关。
fn age_file(path: &std::path::Path, older_than_secs: u64) {
    let file = std::fs::OpenOptions::new().write(true).open(path).unwrap();
    let when = std::time::SystemTime::now() - std::time::Duration::from_secs(older_than_secs);
    file.set_modified(when).expect("回拨文件修改时间失败");
}

#[tokio::test]
async fn reclaim_discards_interrupted_uploads_and_retries_file_deletion() {
    let _g = SERIAL.lock().await;
    let pool = fresh_pool().await;
    let author = seed_user(&pool, "author").await;
    let dir = media_dir("reclaim");
    let storage: Arc<dyn MediaStorage> = Arc::new(LocalMediaStorage::new(&dir));
    let interactor = MediaInteractor::new(
        Arc::new(PostgresMediaRepository::new(pool.clone())),
        storage.clone(),
        Arc::new(infrastructure::SystemClock),
    );
    let actor = cli_actor(author);

    // 中断的上传：文件在暂存区，数据库行停在 staged，且都已超过宽限期。
    let info = domain::media::inspect_image(&png_bytes(4, 4)).unwrap();
    let broken_id = Uuid::now_v7();
    let broken_key = format!("objects/{broken_id}.png");
    storage
        .put_staged(&broken_key, &png_bytes(4, 4))
        .await
        .unwrap();
    age_file(
        &dir.join("staging").join(format!("{broken_id}.png")),
        (STAGED_GRACE + 60) as u64,
    );
    let broken = Media::stage(
        broken_id,
        author,
        broken_key.clone(),
        "broken.png",
        info,
        png_bytes(4, 4).len() as u64,
        "b".repeat(64),
        OffsetDateTime::now_utc()
            - time::Duration::seconds(application::media::STAGED_GRACE_SECS + 60),
    )
    .unwrap()
    .snapshot();
    repo(&pool).insert_staged(&broken).await.unwrap();
    assert_eq!(media_status(&pool, broken.id).await, "staged");

    // 正常上传后直接进入回收：文件真实存在，等待删除确认。
    let uploaded = interactor
        .upload(
            &actor,
            UploadMediaCmd {
                file_name: "ok.png".into(),
                bytes: png_bytes(8, 8),
            },
        )
        .await
        .unwrap();
    let key: String = sqlx::query_scalar("SELECT storage_key FROM media_assets WHERE id = $1")
        .bind(uploaded.id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(storage.read(&key).await.unwrap().is_some());
    assert_eq!(
        repo(&pool)
            .begin_delete(uploaded.id, uploaded.version, OffsetDateTime::now_utc())
            .await
            .unwrap(),
        MediaDeleteOutcome::Marked
    );

    let report = interactor.reclaim(&actor).await.unwrap();
    assert_eq!(report.abandoned_staged, 1, "超期未完成的上传应被认领放弃");
    // 被放弃的上传也走 pending_deletion → deleted（这正是它可重试的原因），
    // 因此「完成删除」计的是两项：本次认领的与之前残留的。
    assert_eq!(report.deleted, 2, "认领的未完成上传与待回收资产都完成删除");
    assert_eq!(report.orphaned_staging_files, 0, "没有无记录的暂存残留");
    assert!(
        report.failures.is_empty(),
        "实际失败：{:?}",
        report.failures
    );
    assert_eq!(media_status(&pool, broken.id).await, "deleted");
    assert_eq!(media_status(&pool, uploaded.id).await, "deleted");
    assert_eq!(
        storage.read(&key).await.unwrap(),
        None,
        "正式文件必须被删除"
    );

    // 幂等重放：没有新的可回收项，也不报错。
    let again = interactor.reclaim(&actor).await.unwrap();
    assert_eq!(again.abandoned_staged, 0);
    assert_eq!(again.deleted, 0);
    assert_eq!(again.orphaned_staging_files, 0);
    assert!(again.failures.is_empty());

    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn upload_rejects_non_image_bytes_and_records_metadata() {
    let _g = SERIAL.lock().await;
    let pool = fresh_pool().await;
    let author = seed_user(&pool, "author").await;
    let dir = media_dir("upload");
    let storage = Arc::new(LocalMediaStorage::new(&dir));
    let interactor = MediaInteractor::new(
        Arc::new(PostgresMediaRepository::new(pool.clone())),
        storage.clone(),
        Arc::new(infrastructure::SystemClock),
    );
    let actor = cli_actor(author);

    // SVG 与任意文本都不接受：只开放常见位图。
    for (name, bytes) in [
        (
            "evil.svg",
            b"<svg xmlns=\"http://www.w3.org/2000/svg\"/>".to_vec(),
        ),
        ("note.txt", b"hello".to_vec()),
        ("empty.png", Vec::new()),
    ] {
        let err = interactor
            .upload(
                &actor,
                UploadMediaCmd {
                    file_name: name.into(),
                    bytes,
                },
            )
            .await
            .unwrap_err();
        assert!(matches!(err, UseCaseError::Invalid(_)), "{name}：{err:?}");
    }
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM media_assets")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 0, "被拒绝的上传不得留下任何记录");

    let dto = interactor
        .upload(
            &actor,
            UploadMediaCmd {
                file_name: "../../etc/passwd".into(),
                bytes: png_bytes(120, 60),
            },
        )
        .await
        .unwrap();
    assert_eq!(dto.width, 120);
    assert_eq!(dto.height, 60);
    assert_eq!(dto.mime, "image/png");
    assert_eq!(dto.original_name, "passwd", "展示名不得保留目录成分");
    assert_eq!(dto.byte_size as usize, png_bytes(120, 60).len());
    assert_eq!(dto.url, format!("/media/{}", dto.id));
    assert_eq!(dto.reference_count, 0);
    assert_eq!(dto.public_reference_count, 0);
    assert_eq!(dto.owner_display, "author 的展示名");

    // 未引用资产的匿名读取被拒（此处以「无 viewer 权限」等价表达）。
    let denied = interactor.read(dto.id, None).await.unwrap_err();
    assert!(matches!(denied, UseCaseError::NotFound(_)), "{denied:?}");

    // 库视图能读到它，且时间倒序。
    let page = interactor.list(&actor, 1).await.unwrap();
    assert_eq!(page.total, 1);
    assert_eq!(page.items[0].id, dto.id);

    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn usage_listing_reports_every_referencing_content() {
    let _g = SERIAL.lock().await;
    let pool = fresh_pool().await;
    let author = seed_user(&pool, "author").await;
    let media = ready_media(&pool, author, 16, 16).await;
    let store = repo(&pool);

    let public_post = seeded_post(&pool, author, "public-post", markdown_with(&[media.id])).await;
    let draft_post = seeded_post(&pool, author, "draft-post", markdown_with(&[media.id])).await;
    set_post_status(&pool, public_post.id, "published", "public", false).await;
    set_post_status(&pool, draft_post.id, "draft", "private", false).await;

    let usage = store.usage_of(media.id).await.unwrap();
    assert_eq!(usage.len(), 2);
    let public_row = usage.iter().find(|r| r.slug == "public-post").unwrap();
    assert!(public_row.public);
    assert_eq!(public_row.kind, application::ports::MediaContentKind::Post);
    let draft_row = usage.iter().find(|r| r.slug == "draft-post").unwrap();
    assert!(!draft_row.public, "草稿/私密引用不是公开来源");
    assert_eq!(draft_row.status, "draft");
    // 使用位置带出内容归属：应用层据此按 own/any 过滤展示。
    assert_eq!(draft_row.author_id, Some(author));

    let view = store.find_view(media.id).await.unwrap().unwrap();
    assert_eq!(view.reference_count, 2);
    assert_eq!(view.public_reference_count, 1);

    // 草稿文章移入回收站后仍占用引用，但不再是公开来源。
    set_post_status(&pool, public_post.id, "published", "public", true).await;
    let view = store.find_view(media.id).await.unwrap().unwrap();
    assert_eq!(view.reference_count, 2);
    assert_eq!(view.public_reference_count, 0);
}

#[tokio::test]
async fn staged_assets_are_not_listed_and_ready_assets_are_paginated() {
    let _g = SERIAL.lock().await;
    let pool = fresh_pool().await;
    let author = seed_user(&pool, "author").await;
    let store = repo(&pool);

    for index in 0..3 {
        ready_media(&pool, author, 4 + index, 4).await;
    }
    // 一条从未就绪的暂存行：不得出现在媒体库中。
    let info = domain::media::inspect_image(&png_bytes(2, 2)).unwrap();
    let staged_id = Uuid::now_v7();
    let staged = Media::stage(
        staged_id,
        author,
        format!("objects/{staged_id}.png"),
        "half.png",
        info,
        png_bytes(2, 2).len() as u64,
        "c".repeat(64),
        OffsetDateTime::now_utc(),
    )
    .unwrap()
    .snapshot();
    store.insert_staged(&staged).await.unwrap();

    let (items, total) = store.list(2, 0).await.unwrap();
    assert_eq!(total, 3, "总数只统计可用资产");
    assert_eq!(items.len(), 2, "分页上限生效");
    let (second_page, _) = store.list(2, 2).await.unwrap();
    assert_eq!(second_page.len(), 1);
    assert!(
        !items
            .iter()
            .chain(second_page.iter())
            .any(|row| row.snapshot.id == staged_id),
        "暂存资产不得出现在媒体库"
    );

    // 宽限期内的暂存资产不会被认领（它们可能正在被上传推进）。
    assert!(
        store
            .claim_abandoned_staged(
                OffsetDateTime::now_utc() - time::Duration::seconds(60),
                OffsetDateTime::now_utc(),
                10,
            )
            .await
            .unwrap()
            .is_empty(),
        "未超期的暂存资产必须保持原状"
    );
    assert_eq!(media_status(&pool, staged_id).await, "staged");
    // 超期后才可认领。
    assert_eq!(
        store
            .claim_abandoned_staged(
                OffsetDateTime::now_utc() + time::Duration::seconds(1),
                OffsetDateTime::now_utc(),
                10,
            )
            .await
            .unwrap()
            .len(),
        1
    );
    assert_eq!(media_status(&pool, staged_id).await, "pending_deletion");
    assert_eq!(store.list_pending_deletion(10).await.unwrap().len(), 1);
}

// ---------------------------------------------------------------------------
// 回收与上传的互斥（第 1 类缺陷的回归防线）
// ---------------------------------------------------------------------------

/// 回收**绝不能**删掉一个已经完成（或正在完成）上传的资产的文件。
///
/// 按时间顺序模拟「上传赢了」：行停在 `staged` 且已超期，随后上传完成
/// （promote + mark_ready），此时回收才开始扫描。资产已是 `ready`，
/// 认领不成立，文件必须原封不动；读取路径必须照常可用。
#[tokio::test]
async fn reclaim_never_touches_an_asset_whose_upload_won_the_race() {
    let _g = SERIAL.lock().await;
    let pool = fresh_pool().await;
    let author = seed_user(&pool, "author").await;
    let dir = media_dir("race-won");
    let storage: Arc<dyn MediaStorage> = Arc::new(LocalMediaStorage::new(&dir));
    let store = repo(&pool);
    let interactor = MediaInteractor::new(
        Arc::new(PostgresMediaRepository::new(pool.clone())),
        storage.clone(),
        Arc::new(infrastructure::SystemClock),
    );
    let actor = cli_actor(author);

    // 一次「很慢」的上传：行已超期仍是 staged，文件还在暂存区。
    let id = Uuid::now_v7();
    let key = format!("objects/{id}.png");
    storage.put_staged(&key, &png_bytes(6, 6)).await.unwrap();
    let staged = Media::stage(
        id,
        author,
        key.clone(),
        "slow.png",
        domain::media::inspect_image(&png_bytes(6, 6)).unwrap(),
        png_bytes(6, 6).len() as u64,
        "d".repeat(64),
        OffsetDateTime::now_utc() - time::Duration::seconds(STAGED_GRACE * 4),
    )
    .unwrap()
    .snapshot();
    store.insert_staged(&staged).await.unwrap();

    // 上传在回收扫描之前完成：文件移入正式位置，行标记 ready。
    storage.promote(&key).await.unwrap();
    assert!(
        store
            .mark_ready(id, OffsetDateTime::now_utc())
            .await
            .unwrap()
    );
    assert_eq!(media_status(&pool, id).await, "ready");

    // 现在才跑回收：它必须完全不碰这个资产。
    let report = interactor.reclaim(&actor).await.unwrap();
    assert_eq!(
        report.abandoned_staged, 0,
        "已就绪的资产不能被当作放弃的上传"
    );
    assert_eq!(report.deleted, 0);
    assert_eq!(media_status(&pool, id).await, "ready");
    let bytes = storage.read(&key).await.unwrap();
    assert!(
        bytes.is_some(),
        "回收删掉了刚就绪资产的文件——这正是要防的破图缺陷"
    );
    assert!(
        interactor.read(id, Some(&actor)).await.is_ok(),
        "就绪资产必须可以被读取"
    );

    std::fs::remove_dir_all(dir).unwrap();
}

/// 反向：回收先认领，则上传的 `staged → ready` 必须失败，且资产不会变成 `ready`。
#[tokio::test]
async fn upload_cannot_become_ready_after_reclaim_claimed_it() {
    let _g = SERIAL.lock().await;
    let pool = fresh_pool().await;
    let author = seed_user(&pool, "author").await;
    let dir = media_dir("race-lost");
    let storage: Arc<dyn MediaStorage> = Arc::new(LocalMediaStorage::new(&dir));
    let store = repo(&pool);
    let interactor = MediaInteractor::new(
        Arc::new(PostgresMediaRepository::new(pool.clone())),
        storage.clone(),
        Arc::new(infrastructure::SystemClock),
    );
    let actor = cli_actor(author);

    let id = Uuid::now_v7();
    let key = format!("objects/{id}.png");
    storage.put_staged(&key, &png_bytes(6, 6)).await.unwrap();
    age_file(
        &dir.join("staging").join(format!("{id}.png")),
        (STAGED_GRACE * 4) as u64,
    );
    let staged = Media::stage(
        id,
        author,
        key.clone(),
        "slow.png",
        domain::media::inspect_image(&png_bytes(6, 6)).unwrap(),
        png_bytes(6, 6).len() as u64,
        "d".repeat(64),
        OffsetDateTime::now_utc() - time::Duration::seconds(STAGED_GRACE * 4),
    )
    .unwrap()
    .snapshot();
    store.insert_staged(&staged).await.unwrap();

    let report = interactor.reclaim(&actor).await.unwrap();
    assert_eq!(report.abandoned_staged, 1);
    // 认领之后上传才走到「标记就绪」：必须失败，不能复活成可引用资产。
    assert!(
        !store
            .mark_ready(id, OffsetDateTime::now_utc())
            .await
            .unwrap(),
        "已被回收认领的资产不得再被标记就绪"
    );
    assert_eq!(media_status(&pool, id).await, "deleted");
    assert_eq!(storage.read(&key).await.unwrap(), None);

    std::fs::remove_dir_all(dir).unwrap();
}

/// 真并发：`staged → ready`（上传）与认领（回收）只有一个能赢，
/// 且任一结果都必须自洽——就绪则文件在，回收则文件不在。
#[tokio::test]
async fn concurrent_ready_and_reclaim_claim_are_mutually_exclusive() {
    let _g = SERIAL.lock().await;
    let pool = fresh_pool().await;
    let author = seed_user(&pool, "author").await;
    let dir = media_dir("race-concurrent");
    let storage: Arc<dyn MediaStorage> = Arc::new(LocalMediaStorage::new(&dir));
    let store: Arc<dyn MediaRepository> = Arc::new(PostgresMediaRepository::new(pool.clone()));
    let actor = cli_actor(author);

    for round in 0..8 {
        let id = Uuid::now_v7();
        let key = format!("objects/{id}.png");
        storage.put_staged(&key, &png_bytes(6, 6)).await.unwrap();
        let old = OffsetDateTime::now_utc() - time::Duration::seconds(STAGED_GRACE * 4);
        let staged = Media::stage(
            id,
            author,
            key.clone(),
            "race.png",
            domain::media::inspect_image(&png_bytes(6, 6)).unwrap(),
            png_bytes(6, 6).len() as u64,
            "f".repeat(64),
            old,
        )
        .unwrap()
        .snapshot();
        store.insert_staged(&staged).await.unwrap();
        // 上传在并发前把文件移入正式位置（真实流程中 promote 先于 mark_ready）。
        storage.promote(&key).await.unwrap();

        let now = OffsetDateTime::now_utc();
        let cutoff = now - time::Duration::seconds(STAGED_GRACE);
        let update = store.mark_ready(id, now);
        let claim = store.claim_abandoned_staged(cutoff, now, RECLAIM_BATCH);
        let (ready_won, claimed) = tokio::join!(update, claim);
        let ready_won = ready_won.unwrap();
        let claimed = claimed.unwrap();

        assert!(
            ready_won ^ !claimed.is_empty(),
            "第 {round} 轮：两个条件更新不得同时成功（ready={ready_won}, claimed={}）",
            claimed.len()
        );

        let status = media_status(&pool, id).await;
        if ready_won {
            assert_eq!(status, "ready");
            assert!(claimed.is_empty());
            // 就绪的资产文件必须存在：回收赢不了就不该动文件。
            assert!(storage.read(&key).await.unwrap().is_some());
        } else {
            assert_eq!(claimed.len(), 1);
            assert_eq!(status, "pending_deletion", "认领后停在可重试的待回收状态");
            // 回收确实负责删除该文件（这里显式完成删除，模拟 reclaim 的第二步）。
            storage.delete(&key).await.unwrap();
            assert!(store.confirm_deleted(id, now).await.unwrap());
            assert_eq!(media_status(&pool, id).await, "deleted");
            assert!(storage.read(&key).await.unwrap().is_none());
        }
    }

    let _ = actor;
    std::fs::remove_dir_all(dir).unwrap();
}

/// 宽限期：未超期的 `staged` 资产即使在回收运行后也必须保持原样。
#[tokio::test]
async fn reclaim_leaves_fresh_staged_uploads_alone() {
    let _g = SERIAL.lock().await;
    let pool = fresh_pool().await;
    let author = seed_user(&pool, "author").await;
    let dir = media_dir("grace");
    let storage: Arc<dyn MediaStorage> = Arc::new(LocalMediaStorage::new(&dir));
    let store = repo(&pool);
    let interactor = MediaInteractor::new(
        Arc::new(PostgresMediaRepository::new(pool.clone())),
        storage.clone(),
        Arc::new(infrastructure::SystemClock),
    );
    let actor = cli_actor(author);

    // 刚写入的暂存文件 + 刚插入的 staged 行（模拟正在进行的上传）。
    let id = Uuid::now_v7();
    let key = format!("objects/{id}.png");
    storage.put_staged(&key, &png_bytes(5, 5)).await.unwrap();
    let fresh = Media::stage(
        id,
        author,
        key.clone(),
        "fresh.png",
        domain::media::inspect_image(&png_bytes(5, 5)).unwrap(),
        png_bytes(5, 5).len() as u64,
        "e".repeat(64),
        OffsetDateTime::now_utc(),
    )
    .unwrap()
    .snapshot();
    store.insert_staged(&fresh).await.unwrap();

    let report = interactor.reclaim(&actor).await.unwrap();
    assert_eq!(report.abandoned_staged, 0, "宽限期内的上传不能被认领");
    assert_eq!(
        report.orphaned_staging_files, 0,
        "宽限期内的暂存文件不能被清扫"
    );
    assert_eq!(media_status(&pool, id).await, "staged");
    assert!(
        storage.read(&key).await.unwrap().is_none(),
        "未 promote 的资产只在暂存区"
    );
    assert!(
        dir.join("staging").join(format!("{id}.png")).exists(),
        "正在进行的上传，其暂存文件必须留存"
    );

    std::fs::remove_dir_all(dir).unwrap();
}

/// 没有数据库记录的暂存残留（写入文件后插入失败，或进程中途退出）必须被回收找到。
#[tokio::test]
async fn reclaim_sweeps_staging_orphans_without_a_database_row() {
    let _g = SERIAL.lock().await;
    let pool = fresh_pool().await;
    let author = seed_user(&pool, "author").await;
    let dir = media_dir("orphan");
    let storage = Arc::new(LocalMediaStorage::new(&dir));
    let interactor = MediaInteractor::new(
        Arc::new(PostgresMediaRepository::new(pool.clone())),
        storage.clone(),
        Arc::new(infrastructure::SystemClock),
    );
    let actor = cli_actor(author);

    // 孤儿一：超期的暂存文件，没有任何行指向它。
    let orphan_key = format!("objects/{}.png", Uuid::now_v7());
    storage
        .put_staged(&orphan_key, &png_bytes(3, 3))
        .await
        .unwrap();
    let orphan_name = std::path::Path::new(&orphan_key)
        .file_name()
        .unwrap()
        .to_owned();
    age_file(
        &dir.join("staging").join(&orphan_name),
        (STAGED_GRACE * 3) as u64,
    );

    // 孤儿二：写入过程中被中断的 `.part` 残留（put_staged 先写临时文件）。
    let part = dir.join("staging").join("interrupted.part");
    std::fs::write(&part, b"partial").unwrap();
    age_file(&part, (STAGED_GRACE * 3) as u64);

    // 对照组：刚写入、可能属于进行中上传的暂存文件，必须保留。
    let fresh_key = format!("objects/{}.png", Uuid::now_v7());
    storage
        .put_staged(&fresh_key, &png_bytes(3, 3))
        .await
        .unwrap();
    let fresh_name = std::path::Path::new(&fresh_key)
        .file_name()
        .unwrap()
        .to_owned();

    let report = interactor.reclaim(&actor).await.unwrap();
    assert_eq!(report.orphaned_staging_files, 2, "两个超期孤儿都应被清理");
    assert!(!dir.join("staging").join(&orphan_name).exists());
    assert!(!part.exists());
    assert!(
        dir.join("staging").join(&fresh_name).exists(),
        "宽限期内的暂存文件不得被清扫"
    );

    // 行表为空：这些清理完全不依赖数据库状态。
    let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM media_assets")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(rows, 0);

    std::fs::remove_dir_all(dir).unwrap();
}

/// 插入失败时上传会尽力清理无记录的暂存文件，不留孤儿。
#[tokio::test]
async fn failing_to_record_an_upload_removes_its_staged_file() {
    let _g = SERIAL.lock().await;
    let pool = fresh_pool().await;
    let dir = media_dir("insert-fails");
    let storage = Arc::new(LocalMediaStorage::new(&dir));
    // 让 insert_staged 失败：owner_id 对 users 有外键，用一个不存在的用户 id。
    let interactor = MediaInteractor::new(
        Arc::new(PostgresMediaRepository::new(pool.clone())),
        storage.clone(),
        Arc::new(infrastructure::SystemClock),
    );
    let ghost_owner = Uuid::now_v7();

    let err = interactor
        .upload(
            &cli_actor(ghost_owner),
            UploadMediaCmd {
                file_name: "ghost.png".into(),
                bytes: png_bytes(7, 7),
            },
        )
        .await
        .unwrap_err();
    assert!(matches!(err, UseCaseError::Repository(_)), "实际：{err:?}");

    let staging: Vec<_> = std::fs::read_dir(dir.join("staging"))
        .map(|entries| entries.filter_map(Result::ok).collect())
        .unwrap_or_default();
    assert!(
        staging.is_empty(),
        "插入失败后不得留下无记录的暂存文件：{staging:?}"
    );
    let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM media_assets")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(rows, 0);

    std::fs::remove_dir_all(dir).unwrap();
}

/// 头像的公开来源 = 账号未软删除：软删除后匿名读取立即失效，但引用仍占用
/// （图片不得被删，恢复账号后语义不漂移）。
#[tokio::test]
async fn avatar_public_source_follows_user_soft_deletion() {
    let _g = SERIAL.lock().await;
    let pool = fresh_pool().await;
    let user_id = seed_user(&pool, "avatar-owner").await;
    let media = ready_media(&pool, user_id, 12, 12).await;
    let users = PostgresUserRepository::new(pool.clone());
    let store = repo(&pool);
    let now = OffsetDateTime::now_utc();

    users
        .set_avatar(user_id, Some(media.id), now)
        .await
        .unwrap();
    assert!(
        store.has_public_reference(media.id).await.unwrap(),
        "未软删除账号的头像是公开来源"
    );

    // 软删除账号：公开来源立即消失，但引用（删除保护）仍在。
    sqlx::query("UPDATE users SET deleted_at = now() WHERE id = $1")
        .bind(user_id)
        .execute(&pool)
        .await
        .unwrap();
    assert!(
        !store.has_public_reference(media.id).await.unwrap(),
        "软删除后头像不再是公开来源"
    );
    assert_eq!(
        store
            .begin_delete(media.id, media.version, now)
            .await
            .unwrap(),
        MediaDeleteOutcome::Referenced { count: 1 },
        "引用仍在：删除必须被拒绝，恢复账号后语义不变"
    );

    // 恢复账号 → 清除头像：引用释放，图片可进入回收流程。
    // （软删除账号不允许改头像，这正是 set_avatar 的 deleted_at IS NULL 谓词。）
    sqlx::query("UPDATE users SET deleted_at = NULL WHERE id = $1")
        .bind(user_id)
        .execute(&pool)
        .await
        .unwrap();
    users.set_avatar(user_id, None, now).await.unwrap();
    assert_eq!(
        store
            .begin_delete(media.id, media.version, now)
            .await
            .unwrap(),
        MediaDeleteOutcome::Marked
    );
}

/// `attachable_status`：附着授权的数据面（归属 + 公开性）。
/// ready 资产给出 owner 与公开引用判定；草稿引用不算公开来源，
/// 随文章发布转为公开——与匿名读取同一谓词。
#[tokio::test]
async fn attachable_status_reports_owner_and_publicity() {
    let _g = SERIAL.lock().await;
    let pool = fresh_pool().await;
    let owner = seed_user(&pool, "attach-owner").await;
    let media = ready_media(&pool, owner, 10, 10).await;

    // ready 且无引用：归属可见，尚未公开。
    let status = MediaRefGuard::attachable_status(&repo(&pool), media.id)
        .await
        .unwrap()
        .expect("ready 资产应可附着");
    assert_eq!(status.owner_id, owner);
    assert!(!status.publicly_referenced);

    // 不存在的资产：None（调用方按「不存在或已不可用」处理）。
    assert!(
        repo(&pool)
            .attachable_status(Uuid::now_v7())
            .await
            .unwrap()
            .is_none(),
        "不存在的 id 不得给出附着状态"
    );

    // 被草稿文章引用：引用存在但不是公开来源。
    let author = seed_user(&pool, "attach-author").await;
    let row = seeded_post(
        &pool,
        author,
        "attach-status-post",
        markdown_with(&[media.id]),
    )
    .await;
    let status = MediaRefGuard::attachable_status(&repo(&pool), media.id)
        .await
        .unwrap()
        .unwrap();
    assert!(!status.publicly_referenced, "草稿引用不是公开来源");

    // 文章发布后成为公开来源。
    set_post_status(&pool, row.id, "published", "public", false).await;
    let status = MediaRefGuard::attachable_status(&repo(&pool), media.id)
        .await
        .unwrap()
        .unwrap();
    assert!(status.publicly_referenced, "公开文章的引用是公开来源");
}
