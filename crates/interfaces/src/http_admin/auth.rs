//! 管理请求身份提取、CSRF 与同源检查。

use crate::http_auth::AdminState;
use crate::http_support::{
    RequestId, admin_error, cookie_value, csrf_token_matches, ensure_same_origin,
};
use application::error::UseCaseError;
use application::identity::Actor;
use application::ports::SESSION_COOKIE;
use axum::extract::FromRef;
use axum::http::request::Parts;
use axum::response::Response;

/// 已认证的管理调用（含 CSRF/Origin 校验结果）。
pub struct AdminAuth {
    pub actor: Actor,
}

impl<S> axum::extract::FromRequestParts<S> for AdminAuth
where
    S: Send + Sync,
    AdminState: axum::extract::FromRef<S>,
{
    type Rejection = Response;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let admin = AdminState::from_ref(state);
        // 提前拒绝（未登录/CSRF/跨源）也必须带上请求编号，故先从扩展取上下文。
        let request_id = parts
            .extensions
            .get::<RequestId>()
            .cloned()
            .unwrap_or_else(RequestId::generate);
        let Some(token) = cookie_value(&parts.headers, SESSION_COOKIE) else {
            return Err(admin_error(UseCaseError::Unauthenticated, &request_id));
        };
        // 一次校验同时拿到会话记录与 Actor：分开调用会对同一请求写两次 last_seen_at。
        // 「先令牌有效、再版本比对」的判定顺序不变，CSRF 仍在授权动作之前。
        let (record, actor) = admin
            .auth
            .session_actor(&token)
            .await
            .map_err(|e| admin_error(e, &request_id))?;

        // 写方法校验 CSRF + Origin（读方法不产生副作用）。
        let write_method = !matches!(parts.method.as_str(), "GET" | "HEAD" | "OPTIONS");
        if write_method {
            ensure_same_origin(&parts.headers).map_err(|e| admin_error(e, &request_id))?;
            let provided = parts
                .headers
                .get("x-csrf-token")
                .and_then(|v| v.to_str().ok())
                .unwrap_or_default();
            if provided.is_empty() || !csrf_token_matches(provided, &record.csrf_token) {
                return Err(admin_error(
                    UseCaseError::Invalid("CSRF 校验失败".into()),
                    &request_id,
                ));
            }
        }

        // 身份已由会话验证：只有这里可以补录 actor，完成日志才带上它。
        request_id.set_actor(actor.user_id.0);
        let actor = actor.with_audit_ip(crate::http_client_ip::ClientAddress::from_parts(parts).0);
        Ok(Self { actor })
    }
}
