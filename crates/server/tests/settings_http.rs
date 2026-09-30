//! 站点设置 HTTP 集成测试：会话认证 + CSRF + settings.manage 授权 +
//! 优先级（数据库 > 内置默认值）+ 版本冲突 + 公开页面即时生效。
//!
//! 栈与生产装配同构：public_router（真实主题）+ auth_router + settings_router。

mod common;

use std::{
    future::Future,
    path::PathBuf,
    sync::{Arc, Mutex},
};

use application::auth::{AuthDeps, AuthInteractor};
use application::content::PostInteractor;
use application::html_rebuild::{RebuildCounts, RebuildReport};
use application::html_rebuild_admin::{
    HtmlRebuildAdminInteractor, HtmlRebuildJob, HtmlRebuildJobStatus, HtmlRebuildJobs,
    HtmlRebuildView,
};
use application::identity::{Actor, CreateUserCmd, RoleInteractor, UserInteractor};
use application::page::PageInteractor;
use application::ports::{
    Clock, ExternalIdentity, ExternalIdentityClient, OAuthAccountStore, OAuthConfigStore,
    ProviderConfig, ProviderKind, SecureRandom, SettingsStore, ThemeSettingsStore,
};
use application::public_site::PublicSiteInteractor;
use application::settings::SettingsInteractor;
use application::site_info::SiteInfo;
use application::tasks::{
    TaskAdmin, TaskKind, TaskListQuery, TaskReport, TaskRun, TaskRunPage, TaskSchedule,
    TaskScheduleInput, TaskStartInput, TaskStatus, TaskTrigger, TaskView, TasksInteractor,
};
use application::themes::ThemeRegistry;
use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::middleware;
use http_body_util::BodyExt;
use infrastructure::{
    InMemoryOAuthAttemptStore, InMemorySessionStore, MiniJinjaThemeRenderer,
    PostgresCategoryRepository, PostgresOAuthAccountStore, PostgresOAuthConfigStore,
    PostgresPageRepository, PostgresPostRepository, PostgresPublishedCategoryQuery,
    PostgresPublishedPageQuery, PostgresPublishedPostQuery, PostgresPublishedSeriesQuery,
    PostgresPublishedTagQuery, PostgresRbacStore, PostgresSeriesRepository, PostgresSettingsStore,
    PostgresTagRepository, PostgresUserRepository, RenderingRuntime, SystemClock,
};
use interfaces::http::{PublicSiteState, mount_theme_assets, public_router};
use interfaces::http_admin::{pages_router, settings_router};
use interfaces::http_auth::{AdminState, AuthState, admin_router, auth_router};
use interfaces::http_support::request_context;
use serde_json::json;
use sqlx::PgPool;
use tower::ServiceExt;
use uuid::Uuid;

static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// 回退装配值：模拟内置默认值（数据库未配置时公开页面使用它）。
const FALLBACK_TITLE: &str = "默认站点";
const FALLBACK_DESCRIPTION: &str = "回退描述";

struct FakeIdpClient {
    external_id: Mutex<String>,
}

#[async_trait]
impl ExternalIdentityClient for FakeIdpClient {
    async fn authorize_url(
        &self,
        _config: &ProviderConfig,
        state: &str,
        _challenge: Option<&str>,
        _nonce: Option<&str>,
        _redirect_uri: &str,
    ) -> Result<String, application::error::UseCaseError> {
        Ok(format!("https://idp.example/authorize?state={state}"))
    }

    async fn exchange(
        &self,
        _config: &ProviderConfig,
        _code: &str,
        _verifier: Option<&str>,
        _nonce: Option<&str>,
        _redirect_uri: &str,
    ) -> Result<ExternalIdentity, application::error::UseCaseError> {
        Ok(ExternalIdentity {
            provider_key: "https://idp.example".into(),
            provider_user_id: self.external_id.lock().unwrap().clone(),
            email: None,
        })
    }
}

struct TestRandom;

impl SecureRandom for TestRandom {
    fn token_hex(&self) -> Result<String, application::error::UseCaseError> {
        Ok(Uuid::now_v7().simple().to_string())
    }
    fn pkce_s256(&self, _verifier: &str) -> Result<String, application::error::UseCaseError> {
        Ok("challenge".into())
    }
}

#[derive(Default)]
struct FakeHtmlRebuildState {
    views: usize,
    starts: Vec<application::audit::AuditContext>,
    job: Option<HtmlRebuildJob>,
}

#[derive(Default)]
struct FakeHtmlRebuildJobs(Mutex<FakeHtmlRebuildState>);

#[async_trait]
impl HtmlRebuildJobs for FakeHtmlRebuildJobs {
    async fn view(&self) -> Result<HtmlRebuildView, application::UseCaseError> {
        let mut state = self.0.lock().unwrap();
        state.views += 1;
        Ok(HtmlRebuildView {
            pending: state.job.is_none().then_some(RebuildCounts {
                posts: 3,
                pages: 2,
                comments: 1,
            }),
            job: state.job.clone(),
            available: true,
        })
    }

    async fn start(
        &self,
        audit: application::audit::AuditContext,
    ) -> Result<HtmlRebuildJob, application::UseCaseError> {
        let mut state = self.0.lock().unwrap();
        state.starts.push(audit);
        let job = HtmlRebuildJob {
            id: Uuid::from_u128(42),
            status: HtmlRebuildJobStatus::Running,
            report: RebuildReport {
                rebuilt: RebuildCounts {
                    posts: 2,
                    ..Default::default()
                },
                skipped: RebuildCounts {
                    pages: 1,
                    ..Default::default()
                },
                batches: 1,
                has_more: true,
                ..Default::default()
            },
        };
        state.job = Some(job.clone());
        Ok(job)
    }
}

#[derive(Debug, PartialEq, Eq)]
enum TaskCall {
    View(Option<TaskKind>, Option<String>, Option<u32>),
    Enqueue(TaskKind, Option<String>, application::audit::AuditContext),
    Retry(Uuid, application::audit::AuditContext),
    Cancel(Uuid, application::audit::AuditContext),
    Schedule(i64, application::audit::AuditContext),
}
#[derive(Default)]
struct FakeTaskState {
    calls: Vec<TaskCall>,
    latest: Option<TaskRun>,
    schedule_version: i64,
}
#[derive(Default)]
struct FakeTaskAdmin(Mutex<FakeTaskState>);

fn fake_task(
    id: Uuid,
    kind: TaskKind,
    run_at: time::OffsetDateTime,
    trigger: TaskTrigger,
) -> TaskRun {
    TaskRun {
        id,
        kind,
        status: TaskStatus::Queued,
        trigger,
        run_at,
        created_at: time::OffsetDateTime::now_utc(),
        started_at: None,
        finished_at: None,
        retry_of: None,
        report: TaskReport::default(),
        can_retry: false,
        can_cancel: true,
    }
}
#[async_trait]
impl TaskAdmin for FakeTaskAdmin {
    async fn view(&self, query: TaskListQuery) -> Result<TaskView, application::UseCaseError> {
        let mut state = self.0.lock().unwrap();
        state
            .calls
            .push(TaskCall::View(query.kind, query.cursor, query.limit));
        let latest = state.latest.clone().into_iter().collect::<Vec<_>>();
        Ok(TaskView {
            available: true,
            retention_available: true,
            pending_html: Some(RebuildCounts {
                posts: 3,
                pages: 2,
                comments: 1,
            }),
            schedules: vec![TaskSchedule {
                kind: TaskKind::Retention,
                enabled: false,
                interval_seconds: 86400,
                next_run_at: None,
                version: state.schedule_version,
            }],
            latest: latest.clone(),
            runs: TaskRunPage {
                items: latest,
                next_cursor: None,
            },
        })
    }
    async fn enqueue(
        &self,
        input: TaskStartInput,
        audit: application::audit::AuditContext,
    ) -> Result<TaskRun, application::UseCaseError> {
        let mut state = self.0.lock().unwrap();
        let run_at = input.resolve_run_at(time::OffsetDateTime::now_utc())?;
        let trigger = if input.run_at.is_some() {
            TaskTrigger::Once
        } else {
            TaskTrigger::Manual
        };
        state
            .calls
            .push(TaskCall::Enqueue(input.kind, input.run_at, audit));
        let run = fake_task(
            Uuid::from_u128(100 + state.calls.len() as u128),
            input.kind,
            run_at,
            trigger,
        );
        state.latest = Some(run.clone());
        Ok(run)
    }
    async fn retry(
        &self,
        id: Uuid,
        audit: application::audit::AuditContext,
    ) -> Result<TaskRun, application::UseCaseError> {
        let mut state = self.0.lock().unwrap();
        state.calls.push(TaskCall::Retry(id, audit));
        if id.is_nil() {
            return Err(application::UseCaseError::Conflict(
                application::error::ConflictKind::Unknown,
            ));
        }
        let mut run = fake_task(
            Uuid::from_u128(200),
            TaskKind::HtmlRebuild,
            time::OffsetDateTime::now_utc(),
            TaskTrigger::Retry,
        );
        run.retry_of = Some(id);
        state.latest = Some(run.clone());
        Ok(run)
    }
    async fn cancel(
        &self,
        id: Uuid,
        audit: application::audit::AuditContext,
    ) -> Result<TaskRun, application::UseCaseError> {
        let mut state = self.0.lock().unwrap();
        state.calls.push(TaskCall::Cancel(id, audit));
        if id.is_nil() {
            return Err(application::UseCaseError::Conflict(
                application::error::ConflictKind::Unknown,
            ));
        }
        let mut run = fake_task(
            id,
            TaskKind::HtmlRebuild,
            time::OffsetDateTime::now_utc(),
            TaskTrigger::Once,
        );
        run.status = TaskStatus::Cancelled;
        run.can_cancel = false;
        run.finished_at = Some(time::OffsetDateTime::now_utc());
        state.latest = Some(run.clone());
        Ok(run)
    }
    async fn save_retention_schedule(
        &self,
        input: TaskScheduleInput,
        audit: application::audit::AuditContext,
    ) -> Result<TaskSchedule, application::UseCaseError> {
        let mut state = self.0.lock().unwrap();
        state.calls.push(TaskCall::Schedule(input.version, audit));
        if state.schedule_version != input.version {
            return Err(application::UseCaseError::VersionConflict);
        }
        state.schedule_version += 1;
        Ok(TaskSchedule {
            kind: TaskKind::Retention,
            enabled: input.enabled,
            interval_seconds: input.interval_seconds,
            next_run_at: input.resolve_next_run_at(time::OffsetDateTime::now_utc())?,
            version: state.schedule_version,
        })
    }
}

