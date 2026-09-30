//! Authenticated management of durable, allow-listed background tasks.
use std::sync::Arc;

use application::{
    UseCaseError,
    tasks::{TaskListQuery, TasksInteractor},
};
use axum::{
    Json, Router,
    extract::{
        DefaultBodyLimit, FromRef, Path, Query, State,
        rejection::{JsonRejection, PathRejection, QueryRejection},
    },
    http::StatusCode,
    middleware,
    response::{IntoResponse, Response},
    routing::{get, post, put},
};
use uuid::Uuid;

use crate::{
    http_admin::AdminAuth,
    http_auth::AdminState,
    http_contract::{TaskRun, TaskSchedule, TaskScheduleBody, TaskStartBody, TaskView},
    http_support::{RequestId, admin_error, no_store},
};

#[derive(Clone)]
pub struct TasksState {
    pub tasks: Arc<TasksInteractor>,
    pub admin: AdminState,
}
impl FromRef<TasksState> for AdminState {
    fn from_ref(state: &TasksState) -> Self {
        state.admin.clone()
    }
}

pub fn tasks_router(state: TasksState) -> Router {
    Router::new()
        .route("/api/admin/v1/tasks", get(view).post(start))
        .route("/api/admin/v1/tasks/{id}/retry", post(retry))
        .route("/api/admin/v1/tasks/{id}/cancel", post(cancel))
        .route("/api/admin/v1/tasks/retention-schedule", put(save_schedule))
        .layer(DefaultBodyLimit::max(8 * 1024))
        .layer(middleware::from_fn(no_store))
        .with_state(state)
}

async fn view(
    State(s): State<TasksState>,
    auth: AdminAuth,
    id: RequestId,
    query: Result<Query<TaskListQuery>, QueryRejection>,
) -> Response {
    if !auth.actor.has_permission("settings.manage") {
        return admin_error(UseCaseError::Forbidden, &id);
    }
    let query = match query {
        Ok(Query(query)) => query,
        Err(_) => return admin_error(UseCaseError::Invalid("任务筛选参数无效".into()), &id),
    };
    match s
        .tasks
        .view(&auth.actor, query)
        .await
        .and_then(TaskView::try_from)
    {
        Ok(view) => Json(view).into_response(),
        Err(error) => admin_error(error, &id),
    }
}

async fn start(
    State(s): State<TasksState>,
    auth: AdminAuth,
    id: RequestId,
    body: Result<Json<TaskStartBody>, JsonRejection>,
) -> Response {
    if !auth.actor.has_permission("settings.manage") {
        return admin_error(UseCaseError::Forbidden, &id);
    }
    let body = match body {
        Ok(Json(body)) => body,
        Err(error) => return invalid_body(error, &id),
    };
    match s
        .tasks
        .enqueue(&auth.actor, body.into())
        .await
        .and_then(TaskRun::try_from)
    {
        Ok(run) => (StatusCode::ACCEPTED, Json(run)).into_response(),
        Err(error) => admin_error(error, &id),
    }
}

async fn retry(
    State(s): State<TasksState>,
    auth: AdminAuth,
    id: RequestId,
    path: Result<Path<Uuid>, PathRejection>,
) -> Response {
    if !auth.actor.has_permission("settings.manage") {
        return admin_error(UseCaseError::Forbidden, &id);
    }
    let run_id = match path {
        Ok(Path(id)) => id,
        Err(_) => return admin_error(UseCaseError::Invalid("任务编号无效".into()), &id),
    };
    match s
        .tasks
        .retry(&auth.actor, run_id)
        .await
        .and_then(TaskRun::try_from)
    {
        Ok(run) => (StatusCode::ACCEPTED, Json(run)).into_response(),
        Err(error) => admin_error(error, &id),
    }
}

async fn cancel(
    State(s): State<TasksState>,
    auth: AdminAuth,
    id: RequestId,
    path: Result<Path<Uuid>, PathRejection>,
) -> Response {
    if !auth.actor.has_permission("settings.manage") {
        return admin_error(UseCaseError::Forbidden, &id);
    }
    let run_id = match path {
        Ok(Path(id)) => id,
        Err(_) => return admin_error(UseCaseError::Invalid("任务编号无效".into()), &id),
    };
    match s
        .tasks
        .cancel(&auth.actor, run_id)
        .await
        .and_then(TaskRun::try_from)
    {
        Ok(run) => Json(run).into_response(),
        Err(error) => admin_error(error, &id),
    }
}

async fn save_schedule(
    State(s): State<TasksState>,
    auth: AdminAuth,
    id: RequestId,
    body: Result<Json<TaskScheduleBody>, JsonRejection>,
) -> Response {
    if !auth.actor.has_permission("settings.manage") {
        return admin_error(UseCaseError::Forbidden, &id);
    }
    let body = match body {
        Ok(Json(body)) => body,
        Err(error) => return invalid_body(error, &id),
    };
    match s
        .tasks
        .save_retention_schedule(&auth.actor, body.into())
        .await
        .and_then(TaskSchedule::try_from)
    {
        Ok(schedule) => Json(schedule).into_response(),
        Err(error) => admin_error(error, &id),
    }
}

fn invalid_body(error: JsonRejection, id: &RequestId) -> Response {
    if error.status() == StatusCode::PAYLOAD_TOO_LARGE {
        return StatusCode::PAYLOAD_TOO_LARGE.into_response();
    }
    admin_error(UseCaseError::Invalid("任务请求格式无效".into()), id)
}
