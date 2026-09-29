//! 用户与角色管理 API：本人资料、账号列表/创建/启停、角色列表与分配。
//!
//! 本层只做传输映射，**不重复实现权限判断**：授权边界（`user.manage` /
//! `role.manage` / `admin.manage`）、委派上限与最后 Admin 保护都由
//! `UserInteractor` / `RoleInteractor` 及 RBAC 存储在锁内执行。
//!
//! 统一的 `AdminAuth` 提取器负责会话 + CSRF/Origin；错误经 `admin_error`
//! 映射成 `{error, code, request_id}`。冲突码是界面分支的依据：
//! `username_taken` / `email_taken` 定位到创建表单字段，`last_admin` 解释
//! 为什么移除 Admin 角色被拒（与 `forbidden` 区分）。

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, put};
use axum::{Json, Router};
use serde::Deserialize;

use application::identity::CreateUserCmd;

use crate::http_admin::AdminAuth;
use crate::http_auth::AdminState;
use crate::http_support::{RequestId, admin_error, no_store};

/// 账号/角色请求体上限：字段都是短字符串，4 KiB 足够，
/// 同时把超长输入挡在规范化与写库之前。
pub const IDENTITY_BODY_LIMIT: usize = 4 * 1024;

#[derive(Deserialize, Default)]
pub struct UserListQuery {
    /// 缺省由用例回落到 `ADMIN_USER_PAGE_DEFAULT`；超过上限由用例收敛。
    pub limit: Option<i64>,
    pub offset: Option<i64>,
}

#[derive(Deserialize, ts_rs::TS)]
#[ts(rename = "CreateUserInput", optional_fields = nullable)]
pub struct CreateUserBody {
    pub username: String,
    #[serde(default)]
    pub email: Option<String>,
    #[serde(default)]
    pub display_name: Option<String>,
}

pub fn identity_router(state: AdminState) -> Router {
    Router::new()
        .route("/api/admin/v1/me/profile", put(update_profile))
        .route("/api/admin/v1/users", get(list_users).post(create_user))
        .route("/api/admin/v1/users/{user_id}/status", put(change_status))
        .route(
            "/api/admin/v1/users/{username}/roles/{role}",
            put(assign_role).delete(remove_role),
        )
        .route("/api/admin/v1/roles", get(list_roles))
        .layer(axum::extract::DefaultBodyLimit::max(IDENTITY_BODY_LIMIT))
        .layer(axum::middleware::from_fn(no_store))
        .with_state(state)
}

// ---------------------------------------------------------------------------
// 处理器
// ---------------------------------------------------------------------------

#[derive(Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
#[ts(rename = "UpdateProfileInput", optional_fields = nullable)]
struct UpdateProfileBody {
    display_name: Option<String>,
    bio: Option<String>,
    expected_version: i64,
}

async fn update_profile(
    AdminAuth { actor }: AdminAuth,
    request_id: RequestId,
    State(state): State<AdminState>,
    Json(body): Json<UpdateProfileBody>,
) -> Response {
    match state
        .users
        .update_own_profile(&actor, body.display_name, body.bio, body.expected_version)
        .await
    {
        Ok(profile) => Json(crate::http_contract::Profile::from(profile)).into_response(),
        Err(error) => admin_error(error, &request_id),
    }
}

async fn list_users(
    AdminAuth { actor }: AdminAuth,
    request_id: RequestId,
    State(state): State<AdminState>,
    Query(query): Query<UserListQuery>,
) -> Response {
    match state
        .users
        .list_users(&actor, query.limit.unwrap_or(0), query.offset.unwrap_or(0))
        .await
    {
        Ok(users) => (
            StatusCode::OK,
            Json(
                users
                    .into_iter()
                    .map(crate::http_contract::AdminUser::from)
                    .collect::<Vec<_>>(),
            ),
        )
            .into_response(),
        Err(e) => admin_error(e, &request_id),
    }
}

#[derive(Deserialize, ts_rs::TS)]
#[serde(rename_all = "lowercase")]
#[ts(rename = "AccountStatus")]
enum AccountStatus {
    Active,
    Disabled,
}

#[derive(Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
#[ts(rename = "ChangeStatusInput", optional_fields = nullable)]
struct ChangeStatusBody {
    status: AccountStatus,
    expected_version: i64,
}

async fn change_status(
    AdminAuth { actor }: AdminAuth,
    request_id: RequestId,
    State(state): State<AdminState>,
    Path(user_id): Path<uuid::Uuid>,
    Json(body): Json<ChangeStatusBody>,
) -> Response {
    let status = match body.status {
        AccountStatus::Active => application::identity::UserStatus::Active,
        AccountStatus::Disabled => application::identity::UserStatus::Disabled,
    };
    match state
        .users
        .change_status(&actor, user_id, status, body.expected_version)
        .await
    {
        Ok(result) => Json(crate::http_contract::UserStatusResult::from(result)).into_response(),
        Err(error) => admin_error(error, &request_id),
    }
}

async fn create_user(
    AdminAuth { actor }: AdminAuth,
    request_id: RequestId,
    State(state): State<AdminState>,
    Json(body): Json<CreateUserBody>,
) -> Response {
    match state
        .users
        .create_user(
            &actor,
            CreateUserCmd {
                username: body.username,
                email: body.email,
                display_name: body.display_name,
            },
        )
        .await
    {
        Ok(user) => (
            StatusCode::CREATED,
            Json(crate::http_contract::CreatedUser::from(user)),
        )
            .into_response(),
        Err(e) => admin_error(e, &request_id),
    }
}

/// 幂等分配：已持有该角色时不递增版本、不撤销会话（存储层 `ON CONFLICT DO NOTHING`）。
async fn assign_role(
    AdminAuth { actor }: AdminAuth,
    request_id: RequestId,
    State(state): State<AdminState>,
    Path((username, role)): Path<(String, String)>,
) -> Response {
    match state
        .roles
        .assign_to_username(&actor, &username, &role)
        .await
    {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => admin_error(e, &request_id),
    }
}

async fn remove_role(
    AdminAuth { actor }: AdminAuth,
    request_id: RequestId,
    State(state): State<AdminState>,
    Path((username, role)): Path<(String, String)>,
) -> Response {
    match state
        .roles
        .remove_from_username(&actor, &username, &role)
        .await
    {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => admin_error(e, &request_id),
    }
}

async fn list_roles(
    AdminAuth { actor }: AdminAuth,
    request_id: RequestId,
    State(state): State<AdminState>,
) -> Response {
    match state.roles.list(&actor).await {
        Ok(roles) => (
            StatusCode::OK,
            Json(
                roles
                    .into_iter()
                    .map(crate::http_contract::RoleSummary::from)
                    .collect::<Vec<_>>(),
            ),
        )
            .into_response(),
        Err(e) => admin_error(e, &request_id),
    }
}

pub(crate) fn export_contract(out: &mut Vec<String>) {
    crate::http_contract::declare::<CreateUserBody>(out);
    crate::http_contract::declare::<UpdateProfileBody>(out);
    crate::http_contract::declare::<ChangeStatusBody>(out);
    crate::http_contract::declare::<AccountStatus>(out);
}