fn site_fallback() -> SiteInfo {
    SiteInfo {
        home_page_size: application::site_info::DEFAULT_HOME_PAGE_SIZE,
        navigation: vec![],
        time_zone: "UTC".into(),
        title: FALLBACK_TITLE.into(),
        description: FALLBACK_DESCRIPTION.into(),
        logo_url: None,
    }
}

/// 测试用可信站点地址：SEO/canonical/feed 链接都基于它拼接。
fn test_base_url() -> application::seo::PublicBaseUrl {
    application::seo::PublicBaseUrl::parse("https://blog.test").unwrap()
}

/// 与生产装配同构的测试栈；返回完整 router、登录依赖与连接池。
struct Stack {
    router: axum::Router,
    idp: Arc<FakeIdpClient>,
    pool: PgPool,
    media_dir: PathBuf,
    html_rebuild: Arc<FakeHtmlRebuildJobs>,
    tasks: Arc<FakeTaskAdmin>,
}

async fn build(pool: PgPool) -> Stack {
    let clock: Arc<dyn Clock> = Arc::new(SystemClock);
    let rendering = Arc::new(RenderingRuntime::default());
    let user_repo = Arc::new(PostgresUserRepository::new(common::database(pool.clone())));
    let rbac = Arc::new(PostgresRbacStore::new(common::database(pool.clone())));
    let roles = Arc::new(RoleInteractor::new(rbac.clone(), user_repo.clone()));
    roles.sync_registry().await.expect("同步权限目录失败");
    sqlx::query("INSERT INTO roles(id,code,name) VALUES($1,'account-manager','Account manager')")
        .bind(Uuid::now_v7())
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO role_permissions(role_id,permission_code) SELECT id,unnest(ARRAY['user.manage','role.manage','settings.manage']) FROM roles WHERE code='account-manager'")
        .execute(&pool).await.unwrap();
    let users = Arc::new(UserInteractor::new(
        application::identity::UserStores {
            query: user_repo.clone(),
            profiles: user_repo.clone(),
            accounts: user_repo.clone(),
        },
        rbac,
        clock.clone(),
        common::media_guard(pool.clone()),
    ));

    // admin 持有 settings.manage；editor/author 不持有（内容权限集）。
    let mut ids = std::collections::HashMap::new();
    for (username, role) in [
        ("admin", Some("account-manager")),
        ("owner", Some("admin")),
        ("editor", Some("editor")),
        ("author", Some("author")),
    ] {
        let user = users
            .create_user(
                &Actor::bootstrap_cli(),
                CreateUserCmd {
                    username: username.into(),
                    email: None,
                    display_name: Some(username.into()),
                },
            )
            .await
            .unwrap();
        if let Some(role) = role {
            roles
                .assign_to_username(&Actor::bootstrap_cli(), username, role)
                .await
                .unwrap();
        }
        ids.insert(username.to_string(), user.id);
    }

    let configs = Arc::new(PostgresOAuthConfigStore::new(common::database(
        pool.clone(),
    )));
    configs
        .save(
            &[ProviderConfig {
                id: "idp".into(),
                name: Some("示例 IdP".into()),
                kind: ProviderKind::Oidc,
                issuer: Some("https://idp.example".into()),
                client_id: "client".into(),
                secret_ref: "IDP_SECRET".into(),
                scopes: vec![],
            }],
            0,
            None.into(),
        )
        .await
        .unwrap();
    let accounts: Arc<dyn OAuthAccountStore> = Arc::new(PostgresOAuthAccountStore::new(
        common::database(pool.clone()),
    ));
    for (username, uid) in &ids {
        accounts
            .bind(
                *uid,
                "https://idp.example",
                &format!("sub-{username}"),
                None,
                None.into(),
            )
            .await
            .unwrap();
    }

    let idp = Arc::new(FakeIdpClient {
        external_id: Mutex::new("sub-admin".into()),
    });
    let sessions: Arc<dyn application::ports::SessionStore> =
        Arc::new(InMemorySessionStore::with_defaults());
    let auth = Arc::new(AuthInteractor::new(
        AuthDeps {
            sessions: sessions.clone(),
            attempts: Arc::new(InMemoryOAuthAttemptStore::with_defaults()),
            configs,
            accounts: accounts.clone(),
            identity_client: idp.clone(),
            random: Arc::new(TestRandom),
        },
        users.clone(),
        clock.clone(),
        "http://127.0.0.1:18099".into(),
    ));
    let passwords = common::password_interactor(user_repo.clone(), sessions);

    let tag_repo = Arc::new(PostgresTagRepository::new(common::database(pool.clone())));
    let category_repo = Arc::new(PostgresCategoryRepository::new(common::database(
        pool.clone(),
    )));
    let series_repo = Arc::new(PostgresSeriesRepository::new(common::database(
        pool.clone(),
    )));
    let posts = Arc::new(PostInteractor::new(
        Arc::new(PostgresPostRepository::new(
            common::database(pool.clone()),
            rendering.clone(),
        )),
        tag_repo.clone(),
        category_repo.clone(),
        series_repo.clone(),
        clock.clone(),
        common::media_guard(pool.clone()),
    ));
    let pages = Arc::new(PageInteractor::new(
        Arc::new(PostgresPageRepository::new(
            common::database(pool.clone()),
            rendering.clone(),
        )),
        clock.clone(),
    ));
    let tags = Arc::new(application::tag::TagInteractor::new(
        tag_repo,
        clock.clone(),
    ));
    let categories = Arc::new(application::category::CategoryInteractor::new(
        category_repo,
        clock.clone(),
    ));
    let series = Arc::new(application::series::SeriesInteractor::new(
        series_repo,
        clock.clone(),
        common::media_guard(pool.clone()),
    ));

    let settings_store: Arc<dyn SettingsStore> =
        Arc::new(PostgresSettingsStore::new(common::database(pool.clone())));
    let theme_store: Arc<dyn ThemeSettingsStore> =
        Arc::new(PostgresSettingsStore::new(common::database(pool.clone())));
    let theme_data = Arc::new(application::theme_data::ThemeData::new(
        Arc::new(PostgresPublishedPostQuery::new(common::database(
            pool.clone(),
        ))),
        Arc::new(PostgresPublishedTagQuery::new(common::database(
            pool.clone(),
        ))),
        Arc::new(PostgresPublishedCategoryQuery::new(common::database(
            pool.clone(),
        ))),
    ));
    let default_theme = MiniJinjaThemeRenderer::load(std::path::Path::new("../../themes/default"))
        .expect("默认主题加载失败")
        .with_data(theme_data.clone());
    let paper_theme = MiniJinjaThemeRenderer::load(std::path::Path::new("../../themes/paper"))
        .expect("Paper 主题加载失败")
        .with_data(theme_data);
    let theme_assets = vec![default_theme.assets(), paper_theme.assets()];
    let mut registry = ThemeRegistry::new("default".into());
    registry
        .add(
            "default".into(),
            "Default".into(),
            rendering.theme_renderer(default_theme),
        )
        .unwrap();
    registry
        .add(
            "paper".into(),
            "Paper".into(),
            rendering.theme_renderer(paper_theme),
        )
        .unwrap();
    let registry = Arc::new(registry);
    let settings = Arc::new(
        SettingsInteractor::new(
            settings_store.clone(),
            clock,
            site_fallback(),
            common::media_guard(pool.clone()),
        )
        .with_themes(theme_store.clone(), registry.clone())
        .with_time_zones(Arc::new(infrastructure::IanaTimeZones)),
    );

    // 公开站点：真实主题 + settings 解析（数据库 site 行 > 装配回退值）。
    let theme = registry.renderer("default").unwrap();
    let public_site = Arc::new(
        PublicSiteInteractor::new(
            Arc::new(PostgresPublishedPostQuery::new(common::database(
                pool.clone(),
            ))),
            Arc::new(PostgresPublishedPageQuery::new(common::database(
                pool.clone(),
            ))),
            Arc::new(PostgresPublishedTagQuery::new(common::database(
                pool.clone(),
            ))),
            Arc::new(PostgresPublishedCategoryQuery::new(common::database(
                pool.clone(),
            ))),
            Arc::new(PostgresPublishedSeriesQuery::new(common::database(
                pool.clone(),
            ))),
            theme,
            settings_store,
            site_fallback(),
            test_base_url(),
        )
        .with_themes(theme_store, registry)
        .with_time_zones(Arc::new(infrastructure::IanaTimeZones)),
    );

    let auth_state = AuthState {
        registration: common::registration(&pool),
        admission: Arc::new(infrastructure::InMemoryRequestAdmission::default()),
        auth: auth.clone(),
        passwords: passwords.clone(),
        secure_cookies: false,
    };
    let media_dir = common::media_dir("settings");
    let admin_state = AdminState {
        content_queries: common::content_queries(&pool),
        auth,
        users,
        passwords,
        posts,
        pages,
        tags,
        categories,
        series,
        settings,
        roles,
        media: common::media_interactor(pool.clone(), media_dir.clone()),
        secure_cookies: false,
    };
    let html_rebuild = Arc::new(FakeHtmlRebuildJobs::default());
    let tasks = Arc::new(FakeTaskAdmin::default());

    let router = mount_theme_assets(
        public_router(PublicSiteState {
            site: public_site,
            health: None,
        }),
        theme_assets,
    )
    .merge(auth_router(auth_state))
    .merge(admin_router(admin_state.clone()))
    .merge(pages_router(admin_state.clone()))
    .merge(settings_router(admin_state.clone()))
    .merge(interfaces::http_tasks::tasks_router(
        interfaces::http_tasks::TasksState {
            tasks: Arc::new(TasksInteractor::new(tasks.clone(), Arc::new(SystemClock))),
            admin: admin_state.clone(),
        },
    ))
    .merge(interfaces::http_html_rebuild::html_rebuild_router(
        interfaces::http_html_rebuild::HtmlRebuildState {
            rebuild: Arc::new(HtmlRebuildAdminInteractor::new(html_rebuild.clone())),
            admin: admin_state.clone(),
        },
    ))
    .merge(interfaces::http_audit::audit_router(
        interfaces::http_audit::AuditState {
            audit: Arc::new(application::audit::AuditInteractor::new(Arc::new(
                infrastructure::audit::PostgresAuditQuery::new(common::database(pool.clone())),
            ))),
            admin: admin_state.clone(),
        },
    ))
    .merge(interfaces::http_retention::retention_router(
        interfaces::http_retention::RetentionState {
            retention: Arc::new(application::retention::RetentionInteractor::new(Arc::new(
                infrastructure::retention::PostgresRetentionStore::new(common::database(
                    pool.clone(),
                )),
            ))),
            admin: admin_state,
        },
    ))
    .layer(middleware::from_fn(request_context));
    Stack {
        router,
        idp,
        pool,
        media_dir,
        html_rebuild,
        tasks,
    }
}

async fn fresh_stack() -> Stack {
    build(common::fresh_database("blog_settings_test").await).await
}

// These new transport regressions reuse the normal authenticated fixture while
// owning random databases, so an independent workspace run cannot reset them.
async fn isolated_html_rebuild<F, R>(scenario: F)
where
    F: FnOnce(Stack) -> R + Send + 'static,
    R: Future<Output = ()> + Send + 'static,
{
    let name = format!("blog_html_http_{}", Uuid::now_v7().simple());
    let stack = build(common::fresh_database(&name).await).await;
    let pool = stack.pool.clone();
    let media_dir = stack.media_dir.clone();
    let result = tokio::spawn(scenario(stack)).await;
    pool.close().await;
    let admin = common::connect(&common::admin_url()).await.unwrap();
    sqlx::raw_sql(&format!("DROP DATABASE {name} WITH (FORCE)"))
        .execute(&admin)
        .await
        .unwrap();
    admin.close().await;
    std::fs::remove_dir_all(media_dir).unwrap();
    if let Err(error) = result {
        std::panic::resume_unwind(error.into_panic());
    }
}

const HTML_REBUILD_PATH: &str = "/api/admin/v1/maintenance/html-rebuild";

async fn html_rebuild_exchange(
    router: &axum::Router,
    request: Request<Body>,
) -> (StatusCode, serde_json::Value) {
    let response = router.clone().oneshot(request).await.unwrap();
    assert_eq!(
        response
            .headers()
            .get(axum::http::header::CACHE_CONTROL)
            .unwrap(),
        "no-store",
        "成功和提前拒绝的维护响应都不得缓存"
    );
    assert!(response.headers().contains_key("x-request-id"));
    let status = response.status();
    let body = response.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&body).unwrap())
}

#[tokio::test]
async fn html_rebuild_auth_permission_origin_and_csrf_precede_job_admission() {
    isolated_html_rebuild(|stack| async move {
        let (editor, editor_csrf) = login_as(&stack, "editor").await;
        let (admin, csrf) = login_as(&stack, "admin").await;
        for (method, cookie, token, origin, expected, code) in [
            (
                "GET",
                None,
                None,
                "https://blog.test",
                StatusCode::UNAUTHORIZED,
                "unauthenticated",
            ),
            (
                "POST",
                None,
                None,
                "https://blog.test",
                StatusCode::UNAUTHORIZED,
                "unauthenticated",
            ),
            (
                "GET",
                Some(editor.as_str()),
                None,
                "https://blog.test",
                StatusCode::FORBIDDEN,
                "forbidden",
            ),
            (
                "POST",
                Some(editor.as_str()),
                Some(editor_csrf.as_str()),
                "https://blog.test",
                StatusCode::FORBIDDEN,
                "forbidden",
            ),
            (
                "POST",
                Some(admin.as_str()),
                None,
                "https://blog.test",
                StatusCode::BAD_REQUEST,
                "invalid_request",
            ),
            (
                "POST",
                Some(admin.as_str()),
                Some("wrong-csrf"),
                "https://blog.test",
                StatusCode::BAD_REQUEST,
                "invalid_request",
            ),
            (
                "POST",
                Some(admin.as_str()),
                Some(csrf.as_str()),
                "https://evil.example",
                StatusCode::FORBIDDEN,
                "forbidden",
            ),
        ] {
            let mut request = Request::builder()
                .method(method)
                .uri(HTML_REBUILD_PATH)
                .header("host", "blog.test")
                .header("origin", origin);
            if let Some(cookie) = cookie {
                request = request.header("cookie", format!("blog_session={cookie}"));
            }
            if let Some(token) = token {
                request = request.header("x-csrf-token", token);
            }
            let (status, body) =
                html_rebuild_exchange(&stack.router, request.body(Body::empty()).unwrap()).await;
            assert_eq!(status, expected, "{method} {origin}: {body}");
            assert_eq!(body["code"], code);
            assert!(body["request_id"].is_string());
        }
        let state = stack.html_rebuild.0.lock().unwrap();
        assert_eq!(state.views, 0, "拒绝读取不能调用维护 port");
        assert!(state.starts.is_empty(), "拒绝写入不能启动任务");
    })
    .await;
}

#[tokio::test]
async fn html_rebuild_wire_view_and_admission_use_only_the_authenticated_actor_and_verified_ip() {
    isolated_html_rebuild(|stack| async move {
        // This delegated settings manager is not the protected Admin role holder.
        let (admin, csrf) = login_as(&stack, "admin").await;
        let actor_id: Uuid = sqlx::query_scalar("SELECT id FROM users WHERE username='admin'")
            .fetch_one(&stack.pool)
            .await
            .unwrap();
        let owner_id: Uuid = sqlx::query_scalar("SELECT id FROM users WHERE username='owner'")
            .fetch_one(&stack.pool)
            .await
            .unwrap();
        let router = stack.router.clone().layer(axum::Extension(
            interfaces::http_client_ip::TrustedProxies(vec!["127.0.0.1".parse().unwrap()]),
        ));
        let read = || {
            Request::builder()
                .uri(HTML_REBUILD_PATH)
                .header("cookie", format!("blog_session={admin}"))
                .body(Body::empty())
                .unwrap()
        };
        let (status, initial) = html_rebuild_exchange(&router, read()).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            initial,
            json!({"pending":{"posts":3,"pages":2,"comments":1},"job":null,"available":true})
        );

        for (peer, forwarded, expected_ip) in [
            (
                Some("127.0.0.1:42000"),
                "203.0.113.99, 2001:db8::17, 127.0.0.1",
                Some("2001:db8::17"),
            ),
            (
                Some("198.51.100.8:42000"),
                "203.0.113.99",
                Some("198.51.100.8"),
            ),
            (None, "203.0.113.99", None),
        ] {
            let mut request = Request::builder()
                .method("POST")
                .uri(HTML_REBUILD_PATH)
                .header("host", "blog.test")
                .header("origin", "https://blog.test")
                .header("cookie", format!("blog_session={admin}"))
                .header("x-csrf-token", &csrf)
                .header("x-forwarded-for", forwarded)
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"actor_id":owner_id,"ip_address":"203.0.113.99"}).to_string(),
                ))
                .unwrap();
            if let Some(peer) = peer {
                request.extensions_mut().insert(axum::extract::ConnectInfo(
                    peer.parse::<std::net::SocketAddr>().unwrap(),
                ));
            }
            let (status, admitted) = html_rebuild_exchange(&router, request).await;
            assert_eq!(status, StatusCode::ACCEPTED);
            assert_eq!(admitted["id"], Uuid::from_u128(42).to_string());
            assert_eq!(admitted["status"], "running");
            assert_eq!(
                admitted["report"],
                json!({
                    "rebuilt":{"posts":2,"pages":0,"comments":0},
                    "skipped":{"posts":0,"pages":1,"comments":0},
                    "pending":null,"batches":1,"has_more":true,"dry_run":false,"failure":null,
                })
            );
            let (status, view) = html_rebuild_exchange(&router, read()).await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(
                view,
                json!({"pending":null,"job":admitted,"available":true})
            );
            let audit = *stack.html_rebuild.0.lock().unwrap().starts.last().unwrap();
            assert_eq!(audit.actor_id, Some(actor_id));
            assert_eq!(
                audit.ip_address.map(|ip| ip.to_string()).as_deref(),
                expected_ip
            );
        }
        let state = stack.html_rebuild.0.lock().unwrap();
        assert_eq!(state.views, 4);
        assert_eq!(state.starts.len(), 3);
    })
    .await;
}

const TASKS_PATH: &str = "/api/admin/v1/tasks";

#[tokio::test]
async fn tasks_http_auth_permission_origin_csrf_and_validation_precede_admin_port() {
    isolated_html_rebuild(|stack| async move {
        let (editor, editor_csrf) = login_as(&stack, "editor").await;
        let (admin, csrf) = login_as(&stack, "admin").await;
        let id = Uuid::from_u128(42);
        let retry = format!("{TASKS_PATH}/{id}/retry");
        let cancel = format!("{TASKS_PATH}/{id}/cancel");
        let schedule = format!("{TASKS_PATH}/retention-schedule");
        for (method, path) in [
            ("GET", TASKS_PATH),
            ("POST", TASKS_PATH),
            ("POST", retry.as_str()),
            ("POST", cancel.as_str()),
            ("PUT", schedule.as_str()),
        ] {
            for (cookie, token, origin, expected) in [
                (None, None, "https://blog.test", StatusCode::UNAUTHORIZED),
                (
                    Some(editor.as_str()),
                    Some(editor_csrf.as_str()),
                    "https://blog.test",
                    StatusCode::FORBIDDEN,
                ),
            ] {
                let mut request = Request::builder()
                    .method(method)
                    .uri(path)
                    .header("host", "blog.test")
                    .header("origin", origin)
                    .header("content-type", "application/json");
                if let Some(cookie) = cookie {
                    request = request.header("cookie", format!("blog_session={cookie}"));
                }
                if let Some(token) = token {
                    request = request.header("x-csrf-token", token);
                }
                let (status, body) = html_rebuild_exchange(
                    &stack.router,
                    request.body(Body::from("{invalid")).unwrap(),
                )
                .await;
                assert_eq!(status, expected, "{method} {path}: {body}");
            }
            if method != "GET" {
                for (token, origin, expected) in [
                    (None, "https://blog.test", StatusCode::BAD_REQUEST),
                    (Some("wrong"), "https://blog.test", StatusCode::BAD_REQUEST),
                    (
                        Some(csrf.as_str()),
                        "https://evil.example",
                        StatusCode::FORBIDDEN,
                    ),
                ] {
                    let mut request = Request::builder()
                        .method(method)
                        .uri(path)
                        .header("host", "blog.test")
                        .header("origin", origin)
                        .header("cookie", format!("blog_session={admin}"))
                        .header("content-type", "application/json");
                    if let Some(token) = token {
                        request = request.header("x-csrf-token", token);
                    }
                    let (status, body) = html_rebuild_exchange(
                        &stack.router,
                        request.body(Body::from("{invalid")).unwrap(),
                    )
                    .await;
                    assert_eq!(status, expected, "{method} {path}: {body}");
                }
            }
        }
        for query in [
            "kind=untrusted",
            "limit=0",
            "limit=101",
            "limit=no",
            "cursor=broken",
            "extra=hidden",
        ] {
            let request = Request::builder()
                .uri(format!("{TASKS_PATH}?{query}"))
                .header("cookie", format!("blog_session={admin}"))
                .body(Body::empty())
                .unwrap();
            let (status, body) = html_rebuild_exchange(&stack.router, request).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{query}: {body}");
        }
        for action in ["retry", "cancel"] {
            for (cookie, token, expected) in [
                (&editor, &editor_csrf, StatusCode::FORBIDDEN),
                (&admin, &csrf, StatusCode::BAD_REQUEST),
            ] {
                let request = Request::builder()
                    .method("POST")
                    .uri(format!("{TASKS_PATH}/invalid-task-id/{action}"))
                    .header("host", "blog.test")
                    .header("origin", "https://blog.test")
                    .header("cookie", format!("blog_session={cookie}"))
                    .header("x-csrf-token", token)
                    .body(Body::empty())
                    .unwrap();
                let (status, body) = html_rebuild_exchange(&stack.router, request).await;
                assert_eq!(status, expected, "{action}: {body}");
                assert_eq!(
                    body["code"],
                    if expected == StatusCode::FORBIDDEN {
                        "forbidden"
                    } else {
                        "invalid_request"
                    }
                );
            }
        }
        for (method, path) in [("POST", TASKS_PATH), ("PUT", schedule.as_str())] {
            let response = stack
                .router
                .clone()
                .oneshot(
                    Request::builder()
                        .method(method)
                        .uri(path)
                        .header("host", "blog.test")
                        .header("origin", "https://blog.test")
                        .header("cookie", format!("blog_session={admin}"))
                        .header("x-csrf-token", &csrf)
                        .header("content-type", "application/json")
                        .body(Body::from("x".repeat(9 * 1024)))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
            assert_eq!(response.headers()["cache-control"], "no-store");
            assert!(response.headers().contains_key("x-request-id"));
        }
        for input in [
            json!({"kind":"shell"}),
            json!({"kind":"html_rebuild","actor_id":id}),
            json!({"kind":"html_rebuild","ip_address":"203.0.113.99"}),
            json!({"kind":"retention","run_at":"2099-01-01T00:00:00Z"}),
            json!({"kind":"html_rebuild","run_at":"2099-01-01T00:00:00Z"}),
            json!({"kind":"html_rebuild","run_at":"2020-01-01T00:00:00Z"}),
        ] {
            let request = Request::builder()
                .method("POST")
                .uri(TASKS_PATH)
                .header("host", "blog.test")
                .header("origin", "https://blog.test")
                .header("cookie", format!("blog_session={admin}"))
                .header("x-csrf-token", &csrf)
                .header("content-type", "application/json")
                .body(Body::from(input.to_string()))
                .unwrap();
            let (status, body) = html_rebuild_exchange(&stack.router, request).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{input}: {body}");
        }
        for input in [
            json!({"enabled":true,"interval_seconds":3599,"version":0}),
            json!({"enabled":true,"interval_seconds":3600,"version":-1}),
            json!({"enabled":true,"interval_seconds":3600,"version":0,"actor_id":id}),
        ] {
            let request = Request::builder()
                .method("PUT")
                .uri(&schedule)
                .header("host", "blog.test")
                .header("origin", "https://blog.test")
                .header("cookie", format!("blog_session={admin}"))
                .header("x-csrf-token", &csrf)
                .header("content-type", "application/json")
                .body(Body::from(input.to_string()))
                .unwrap();
            assert_eq!(
                html_rebuild_exchange(&stack.router, request).await.0,
                StatusCode::BAD_REQUEST
            );
        }
        assert!(
            stack.tasks.0.lock().unwrap().calls.is_empty(),
            "all rejections precede runtime reads or writes"
        );
    })
    .await;
}

#[tokio::test]
async fn tasks_http_wire_times_whitelist_and_trusted_audit_are_preserved() {
    isolated_html_rebuild(|stack| async move {
        let (admin, csrf) = login_as(&stack, "admin").await;
        let actor: Uuid = sqlx::query_scalar("SELECT id FROM users WHERE username='admin'")
            .fetch_one(&stack.pool)
            .await
            .unwrap();
        let router = stack.router.clone().layer(axum::Extension(
            interfaces::http_client_ip::TrustedProxies(vec!["127.0.0.1".parse().unwrap()]),
        ));
        let due = (time::OffsetDateTime::now_utc() + time::Duration::hours(2))
            .to_offset(time::UtcOffset::from_hms(8, 0, 0).unwrap());
        let input_due = due
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap();
        let expected_due = due
            .to_offset(time::UtcOffset::UTC)
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap();
        let request = |method: &str, path: &str, input: serde_json::Value| {
            let mut request = Request::builder()
                .method(method)
                .uri(path)
                .header("host", "blog.test")
                .header("origin", "https://blog.test")
                .header("cookie", format!("blog_session={admin}"))
                .header("x-csrf-token", &csrf)
                .header("content-type", "application/json")
                .header("x-forwarded-for", "203.0.113.99, 2001:db8::17, 127.0.0.1")
                .body(Body::from(input.to_string()))
                .unwrap();
            request.extensions_mut().insert(axum::extract::ConnectInfo(
                "127.0.0.1:42000".parse::<std::net::SocketAddr>().unwrap(),
            ));
            request
        };
        let (status, initial) =
            html_rebuild_exchange(&router, request("GET", TASKS_PATH, json!(null))).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(initial["latest"], json!([]));
        assert_eq!(
            initial["pending_html"],
            json!({"posts":3,"pages":2,"comments":1})
        );
        let (status, planned) = html_rebuild_exchange(
            &router,
            request(
                "POST",
                TASKS_PATH,
                json!({"kind":"html_rebuild","run_at":input_due}),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::ACCEPTED);
        assert_eq!(planned["run_at"], expected_due);
        assert_eq!(planned["trigger"], "once");
        assert_eq!(planned["status"], "queued");
        let id = planned["id"].as_str().unwrap();
        let (status, view) = html_rebuild_exchange(
            &router,
            request(
                "GET",
                "/api/admin/v1/tasks?kind=html_rebuild&limit=100",
                json!(null),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(view["latest"], json!([planned.clone()]));
        assert_eq!(view["runs"]["items"], json!([planned]));
        for kind in ["retention", "publish_due"] {
            let (status, run) =
                html_rebuild_exchange(&router, request("POST", TASKS_PATH, json!({"kind":kind})))
                    .await;
            assert_eq!(status, StatusCode::ACCEPTED);
            assert_eq!(run["kind"], kind);
            assert_eq!(run["trigger"], "manual");
            assert!(application::tasks::parse_time(run["run_at"].as_str().unwrap()).is_ok());
        }
        let (status, retry) = html_rebuild_exchange(
            &router,
            request("POST", &format!("{TASKS_PATH}/{id}/retry"), json!(null)),
        )
        .await;
        assert_eq!(status, StatusCode::ACCEPTED);
        assert_eq!(retry["retry_of"], id);
        assert_ne!(retry["id"], id);
        assert_eq!(retry["trigger"], "retry");
        let (status, cancelled) = html_rebuild_exchange(
            &router,
            request("POST", &format!("{TASKS_PATH}/{id}/cancel"), json!(null)),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(cancelled["status"], "cancelled");
        assert_eq!(cancelled["can_cancel"], false);
        let schedule_path = format!("{TASKS_PATH}/retention-schedule");
        let policy =
            json!({"enabled":true,"interval_seconds":3600,"next_run_at":input_due,"version":0});
        let (status, saved) =
            html_rebuild_exchange(&router, request("PUT", &schedule_path, policy.clone())).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(saved["version"], 1);
        assert_eq!(saved["next_run_at"], expected_due);
        let (status, conflict) =
            html_rebuild_exchange(&router, request("PUT", &schedule_path, policy)).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(conflict["code"], "version_conflict");
        for action in ["retry", "cancel"] {
            let (status, conflict) = html_rebuild_exchange(
                &router,
                request(
                    "POST",
                    &format!("{TASKS_PATH}/{}/{action}", Uuid::nil()),
                    json!(null),
                ),
            )
            .await;
            assert_eq!(status, StatusCode::CONFLICT);
            assert_eq!(conflict["code"], "conflict");
        }
        let state = stack.tasks.0.lock().unwrap();
        assert_eq!(state.calls[0], TaskCall::View(None, None, None));
        assert_eq!(
            state.calls[1],
            TaskCall::Enqueue(
                TaskKind::HtmlRebuild,
                Some(input_due),
                application::audit::AuditContext {
                    actor_id: Some(actor),
                    ip_address: Some("2001:db8::17".parse().unwrap())
                }
            )
        );
        assert_eq!(
            state.calls[2],
            TaskCall::View(Some(TaskKind::HtmlRebuild), None, Some(100))
        );
        for call in &state.calls {
            let audit = match call {
                TaskCall::View(..) => continue,
                TaskCall::Enqueue(_, _, audit)
                | TaskCall::Retry(_, audit)
                | TaskCall::Cancel(_, audit)
                | TaskCall::Schedule(_, audit) => audit,
            };
            assert_eq!(audit.actor_id, Some(actor));
            assert_eq!(audit.ip_address, Some("2001:db8::17".parse().unwrap()));
        }
    })
    .await;
}

#[tokio::test]
async fn audit_history_has_its_own_permission_and_no_write_endpoint() {
    let _guard = SERIAL.lock().await;
    let stack = fresh_stack().await;
    let path = "/api/admin/v1/audit-logs";
    let (status, _, cache) = get(&stack.router, path, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(cache.as_deref(), Some("no-store"));
    let (admin, _) = login_as(&stack, "admin").await;
    assert_eq!(
        get(&stack.router, path, Some(&admin)).await.0,
        StatusCode::FORBIDDEN
    );
    let (owner, csrf) = login_as(&stack, "owner").await;
    let (status, body, cache) = get(&stack.router, &format!("{path}?limit=1"), Some(&owner)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(cache.as_deref(), Some("no-store"));
    assert_eq!(body["items"].as_array().unwrap().len(), 1);
    assert!(body["next_cursor"].is_string());
    for query in [
        "limit=0",
        "limit=101",
        "cursor=bad",
        "actor_id=bad",
        "unrecognized=true",
        "from=2026-09-27",
    ] {
        let (status, body, _) = get(&stack.router, &format!("{path}?{query}"), Some(&owner)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{query}");
        assert_eq!(body["code"], "invalid_request");
    }
    assert_eq!(
        put(&stack.router, path, &owner, &csrf, json!({})).await.0,
        StatusCode::METHOD_NOT_ALLOWED
    );
}

#[tokio::test]
async fn request_ip_is_verified_and_stays_with_its_business_transaction() {
    let _guard = SERIAL.lock().await;
    let stack = fresh_stack().await;
    let (owner, csrf) = login_as(&stack, "owner").await;
    let router =
        stack
            .router
            .clone()
            .layer(axum::Extension(interfaces::http_client_ip::TrustedProxies(
                vec!["127.0.0.1".parse().unwrap()],
            )));
    // One trusted proxy chain, an untrusted socket with a forged prefix, an
    // invalid chain, and an unknown peer. Only verified addresses are persisted.
    for (version, peer, forwarded, expected) in [
        (
            0,
            Some("127.0.0.1:4000"),
            "203.0.113.99, 2001:db8::42, 127.0.0.1",
            Some("2001:db8::42"),
        ),
        (
            1,
            Some("198.51.100.8:4000"),
            "203.0.113.99",
            Some("198.51.100.8"),
        ),
        (2, Some("127.0.0.1:4000"), "invalid, 2001:db8::42", None),
        (3, None, "203.0.113.99", None),
    ] {
        let mut request = Request::builder()
            .method("PUT")
            .uri("/api/admin/v1/settings/site")
            .header("cookie", format!("blog_session={owner}"))
            .header("x-csrf-token", &csrf)
            .header("x-forwarded-for", forwarded)
            .header("content-type", "application/json")
            .body(Body::from(
                json!({"title":format!("Revision {version}"),"expected_version":version})
                    .to_string(),
            ))
            .unwrap();
        if let Some(peer) = peer {
            request.extensions_mut().insert(axum::extract::ConnectInfo(
                peer.parse::<std::net::SocketAddr>().unwrap(),
            ));
        }
        let response = router.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let (actor, ip): (Option<Uuid>, Option<String>) = sqlx::query_as("SELECT actor_id,host(ip_address) FROM audit_logs WHERE action='settings.site' ORDER BY created_at DESC,id DESC LIMIT 1")
            .fetch_one(&stack.pool).await.unwrap();
        let owner_id: Uuid = sqlx::query_scalar("SELECT id FROM users WHERE username='owner'")
            .fetch_one(&stack.pool)
            .await
            .unwrap();
        assert_eq!(actor, Some(owner_id));
        assert_eq!(ip.as_deref(), expected);
    }
    let (_, logs, _) = get(
        &router,
        "/api/admin/v1/audit-logs?action=settings.site",
        Some(&owner),
    )
    .await;
    assert_eq!(logs["items"].as_array().unwrap().len(), 4);
    assert_eq!(logs["items"][3]["ip_address"], "2001:db8::42");
    assert_eq!(logs["items"][3]["actor_display"], "owner");
}

#[tokio::test]
async fn retention_requires_permission_csrf_and_current_group_versions() {
    let _g = SERIAL.lock().await;
    let stack = fresh_stack().await;
    let path = "/api/admin/v1/settings/retention";
    assert_eq!(
        get(&stack.router, path, None).await.0,
        StatusCode::UNAUTHORIZED
    );
    let (editor, editor_csrf) = login_as(&stack, "editor").await;
    assert_eq!(
        get(&stack.router, path, Some(&editor)).await.0,
        StatusCode::FORBIDDEN
    );
    let (admin, csrf) = login_as(&stack, "admin").await;
    let (status, mut body, cache) = get(&stack.router, path, Some(&admin)).await;
    assert_eq!(status, StatusCode::OK);
    assert!(cache.unwrap().contains("no-store"));
    assert_eq!(body["comment_ip_days"], 180);
    assert_eq!(body["audit_version"], 0);
    body["comment_ip_days"] = serde_json::json!(60);
    assert_eq!(
        put(&stack.router, path, &editor, &editor_csrf, body.clone())
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        put(&stack.router, path, &admin, "invalid-csrf", body.clone())
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    let (status, saved) = put(&stack.router, path, &admin, &csrf, body.clone()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(saved["comment_version"], 1);
    assert_eq!(saved["audit_version"], 0);
    assert_eq!(
        put(&stack.router, path, &admin, &csrf, body).await.0,
        StatusCode::CONFLICT
    );
    let mut invalid = saved;
    invalid["audit_days"] = serde_json::json!(-1);
    assert_eq!(
        put(&stack.router, path, &admin, &csrf, invalid).await.0,
        StatusCode::BAD_REQUEST
    );
}

/// 只重建公开路由（新 interactor + 新 settings 存储，同一数据库）：
/// 模拟「重启后」的进程——进程内状态全丢，配置只剩数据库。
async fn revived_public_router(pool: &PgPool) -> axum::Router {
    let theme = RenderingRuntime::default().theme_renderer(
        MiniJinjaThemeRenderer::load(std::path::Path::new("../../themes/default"))
            .expect("模板加载失败"),
    );
    let public_site = Arc::new(PublicSiteInteractor::new(
        Arc::new(PostgresPublishedPostQuery::new(common::database(
            pool.clone(),
        ))),
        Arc::new(PostgresPublishedPageQuery::new(common::database(
            pool.clone(),
        ))),
        Arc::new(PostgresPublishedTagQuery::new(common::database(
            pool.clone(),
        ))),
        Arc::new(PostgresPublishedCategoryQuery::new(common::database(
            pool.clone(),
        ))),
        Arc::new(PostgresPublishedSeriesQuery::new(common::database(
            pool.clone(),
        ))),
        theme,
        Arc::new(PostgresSettingsStore::new(common::database(pool.clone()))),
        site_fallback(),
        test_base_url(),
    ));
    public_router(PublicSiteState {
        site: public_site,
        health: None,
    })
}

/// 以指定用户走完整 OAuth 登录，返回 (会话 cookie, CSRF token)。
async fn login_as(stack: &Stack, username: &str) -> (String, String) {
    *stack.idp.external_id.lock().unwrap() = format!("sub-{username}");
    let response = stack
        .router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/auth/login?provider=idp&next=/admin/")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let location = response
        .headers()
        .get(axum::http::header::LOCATION)
        .and_then(|v| v.to_str().ok())
        .unwrap()
        .to_string();
    let state = location
        .split("state=")
        .nth(1)
        .unwrap()
        .split('&')
        .next()
        .unwrap()
        .to_string();
    let binding = response
        .headers()
        .get_all(axum::http::header::SET_COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .find_map(|v| {
            v.split(';')
                .next()?
                .trim()
                .strip_prefix("blog_oauth_state=")
                .map(str::to_string)
        })
        .expect("登录必须下发浏览器绑定 cookie");

    let response = stack
        .router
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/auth/callback/idp?code=x&state={state}"))
                .header("cookie", format!("blog_oauth_state={binding}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let cookie = response
        .headers()
        .get_all(axum::http::header::SET_COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .find_map(|v| {
            v.split(';')
                .next()?
                .trim()
                .strip_prefix("blog_session=")
                .map(str::to_string)
        })
        .expect("会话 cookie");

    let response = stack
        .router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/admin/v1/me")
                .header("cookie", format!("blog_session={cookie}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body: serde_json::Value = serde_json::from_str(&String::from_utf8_lossy(
        &response.into_body().collect().await.unwrap().to_bytes(),
    ))
    .unwrap();
    (cookie, body["csrf_token"].as_str().unwrap().to_string())
}

async fn get(
    router: &axum::Router,
    uri: &str,
    cookie: Option<&str>,
) -> (StatusCode, serde_json::Value, Option<String>) {
    let mut builder = Request::builder().uri(uri);
    if let Some(cookie) = cookie {
        builder = builder.header("cookie", format!("blog_session={cookie}"));
    }
    let response = router
        .clone()
        .oneshot(builder.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let cache = response
        .headers()
        .get(axum::http::header::CACHE_CONTROL)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let body = String::from_utf8_lossy(&response.into_body().collect().await.unwrap().to_bytes())
        .to_string();
    let json = serde_json::from_str(&body).unwrap_or(serde_json::Value::Null);
    (status, json, cache)
}

async fn put(
    router: &axum::Router,
    uri: &str,
    cookie: &str,
    csrf: &str,
    body: serde_json::Value,
) -> (StatusCode, serde_json::Value) {
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri(uri)
                .header("cookie", format!("blog_session={cookie}"))
                .header("x-csrf-token", csrf)
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let text = String::from_utf8_lossy(&response.into_body().collect().await.unwrap().to_bytes())
        .to_string();
    (
        status,
        serde_json::from_str(&text).unwrap_or(serde_json::Value::Null),
    )
}

async fn public_home(router: &axum::Router) -> String {
    let response = router
        .clone()
        .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    String::from_utf8_lossy(&response.into_body().collect().await.unwrap().to_bytes()).to_string()
}

async fn page_write(
    router: &axum::Router,
    method: &str,
    uri: &str,
    cookie: &str,
    csrf: &str,
    body: serde_json::Value,
) -> (StatusCode, serde_json::Value) {
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(uri)
                .header("cookie", format!("blog_session={cookie}"))
                .header("x-csrf-token", csrf)
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null),
    )
}

async fn public_text(router: &axum::Router, uri: &str) -> (StatusCode, String) {
    let response = router
        .clone()
        .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8_lossy(&bytes).to_string())
}

// ---------------------------------------------------------------------------
// 认证与授权
// ---------------------------------------------------------------------------

#[tokio::test]
async fn unauthenticated_requests_are_rejected() {
    let _g = SERIAL.lock().await;
    let stack = fresh_stack().await;

    let (status, body, _) = get(&stack.router, "/api/admin/v1/settings/site", None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["code"], "unauthenticated");

    let (status, body) = put(
        &stack.router,
        "/api/admin/v1/settings/site",
        "not-a-session",
        "not-a-csrf",
        serde_json::json!({"title": "x", "description": "y"}),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["code"], "unauthenticated");
}

#[tokio::test]
async fn settings_manage_is_required_for_read_and_write() {
    let _g = SERIAL.lock().await;
    let stack = fresh_stack().await;
    // editor 有大量内容权限但没有 settings.manage。
    let (cookie, csrf) = login_as(&stack, "editor").await;

    let (status, body, _) = get(&stack.router, "/api/admin/v1/settings/site", Some(&cookie)).await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "读取需 settings.manage：{body}"
    );
    assert_eq!(body["code"], "forbidden");

    let (status, body) = put(
        &stack.router,
        "/api/admin/v1/settings/site",
        &cookie,
        &csrf,
        serde_json::json!({"title": "越权标题", "description": "描述"}),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["code"], "forbidden");

    // 越权 PUT 没有落库：公开页面仍是装配回退值。
    let html = public_home(&stack.router).await;
    assert!(html.contains(FALLBACK_TITLE) && !html.contains("越权标题"));
}

#[tokio::test]
async fn write_requires_csrf_token() {
    let _g = SERIAL.lock().await;
    let stack = fresh_stack().await;
    let (cookie, _csrf) = login_as(&stack, "admin").await;

    let (status, body) = put(
        &stack.router,
        "/api/admin/v1/settings/site",
        &cookie,
        "wrong-csrf-token",
        serde_json::json!({"title": "标题", "description": "描述"}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["code"], "invalid_request");
}

// ---------------------------------------------------------------------------
// 读写闭环与优先级
// ---------------------------------------------------------------------------

#[tokio::test]
async fn unconfigured_site_reads_fallback_and_public_uses_it() {
    let _g = SERIAL.lock().await;
    let stack = fresh_stack().await;
    let (cookie, _csrf) = login_as(&stack, "admin").await;

    let (status, body, cache) =
        get(&stack.router, "/api/admin/v1/settings/site", Some(&cookie)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["source"], "fallback");
    assert_eq!(body["version"], 0);
    assert_eq!(body["title"], FALLBACK_TITLE);
    assert_eq!(body["description"], FALLBACK_DESCRIPTION);
    assert_eq!(cache.as_deref(), Some("no-store"), "管理响应不得缓存");

    // 未配置时公开页面渲染装配回退值。
    let html = public_home(&stack.router).await;
    assert!(html.contains(FALLBACK_TITLE));
    assert!(html.contains(FALLBACK_DESCRIPTION));
}

#[tokio::test]
async fn site_time_zone_changes_live_and_preserves_absolute_timestamps() {
    let _g = SERIAL.lock().await;
    let stack = fresh_stack().await;
    let (cookie, csrf) = login_as(&stack, "admin").await;
    let (author_cookie, _) = login_as(&stack, "author").await;
    let (_, initial, _) = get(&stack.router, "/api/admin/v1/settings/site", Some(&cookie)).await;
    assert_eq!(initial["time_zone"], "UTC");
    assert!(
        initial["time_zones"]
            .as_array()
            .unwrap()
            .contains(&json!("Asia/Shanghai"))
    );
    let published = time::macros::datetime!(2020-09-28 17:30 UTC);
    let scheduled = time::macros::datetime!(2099-09-28 17:30 UTC);
    for (slug, status, at) in [
        ("zoned", "published", published),
        ("future-zoned", "scheduled", scheduled),
    ] {
        sqlx::query("INSERT INTO posts (id,author_id,slug,title,content,content_html,content_render_version,status,published_at,updated_at) SELECT $1,id,$2,'时区测试','正文','<p>正文</p>',1,$3,$4,$4 FROM users WHERE username='author'")
            .bind(Uuid::now_v7()).bind(slug).bind(status).bind(at)
            .execute(&stack.pool).await.unwrap();
    }
    for (version, zone, display) in [
        (
            0,
            "Asia/Shanghai",
            "2020-09-29 01:30 +08:00 (Asia/Shanghai)",
        ),
        (
            1,
            "America/New_York",
            "2020-09-28 13:30 -04:00 (America/New_York)",
        ),
    ] {
        let input = json!({"title":"时区测试","description":"","time_zone":zone,"expected_version":version});
        let (status, saved) = put(
            &stack.router,
            "/api/admin/v1/settings/site",
            &cookie,
            &csrf,
            input.clone(),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{saved}");
        assert_eq!(saved["time_zone"], zone);
        assert_eq!(saved["version"], version + 1);
        // Same router/session: an author can obtain the display zone without settings.manage.
        let (status, me, _) = get(&stack.router, "/api/admin/v1/me", Some(&author_cookie)).await;
        assert_eq!(status, StatusCode::OK, "{me}");
        assert_eq!(me["time_zone"], zone);
        for path in ["/", "/posts/zoned"] {
            let (status, html) = public_text(&stack.router, path).await;
            assert_eq!(status, StatusCode::OK, "{html}");
            assert!(html.contains(&display.replace('/', "&#x2f;")), "{html}");
        }
        let (status, _) = put(
            &stack.router,
            "/api/admin/v1/settings/site",
            &cookie,
            &csrf,
            input.clone(),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT);
        let mut same = input;
        same["expected_version"] = json!(version + 1);
        let (status, saved) = put(
            &stack.router,
            "/api/admin/v1/settings/site",
            &cookie,
            &csrf,
            same,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(saved["version"], version + 1, "相同值保存不增版");
    }
    // Older clients editing the title must not reset the saved zone.
    let (status, saved) = put(
        &stack.router,
        "/api/admin/v1/settings/site",
        &cookie,
        &csrf,
        json!({"title":"旧客户端修改","description":"","expected_version":2}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(saved["time_zone"], "America/New_York");
    for invalid in ["", "Asia/Unknown", "+08:00"] {
        let (status, _) = put(
            &stack.router,
            "/api/admin/v1/settings/site",
            &cookie,
            &csrf,
            json!({"title":"不能保存","description":"","time_zone":invalid,"expected_version":3}),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }
    let stored: Vec<(String, time::OffsetDateTime)> =
        sqlx::query_as("SELECT slug,published_at FROM posts ORDER BY slug")
            .fetch_all(&stack.pool)
            .await
            .unwrap();
    assert_eq!(
        stored,
        vec![
            ("future-zoned".into(), scheduled),
            ("zoned".into(), published)
        ]
    );
    let value: serde_json::Value =
        sqlx::query_scalar("SELECT value FROM settings WHERE key='site'")
            .fetch_one(&stack.pool)
            .await
            .unwrap();
    assert_eq!(value["time_zone"], "America/New_York");
    let audits: i64 =
        sqlx::query_scalar("SELECT count(*) FROM audit_logs WHERE action='settings.site'")
            .fetch_one(&stack.pool)
            .await
            .unwrap();
    assert_eq!(audits, 3);
}

#[tokio::test]
async fn saved_settings_take_effect_on_public_pages_immediately() {
    let _g = SERIAL.lock().await;
    let stack = fresh_stack().await;
    let (cookie, csrf) = login_as(&stack, "admin").await;

    let (status, body) = put(
        &stack.router,
        "/api/admin/v1/settings/site",
        &cookie,
        &csrf,
        serde_json::json!({
            "title": "  数据库站点标题  ",
            "description": "数据库站点描述",
            "expected_version": 0,
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["version"], 1);
    assert_eq!(body["source"], "database");
    assert_eq!(body["title"], "数据库站点标题", "响应为规范化后的值");

    let actor: Uuid = sqlx::query_scalar("SELECT id FROM users WHERE username='admin'")
        .fetch_one(&stack.pool)
        .await
        .unwrap();
    let audits: Vec<(Uuid, serde_json::Value)> = sqlx::query_as("SELECT actor_id,metadata FROM audit_logs WHERE action='settings.site' AND target_id='site'")
        .fetch_all(&stack.pool).await.unwrap();
    assert_eq!(audits, vec![(actor, serde_json::json!({"version":1}))]);
    // Repeated same-value saves and stale submissions must not invent audit events.
    for (version, expected) in [(1, StatusCode::OK), (0, StatusCode::CONFLICT)] {
        let (status, _) = put(&stack.router, "/api/admin/v1/settings/site", &cookie, &csrf,
            serde_json::json!({"title":"数据库站点标题","description":"数据库站点描述","expected_version":version})).await;
        assert_eq!(status, expected);
    }
    // Exercise the actual HTTP path: failed audit means no settings change.
    sqlx::raw_sql("CREATE FUNCTION fail_site_audit() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'audit unavailable'; END $$; CREATE TRIGGER fail_site_audit BEFORE INSERT ON audit_logs FOR EACH ROW WHEN (NEW.action='settings.site') EXECUTE FUNCTION fail_site_audit()")
        .execute(&stack.pool).await.unwrap();
    let (status, _) = put(
        &stack.router,
        "/api/admin/v1/settings/site",
        &cookie,
        &csrf,
        serde_json::json!({"title":"Rejected","description":"Rejected","expected_version":1}),
    )
    .await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    let version: i64 = sqlx::query_scalar("SELECT version FROM settings WHERE key='site'")
        .fetch_one(&stack.pool)
        .await
        .unwrap();
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM audit_logs WHERE action='settings.site'")
            .fetch_one(&stack.pool)
            .await
            .unwrap();
    assert_eq!((version, count), (1, 1));

    // 无缓存：同一次进程内下一次公开渲染即用数据库值。
    let html = public_home(&stack.router).await;
    assert!(html.contains("数据库站点标题"), "公开页面应使用数据库标题");
    assert!(html.contains("数据库站点描述"));
    assert!(!html.contains(FALLBACK_TITLE), "装配回退值不再生效");

    // 「重启」语义：丢弃全部进程内装配（新 interactor），只保留数据库——
    // 全新公开栈仍读到已保存配置。
    let revived = revived_public_router(&stack.pool).await;
    let html = public_home(&revived).await;
    assert!(html.contains("数据库站点标题"));
}

#[tokio::test]
async fn page_trash_hides_public_entries_and_only_purge_releases_slug() {
    let _g = SERIAL.lock().await;
    let stack = fresh_stack().await;
    let (editor_cookie, editor_csrf) = login_as(&stack, "editor").await;
    let (author_cookie, author_csrf) = login_as(&stack, "author").await;
    let (status, created) = page_write(
        &stack.router,
        "POST",
        "/api/admin/v1/pages",
        &editor_cookie,
        &editor_csrf,
        serde_json::json!({"slug":"about","title":"关于","content":"# 关于","visibility":"public"}),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let id = created["id"].as_str().unwrap();
    let path = &format!("/api/admin/v1/pages/{id}");
    let (status, published) = page_write(
        &stack.router,
        "POST",
        &format!("{path}/publish"),
        &editor_cookie,
        &editor_csrf,
        serde_json::json!({"expected_version":1}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{published}");
    assert_eq!(published["version"], 2);
    assert_eq!(public_text(&stack.router, "/about").await.0, StatusCode::OK);
    assert!(
        public_text(&stack.router, "/sitemap.xml")
            .await
            .1
            .contains("https://blog.test/about")
    );

    let body = serde_json::json!({"expected_version":2});
    let (status, _) = page_write(
        &stack.router,
        "POST",
        &format!("{path}/trash"),
        &author_cookie,
        &author_csrf,
        body.clone(),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = page_write(
        &stack.router,
        "POST",
        &format!("{path}/trash"),
        &editor_cookie,
        "bad-csrf",
        body.clone(),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    for stale in [serde_json::json!({"expected_version":1})] {
        let (status, error) = page_write(
            &stack.router,
            "POST",
            &format!("{path}/trash"),
            &editor_cookie,
            &editor_csrf,
            stale,
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "{error}");
        assert_eq!(error["code"], "version_conflict");
    }
    assert_eq!(public_text(&stack.router, "/about").await.0, StatusCode::OK);

    let (status, _) = page_write(
        &stack.router,
        "POST",
        &format!("{path}/trash"),
        &editor_cookie,
        &editor_csrf,
        body.clone(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        public_text(&stack.router, "/about").await.0,
        StatusCode::NOT_FOUND
    );
    assert!(
        !public_text(&stack.router, "/sitemap.xml")
            .await
            .1
            .contains("https://blog.test/about")
    );
    let (status, _, _) = get(&stack.router, path, Some(&editor_cookie)).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = page_write(
        &stack.router,
        "POST",
        &format!("{path}/trash"),
        &editor_cookie,
        &editor_csrf,
        body.clone(),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // A trashed page retains its address; only Owner may physically purge it.
    let (status, _) = page_write(
        &stack.router,
        "POST",
        "/api/admin/v1/pages",
        &editor_cookie,
        &editor_csrf,
        serde_json::json!({"slug":"about","title":"Reserved","content":"Body"}),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    let (status, _) = page_write(
        &stack.router,
        "POST",
        &format!("{path}/purge"),
        &editor_cookie,
        &editor_csrf,
        serde_json::json!({"expected_version":3}),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    sqlx::query("INSERT INTO user_roles(user_id,role_id) SELECT u.id,r.id FROM users u CROSS JOIN roles r WHERE u.username='admin' AND r.code='admin'")
        .execute(&stack.pool).await.unwrap();
    let (owner_cookie, owner_csrf) = login_as(&stack, "admin").await;
    for (version, expected) in [(2, StatusCode::CONFLICT), (3, StatusCode::NO_CONTENT)] {
        let (status, _) = page_write(
            &stack.router,
            "POST",
            &format!("{path}/purge"),
            &owner_cookie,
            &owner_csrf,
            serde_json::json!({"expected_version":version}),
        )
        .await;
        assert_eq!(status, expected);
    }

    let (status, replacement) = page_write(
        &stack.router,
        "POST",
        "/api/admin/v1/pages",
        &editor_cookie,
        &editor_csrf,
        serde_json::json!({"slug":"about","title":"新页面","content":"new"}),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{replacement}");
    assert_ne!(replacement["id"], id);
    let (status, stale) = page_write(
        &stack.router,
        "POST",
        &format!("{path}/trash"),
        &editor_cookie,
        &editor_csrf,
        serde_json::json!({"expected_version":1}),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{stale}");
    assert_eq!(
        get(
            &stack.router,
            &format!(
                "/api/admin/v1/pages/{}",
                replacement["id"].as_str().unwrap()
            ),
            Some(&editor_cookie)
        )
        .await
        .0,
        StatusCode::OK
    );
}

#[tokio::test]
async fn invalid_values_are_rejected_as_bad_request() {
    let _g = SERIAL.lock().await;
    let stack = fresh_stack().await;
    let (cookie, csrf) = login_as(&stack, "admin").await;

    let long_title = "长".repeat(201);
    let long_description = "述".repeat(501);
    for (title, description) in [
        ("", "描述"),
        ("   ", "描述"),
        (&long_title, "描述"),
        ("标题", &long_description),
    ] {
        let (status, body) = put(
            &stack.router,
            "/api/admin/v1/settings/site",
            &cookie,
            &csrf,
            serde_json::json!({"title": title, "description": description, "expected_version": 0}),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "({title:?}, ..) 应 400：{body}"
        );
        assert_eq!(body["code"], "invalid_request");
    }

    // 均未落库。
    let (_, body, _) = get(&stack.router, "/api/admin/v1/settings/site", Some(&cookie)).await;
    assert_eq!(body["version"], 0);
}

// ---------------------------------------------------------------------------
// 并发冲突与受保护分组
// ---------------------------------------------------------------------------

#[tokio::test]
async fn stale_expected_version_conflicts_then_recovers() {
    let _g = SERIAL.lock().await;
    let stack = fresh_stack().await;
    let (cookie, csrf) = login_as(&stack, "admin").await;

    let (status, _) = put(
        &stack.router,
        "/api/admin/v1/settings/site",
        &cookie,
        &csrf,
        serde_json::json!({"title": "第一版", "description": "描述", "expected_version": 0}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // 另一处已把版本推进到 1：旧前提 0 再提交必须 409，且不覆盖。
    let (status, body) = put(
        &stack.router,
        "/api/admin/v1/settings/site",
        &cookie,
        &csrf,
        serde_json::json!({"title": "过期提交", "description": "描述", "expected_version": 0}),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["code"], "version_conflict");

    let (_, body, _) = get(&stack.router, "/api/admin/v1/settings/site", Some(&cookie)).await;
    assert_eq!(body["title"], "第一版", "冲突提交不得覆盖");

    // 按最新版本重提即成功（前端「仍然覆盖」流程）。
    let version = body["version"].as_i64().unwrap();
    let (status, body) = put(
        &stack.router,
        "/api/admin/v1/settings/site",
        &cookie,
        &csrf,
        serde_json::json!({
            "title": "第二版", "description": "描述", "expected_version": version
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["version"], 2);
}

#[tokio::test]
async fn unknown_or_protected_settings_groups_are_not_routed() {
    let _g = SERIAL.lock().await;
    let stack = fresh_stack().await;
    let (cookie, csrf) = login_as(&stack, "admin").await;

    // oauth（受 oauth.manage 保护的分组）、未知分组与集合路径不可寻址。
    for uri in ["/api/admin/v1/settings/oauth", "/api/admin/v1/settings"] {
        let (status, _, _) = get(&stack.router, uri, Some(&cookie)).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "GET {uri}");
        let (status, _) = put(
            &stack.router,
            uri,
            &cookie,
            &csrf,
            serde_json::json!({"providers": []}),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "PUT {uri}");
    }
}

#[tokio::test]
async fn theme_switch_is_authorized_versioned_and_updates_html_and_assets() {
    let _g = SERIAL.lock().await;
    let stack = fresh_stack().await;
    let uri = "/api/admin/v1/settings/theme";
    let (status, _, _) = get(&stack.router, uri, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (editor_cookie, editor_csrf) = login_as(&stack, "editor").await;
    let (status, _, _) = get(&stack.router, uri, Some(&editor_cookie)).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = put(
        &stack.router,
        uri,
        &editor_cookie,
        &editor_csrf,
        serde_json::json!({"slug":"paper","expected_version":0}),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    let (cookie, csrf) = login_as(&stack, "admin").await;
    let (status, initial, cache) = get(&stack.router, uri, Some(&cookie)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(cache.as_deref(), Some("no-store"));
    assert_eq!(initial["slug"], "default");
    assert_eq!(initial["source"], "fallback");
    assert_eq!(initial["version"], 0);
    assert_eq!(initial["available"].as_array().unwrap().len(), 2);
    assert!(
        public_home(&stack.router)
            .await
            .contains("/assets/default/")
    );

    let (status, body) = put(
        &stack.router,
        uri,
        &cookie,
        "wrong-csrf",
        serde_json::json!({"slug":"paper","expected_version":0}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    let (status, body) = put(
        &stack.router,
        uri,
        &cookie,
        &csrf,
        serde_json::json!({"slug":"../paper","expected_version":0}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    let (status, saved) = put(
        &stack.router,
        uri,
        &cookie,
        &csrf,
        serde_json::json!({"slug":"paper","expected_version":0}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    assert_eq!(saved["version"], 1);
    assert_eq!(saved["source"], "database");
    let html = public_home(&stack.router).await;
    assert!(html.contains("/assets/paper/"));
    assert!(!html.contains("/assets/default/"));
    let asset_url = html
        .split("href=\"")
        .filter_map(|part| part.split('"').next())
        .find(|url| url.starts_with("/assets/paper/"))
        .unwrap();
    let (status, _, _) = get(&stack.router, asset_url, None).await;
    assert_eq!(status, StatusCode::OK);

    let (status, stale) = put(
        &stack.router,
        uri,
        &cookie,
        &csrf,
        serde_json::json!({"slug":"default","expected_version":0}),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{stale}");
    let (status, same) = put(
        &stack.router,
        uri,
        &cookie,
        &csrf,
        serde_json::json!({"slug":"paper","expected_version":1}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(same["version"], 1);
    let (status, restored) = put(
        &stack.router,
        uri,
        &cookie,
        &csrf,
        serde_json::json!({"slug":"default","expected_version":1}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{restored}");
    assert_eq!(restored["version"], 2);
    assert!(
        public_home(&stack.router)
            .await
            .contains("/assets/default/")
    );
}

#[tokio::test]
async fn missing_saved_theme_falls_back_and_can_be_replaced() {
    let _g = SERIAL.lock().await;
    let stack = fresh_stack().await;
    sqlx::query("INSERT INTO settings (key,value,version) VALUES ('theme', '{\"schema_version\":1,\"slug\":\"removed\"}'::jsonb, 1)")
        .execute(&stack.pool).await.unwrap();
    let (cookie, csrf) = login_as(&stack, "admin").await;
    let (_, view, _) = get(&stack.router, "/api/admin/v1/settings/theme", Some(&cookie)).await;
    assert_eq!(view["slug"], "removed");
    assert_eq!(view["effective_slug"], "default");
    assert!(
        public_home(&stack.router)
            .await
            .contains("/assets/default/")
    );
    let (status, saved) = put(
        &stack.router,
        "/api/admin/v1/settings/theme",
        &cookie,
        &csrf,
        serde_json::json!({"slug":"paper","expected_version":1}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    assert_eq!(saved["effective_slug"], "paper");
    assert!(public_home(&stack.router).await.contains("/assets/paper/"));
}

#[tokio::test]
async fn oversize_settings_body_is_rejected_before_parsing() {
    let _g = SERIAL.lock().await;
    let stack = fresh_stack().await;
    let (cookie, csrf) = login_as(&stack, "admin").await;

    let response = stack
        .router
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/api/admin/v1/settings/site")
                .header("cookie", format!("blog_session={cookie}"))
                .header("x-csrf-token", csrf)
                .header("content-type", "application/json")
                .body(Body::from("x".repeat(32 * 1024)))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
}

#[tokio::test]
async fn site_presentation_settings_are_saved_and_preserved_when_omitted() {
    let _g = SERIAL.lock().await;
    let stack = fresh_stack().await;
    let (cookie, csrf) = login_as(&stack, "admin").await;
    let presentation = json!({
        "home_page_size": 7,
        "navigation": [
            {"label":"关于","page_slug":"about","placement":"header"},
            {"label":"联系","page_slug":"contact","placement":"footer"}
        ]
    });
    let mut input = presentation.clone();
    input["title"] = json!("Site");
    input["description"] = json!("");
    input["expected_version"] = json!(0);
    let (status, saved) = put(
        &stack.router,
        "/api/admin/v1/settings/site",
        &cookie,
        &csrf,
        input,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    let (_, loaded, _) = get(&stack.router, "/api/admin/v1/settings/site", Some(&cookie)).await;
    for key in ["home_page_size", "navigation"] {
        assert_eq!(loaded[key], presentation[key]);
    }
    let (status, preserved) = put(
        &stack.router,
        "/api/admin/v1/settings/site",
        &cookie,
        &csrf,
        json!({"title":"Renamed","description":"","expected_version":loaded["version"]}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    for key in ["home_page_size", "navigation"] {
        assert_eq!(preserved[key], presentation[key]);
    }
    stack.pool.close().await;
}
