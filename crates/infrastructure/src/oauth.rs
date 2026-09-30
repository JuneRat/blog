//! OAuth 出站适配器：OIDC（发现文档、PKCE、ID 令牌校验）与 GitHub，
//! 以及 settings 的 oauth 分组存取、oauth_accounts 绑定存储和环境变量秘密源。

use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;

use crate::audit::record_change;
use crate::rbac::{CONFIGURED_EXTERNAL_IDENTITY, PostgresRbacStore};
use async_trait::async_trait;
use jsonwebtoken::jwk::Jwk;
use jsonwebtoken::{Algorithm, DecodingKey, Validation, decode, decode_header};
use serde::Deserialize;
use sqlx::{Executor, PgPool, Row};
use uuid::Uuid;

use application::error::{ConflictKind, UseCaseError};
use application::oauth_config::{validate_provider_configs, validate_stored_providers};
use application::ports::{
    ExternalIdentity, ExternalIdentityClient, OAuthAccountStore, OAuthConfigSnapshot,
    OAuthConfigStore, ProviderConfig, ProviderKind, SecretSource,
};

/// GitHub 固定平台实例端点。
const GITHUB_AUTHORIZE_URL: &str = "https://github.com/login/oauth/authorize";
const GITHUB_TOKEN_URL: &str = "https://github.com/login/oauth/access_token";
const GITHUB_USER_URL: &str = "https://api.github.com/user";
/// GitHub 身份命名空间键（docs：固定平台实例标识，不用 login 或邮箱）。
const GITHUB_PROVIDER_KEY: &str = "github";

fn external_err(context: &str, e: impl std::fmt::Display) -> UseCaseError {
    UseCaseError::External(format!("{context}：{e}"))
}

// ---------------------------------------------------------------------------
// 环境变量秘密源
// ---------------------------------------------------------------------------

/// secret_ref → 环境变量值；缺失视为部署错误。
pub struct EnvSecretSource;

impl SecretSource for EnvSecretSource {
    fn secret_for(&self, secret_ref: &str) -> Result<String, UseCaseError> {
        std::env::var(secret_ref).map_err(|_| {
            UseCaseError::External(format!("环境变量 {secret_ref} 未配置（secret_ref）"))
        })
    }
}

// ---------------------------------------------------------------------------
// 外部身份客户端（OIDC + GitHub）
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
struct OidcDiscovery {
    issuer: String,
    authorization_endpoint: String,
    token_endpoint: String,
    #[serde(default)]
    userinfo_endpoint: Option<String>,
    jwks_uri: String,
}

#[derive(Debug, Clone, Deserialize)]
struct OidcTokenResponse {
    id_token: Option<String>,
    #[serde(default)]
    access_token: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct OidcUserInfo {
    sub: String,
    #[serde(default)]
    email: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct Jwks {
    keys: Vec<Jwk>,
}

#[derive(Debug, Clone, Deserialize)]
struct GitHubUser {
    id: i64,
    #[serde(default)]
    email: Option<String>,
}

/// reqwest 实现：OIDC 发现/JWKS 按 issuer 缓存（进程内）。
pub struct ReqwestIdentityClient {
    http: reqwest::Client,
    secrets: Arc<dyn SecretSource>,
    discovery_cache: tokio::sync::RwLock<HashMap<String, Arc<OidcDiscovery>>>,
}

fn identity_http_builder() -> reqwest::ClientBuilder {
    reqwest::Client::builder()
        .https_only(true)
        .redirect(reqwest::redirect::Policy::none())
        .timeout(std::time::Duration::from_secs(10))
}

/// Discovery 可以声明跨域端点（例如 Google）；禁止降级、嵌入凭据和片段。
/// IdP 仍是管理员配置的信任边界；内网访问限制由部署的出站网络策略承担。
fn validate_endpoint(raw: &str, field: &str) -> Result<(), UseCaseError> {
    let url = reqwest::Url::parse(raw).ok();
    if raw.chars().any(|c| c.is_whitespace() || c.is_control())
        || url.as_ref().is_none_or(|url| {
            url.scheme() != "https"
                || url.host_str().is_none()
                || !url.username().is_empty()
                || url.password().is_some()
                || url.fragment().is_some()
        })
    {
        // 不把带凭据的原始 URL 放进错误或日志。
        return Err(UseCaseError::External(format!(
            "OIDC {field} 必须是不含凭据和片段的绝对 HTTPS URL"
        )));
    }
    Ok(())
}

impl OidcDiscovery {
    fn validate_endpoints(&self) -> Result<(), UseCaseError> {
        validate_endpoint(&self.authorization_endpoint, "authorization_endpoint")?;
        validate_endpoint(&self.token_endpoint, "token_endpoint")?;
        validate_endpoint(&self.jwks_uri, "jwks_uri")?;
        if let Some(url) = &self.userinfo_endpoint {
            validate_endpoint(url, "userinfo_endpoint")?;
        }
        Ok(())
    }
}

// Shared by the network adapter and signed-token regression tests.
fn validate_id_token_claims(
    jwks: &Jwks,
    header: &jsonwebtoken::Header,
    id_token: &str,
    issuer: &str,
    client_id: &str,
    expected_nonce: Option<&str>,
) -> Result<(String, Option<String>), UseCaseError> {
    let key = jwks
        .keys
        .iter()
        .filter(|k| matches!(k.algorithm, jsonwebtoken::jwk::AlgorithmParameters::RSA(_)))
        .find(|k| match (&header.kid, &k.common.key_id) {
            // 有 kid 时精确匹配；无 kid 的 RSA 密钥仅单一时可用。
            (Some(want), Some(have)) => want == have,
            (Some(_), None) => false,
            (None, _) => true,
        })
        .ok_or_else(|| UseCaseError::External("JWKS 中没有匹配的 RSA 公钥".into()))?;
    let decoding_key =
        DecodingKey::from_jwk(key).map_err(|e| external_err("JWKS 公钥构建失败", e))?;

    let mut validation = Validation::new(Algorithm::RS256);
    validation.set_issuer(&[issuer]);
    validation.set_required_spec_claims(&["exp", "iss", "aud", "sub"]);
    validation.set_audience(&[client_id]);
    validation.leeway = 60;
    validation.validate_exp = true;

    let claims: serde_json::Value = decode(id_token, &decoding_key, &validation)
        .map_err(|e| external_err("ID 令牌校验失败", e))?
        .claims;

    // An audience list may include this client alongside other recipients;
    // the authorized party must still be this client (OIDC Core §3.1.3.7).
    let multiple_audiences = claims
        .get("aud")
        .and_then(|aud| aud.as_array())
        .is_some_and(|aud| aud.len() > 1);
    if (multiple_audiences || claims.get("azp").is_some())
        && claims.get("azp").and_then(|azp| azp.as_str()) != Some(client_id)
    {
        return Err(UseCaseError::External("ID 令牌 azp 不匹配或缺失".into()));
    }

    let sub = claims
        .get("sub")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| UseCaseError::External("ID 令牌缺少非空 sub".into()))?
        .to_string();

    // OIDC 必须校验 nonce（防重放）。
    if let Some(expected) = expected_nonce {
        let got = claims.get("nonce").and_then(|v| v.as_str());
        if got != Some(expected) {
            return Err(UseCaseError::External(
                "ID 令牌 nonce 不匹配（疑似重放）".into(),
            ));
        }
    }

    let email = claims
        .get("email")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    Ok((sub, email))
}

impl ReqwestIdentityClient {
    pub fn new(secrets: Arc<dyn SecretSource>) -> Self {
        Self {
            http: identity_http_builder()
                .build()
                .expect("构建 HTTP 客户端失败"),
            secrets,
            discovery_cache: tokio::sync::RwLock::new(HashMap::new()),
        }
    }

    async fn discovery(&self, issuer: &str) -> Result<Arc<OidcDiscovery>, UseCaseError> {
        validate_endpoint(issuer, "issuer")?;
        if let Some(doc) = self.discovery_cache.read().await.get(issuer) {
            return Ok(doc.clone());
        }
        let url = format!(
            "{}/.well-known/openid-configuration",
            issuer.trim_end_matches('/')
        );
        let doc: OidcDiscovery = self
            .http
            .get(&url)
            .send()
            .await
            .map_err(|e| external_err("获取 OIDC 发现文档失败", e))?
            .error_for_status()
            .map_err(|e| external_err("OIDC 发现文档响应异常", e))?
            .json()
            .await
            .map_err(|e| external_err("解析 OIDC 发现文档失败", e))?;
        // 发现文档的 issuer 必须与配置的精确 issuer 一致（防 issuer 漂移）。
        if doc.issuer != issuer {
            return Err(UseCaseError::External(format!(
                "发现文档 issuer（{}）与配置的精确 issuer（{issuer}）不一致",
                doc.issuer
            )));
        }
        doc.validate_endpoints()?;
        let doc = Arc::new(doc);
        self.discovery_cache
            .write()
            .await
            .insert(issuer.to_string(), doc.clone());
        Ok(doc)
    }

    async fn jwks(&self, discovery: &OidcDiscovery) -> Result<Jwks, UseCaseError> {
        let jwks: Jwks = self
            .http
            .get(&discovery.jwks_uri)
            .send()
            .await
            .map_err(|e| external_err("获取 JWKS 失败", e))?
            .error_for_status()
            .map_err(|e| external_err("JWKS 响应异常", e))?
            .json()
            .await
            .map_err(|e| external_err("解析 JWKS 失败", e))?;
        Ok(jwks)
    }

    /// 校验 ID 令牌：签名（JWKS）、精确 issuer、audience、有效期与 nonce。
    #[allow(clippy::too_many_arguments)]
    async fn validate_id_token(
        &self,
        discovery: &OidcDiscovery,
        id_token: &str,
        client_id: &str,
        expected_nonce: Option<&str>,
    ) -> Result<(String, Option<String>), UseCaseError> {
        let header = decode_header(id_token).map_err(|e| external_err("ID 令牌头解析失败", e))?;
        let jwks = self.jwks(discovery).await?;
        validate_id_token_claims(
            &jwks,
            &header,
            id_token,
            &discovery.issuer,
            client_id,
            expected_nonce,
        )
    }

    async fn oidc_exchange(
        &self,
        config: &ProviderConfig,
        issuer: &str,
        code: &str,
        verifier: Option<&str>,
        nonce: Option<&str>,
        redirect_uri: &str,
    ) -> Result<ExternalIdentity, UseCaseError> {
        let discovery = self.discovery(issuer).await?;
        let secret = self.secrets.secret_for(&config.secret_ref)?;

        let mut form: Vec<(&str, &str)> = vec![
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", redirect_uri),
            ("client_id", &config.client_id),
            ("client_secret", &secret),
        ];
        let verifier =
            verifier.ok_or_else(|| UseCaseError::External("OIDC 必须携带 PKCE verifier".into()))?;
        form.push(("code_verifier", verifier));

        let token: OidcTokenResponse = self
            .http
            .post(&discovery.token_endpoint)
            .form(&form)
            .send()
            .await
            .map_err(|e| external_err("OIDC 令牌交换失败", e))?
            .error_for_status()
            .map_err(|e| external_err("OIDC 令牌交换响应异常", e))?
            .json()
            .await
            .map_err(|e| external_err("解析 OIDC 令牌响应失败", e))?;

        let id_token = token
            .id_token
            .ok_or_else(|| UseCaseError::External("OIDC 响应缺少 id_token".into()))?;
        let (sub, email_from_token) = self
            .validate_id_token(&discovery, &id_token, &config.client_id, nonce)
            .await?;

        // 优先 userinfo 快照邮箱，并核对其 sub 与 id_token 一致。
        let mut email = email_from_token;
        if let (Some(userinfo_url), Some(access_token)) = (
            discovery.userinfo_endpoint.as_deref(),
            token.access_token.as_deref(),
        ) {
            let userinfo: OidcUserInfo = self
                .http
                .get(userinfo_url)
                .bearer_auth(access_token)
                .send()
                .await
                .map_err(|e| external_err("获取 userinfo 失败", e))?
                .error_for_status()
                .map_err(|e| external_err("userinfo 响应异常", e))?
                .json()
                .await
                .map_err(|e| external_err("解析 userinfo 失败", e))?;
            if userinfo.sub != sub {
                return Err(UseCaseError::External(
                    "userinfo 的 sub 与 ID 令牌不一致".into(),
                ));
            }
            if userinfo.email.is_some() {
                email = userinfo.email;
            }
        }

        Ok(ExternalIdentity {
            provider_key: issuer.to_string(),
            provider_user_id: sub,
            email,
        })
    }

    async fn github_exchange(
        &self,
        config: &ProviderConfig,
        code: &str,
        redirect_uri: &str,
    ) -> Result<ExternalIdentity, UseCaseError> {
        let secret = self.secrets.secret_for(&config.secret_ref)?;

        let payload = serde_json::json!({
            "client_id": config.client_id,
            "client_secret": secret,
            "code": code,
            "redirect_uri": redirect_uri,
        });
        let token: serde_json::Value = self
            .http
            .post(GITHUB_TOKEN_URL)
            .header("Accept", "application/json")
            .json(&payload)
            .send()
            .await
            .map_err(|e| external_err("GitHub 令牌交换失败", e))?
            .error_for_status()
            .map_err(|e| external_err("GitHub 令牌交换响应异常", e))?
            .json()
            .await
            .map_err(|e| external_err("解析 GitHub 令牌响应失败", e))?;
        let access_token = token
            .get("access_token")
            .and_then(|v| v.as_str())
            .ok_or_else(|| UseCaseError::External("GitHub 响应缺少 access_token".into()))?;

        // 稳定数值用户 ID 是唯一身份键；login/邮箱只是资料。
        let user: GitHubUser = self
            .http
            .get(GITHUB_USER_URL)
            .bearer_auth(access_token)
            .header("User-Agent", "blog")
            .send()
            .await
            .map_err(|e| external_err("读取 GitHub 用户信息失败", e))?
            .error_for_status()
            .map_err(|e| external_err("GitHub 用户信息响应异常", e))?
            .json()
            .await
            .map_err(|e| external_err("解析 GitHub 用户信息失败", e))?;

        Ok(ExternalIdentity {
            provider_key: GITHUB_PROVIDER_KEY.to_string(),
            provider_user_id: user.id.to_string(),
            email: user.email,
        })
    }
}

#[async_trait]
impl ExternalIdentityClient for ReqwestIdentityClient {
    async fn authorize_url(
        &self,
        config: &ProviderConfig,
        state: &str,
        challenge: Option<&str>,
        nonce: Option<&str>,
        redirect_uri: &str,
    ) -> Result<String, UseCaseError> {
        match config.kind {
            ProviderKind::Oidc => {
                let issuer = config.issuer.as_deref().unwrap_or_default();
                let discovery = self.discovery(issuer).await?;
                let scopes = if config.scopes.is_empty() {
                    "openid profile email".to_string()
                } else {
                    config.scopes.join(" ")
                };
                let mut url = reqwest::Url::parse(&discovery.authorization_endpoint)
                    .map_err(|e| external_err("授权端点 URL 非法", e))?;
                {
                    let mut pairs = url.query_pairs_mut();
                    pairs.append_pair("response_type", "code");
                    pairs.append_pair("client_id", &config.client_id);
                    pairs.append_pair("redirect_uri", redirect_uri);
                    pairs.append_pair("scope", &scopes);
                    pairs.append_pair("state", state);
                    if let Some(nonce) = nonce {
                        pairs.append_pair("nonce", nonce);
                    }
                    if let Some(challenge) = challenge {
                        pairs.append_pair("code_challenge", challenge);
                        pairs.append_pair("code_challenge_method", "S256");
                    }
                }
                Ok(url.to_string())
            }
            ProviderKind::GitHub => {
                let scopes = if config.scopes.is_empty() {
                    "read:user".to_string()
                } else {
                    config.scopes.join(" ")
                };
                let mut url = reqwest::Url::parse(GITHUB_AUTHORIZE_URL)
                    .map_err(|e| external_err("授权端点 URL 非法", e))?;
                {
                    let mut pairs = url.query_pairs_mut();
                    pairs.append_pair("client_id", &config.client_id);
                    pairs.append_pair("redirect_uri", redirect_uri);
                    pairs.append_pair("scope", &scopes);
                    pairs.append_pair("state", state);
                }
                Ok(url.to_string())
            }
        }
    }

    async fn exchange(
        &self,
        config: &ProviderConfig,
        code: &str,
        verifier: Option<&str>,
        nonce: Option<&str>,
        redirect_uri: &str,
    ) -> Result<ExternalIdentity, UseCaseError> {
        match config.kind {
            ProviderKind::Oidc => {
                let issuer = config.issuer.as_deref().unwrap_or_default();
                self.oidc_exchange(config, issuer, code, verifier, nonce, redirect_uri)
                    .await
            }
            ProviderKind::GitHub => self.github_exchange(config, code, redirect_uri).await,
        }
    }
}

// ---------------------------------------------------------------------------
// settings 的 oauth 分组
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct OAuthSettingsValue {
    #[serde(default)]
    providers: Vec<ProviderConfig>,
}

/// oauth 配置存 settings（key='oauth'，value 为对象；不含秘密值）。
pub struct PostgresOAuthConfigStore {
    pool: PgPool,
}

impl PostgresOAuthConfigStore {
    pub fn new(database: crate::Database) -> Self {
        let pool = database.pool;
        Self { pool }
    }
}

async fn read_oauth_settings(
    executor: impl Executor<'_, Database = sqlx::Postgres>,
) -> Result<OAuthConfigSnapshot, UseCaseError> {
    let value: Option<(sqlx::types::Json<OAuthSettingsValue>, i64)> =
        sqlx::query_as("SELECT value, version FROM settings WHERE key = 'oauth'")
            .fetch_optional(executor)
            .await
            .map_err(|e| UseCaseError::Repository(e.to_string()))?;
    let snapshot = value
        .map(|(value, version)| OAuthConfigSnapshot {
            providers: value.0.providers,
            version,
        })
        .unwrap_or_default();
    validate_stored_providers(&snapshot.providers)?;
    Ok(snapshot)
}

fn provider_namespaces(providers: &[ProviderConfig]) -> BTreeSet<&str> {
    providers
        .iter()
        .filter_map(|provider| match provider.kind {
            ProviderKind::GitHub => Some("github"),
            ProviderKind::Oidc => provider.issuer.as_deref(),
        })
        .collect()
}

#[async_trait]
impl OAuthConfigStore for PostgresOAuthConfigStore {
    async fn read(&self) -> Result<OAuthConfigSnapshot, UseCaseError> {
        read_oauth_settings(&self.pool).await
    }

    async fn save(
        &self,
        providers: &[ProviderConfig],
        expected_version: i64,
        audit_actor: application::audit::AuditContext,
    ) -> Result<i64, UseCaseError> {
        validate_provider_configs(providers)?;
        let value = serde_json::json!({
            "schema_version": 1,
            "providers": providers,
        });
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| UseCaseError::Repository(e.to_string()))?;
        crate::persistence::acquire_identity_lock(&mut *tx)
            .await
            .map_err(|e| UseCaseError::Repository(e.to_string()))?;
        let before = read_oauth_settings(&mut *tx).await?;
        if before.version != expected_version {
            return Err(UseCaseError::VersionConflict);
        }
        if before.providers == providers {
            tx.commit()
                .await
                .map_err(|e| UseCaseError::Repository(e.to_string()))?;
            return Ok(before.version);
        }
        let removes_namespace =
            !provider_namespaces(&before.providers).is_subset(&provider_namespaces(providers));
        let statement = if expected_version == 0 {
            "INSERT INTO settings (key, value, version, updated_at) \
             VALUES ('oauth', $1, 1, now()) ON CONFLICT (key) DO NOTHING RETURNING version"
        } else {
            "UPDATE settings SET value=$1, version=version+1, updated_at=now() \
             WHERE key='oauth' AND version=$2 RETURNING version"
        };
        let mut query = sqlx::query_scalar(statement).bind(value);
        if expected_version != 0 {
            query = query.bind(expected_version);
        }
        let version: i64 = query
            .fetch_optional(&mut *tx)
            .await
            .map_err(|e| UseCaseError::Repository(e.to_string()))?
            .ok_or(UseCaseError::VersionConflict)?;
        if removes_namespace {
            // 已有 Admin 时，变更后的配置必须仍能对应至少一个 active Admin。
            // 空库/分步 CLI 引导尚无 Admin 时允许维护配置，不调用外部身份服务。
            let has_admin: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM user_roles ur JOIN roles r ON r.id=ur.role_id \
                 WHERE r.code='admin')",
            )
            .fetch_one(&mut *tx)
            .await
            .map_err(|e| UseCaseError::Repository(e.to_string()))?;
            if has_admin && PostgresRbacStore::active_admin_count(&mut *tx).await? == 0 {
                return Err(UseCaseError::LastAdminProtected);
            }
        }
        record_change(
            &mut tx,
            audit_actor,
            "settings.oauth",
            "settings",
            "oauth",
            serde_json::json!({"version":version,"provider_count":providers.len()}),
        )
        .await?;
        tx.commit()
            .await
            .map_err(|e| UseCaseError::Repository(e.to_string()))?;
        Ok(version)
    }
}

// ---------------------------------------------------------------------------
// oauth_accounts 绑定存储
// ---------------------------------------------------------------------------

pub struct PostgresOAuthAccountStore {
    pool: PgPool,
}

impl PostgresOAuthAccountStore {
    pub fn new(database: crate::Database) -> Self {
        let pool = database.pool;
        Self { pool }
    }
}

#[async_trait]
impl OAuthAccountStore for PostgresOAuthAccountStore {
    async fn find_user_by_external_id(
        &self,
        provider_key: &str,
        provider_user_id: &str,
    ) -> Result<Option<Uuid>, UseCaseError> {
        let row: Option<(Uuid,)> = sqlx::query_as(
            "SELECT oa.user_id FROM oauth_accounts oa \
             JOIN users u ON u.id = oa.user_id \
             WHERE oa.provider = $1 AND oa.subject = $2 AND u.status = 'active' AND u.deleted_at IS NULL",
        )
        .bind(provider_key)
        .bind(provider_user_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| UseCaseError::Repository(e.to_string()))?;
        Ok(row.map(|r| r.0))
    }

    async fn bind(
        &self,
        user_id: Uuid,
        provider_key: &str,
        provider_user_id: &str,
        _email: Option<String>,
        audit_actor: application::audit::AuditContext,
    ) -> Result<(), UseCaseError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| UseCaseError::Repository(e.to_string()))?;
        crate::persistence::acquire_identity_lock(&mut *tx)
            .await
            .map_err(|e| UseCaseError::Repository(e.to_string()))?;
        // This path also revokes sessions; do not block the KEY SHARE taken by
        // a concurrent session insertion after capacity eviction.
        sqlx::query("SELECT id FROM users WHERE id=$1 AND status='active' AND deleted_at IS NULL FOR NO KEY UPDATE")
            .bind(user_id).fetch_optional(&mut *tx).await
            .map_err(|e| UseCaseError::Repository(e.to_string()))?
            .ok_or_else(|| UseCaseError::NotFound("用户".into()))?;
        let result = sqlx::query(
            "INSERT INTO oauth_accounts (user_id, provider, subject) VALUES ($1, $2, $3) \
             ON CONFLICT (provider, subject) DO NOTHING",
        )
        .bind(user_id)
        .bind(provider_key)
        .bind(provider_user_id)
        .execute(&mut *tx)
        .await
        .map_err(|e| UseCaseError::Repository(e.to_string()))?;
        if result.rows_affected() == 0 {
            return Err(UseCaseError::Conflict(ConflictKind::ExternalIdentity));
        }
        sqlx::query("UPDATE users SET version=version+1, auth_version=auth_version+1, updated_at=now() WHERE id=$1")
            .bind(user_id).execute(&mut *tx).await.map_err(|e| UseCaseError::Repository(e.to_string()))?;
        sqlx::query("DELETE FROM sessions WHERE user_id=$1")
            .bind(user_id)
            .execute(&mut *tx)
            .await
            .map_err(|e| UseCaseError::Repository(e.to_string()))?;
        let (version, auth_version): (i64, i64) =
            sqlx::query_as("SELECT version,auth_version FROM users WHERE id=$1")
                .bind(user_id)
                .fetch_one(&mut *tx)
                .await
                .map_err(|e| UseCaseError::Repository(e.to_string()))?;
        record_change(
            &mut tx,
            audit_actor,
            "user.oauth.bind",
            "user",
            &user_id.to_string(),
            serde_json::json!({
                "provider": provider_key, "version": version, "auth_version": auth_version,
            }),
        )
        .await?;
        tx.commit()
            .await
            .map_err(|e| UseCaseError::Repository(e.to_string()))
    }

    async fn unbind(
        &self,
        user_id: Uuid,
        provider_key: &str,
        provider_user_id: &str,
        audit_actor: application::audit::AuditContext,
    ) -> Result<(), UseCaseError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| UseCaseError::Repository(e.to_string()))?;
        // 身份变更走统一排他锁（docs §3 协议）。
        crate::persistence::acquire_identity_lock(&mut *tx)
            .await
            .map_err(|e| UseCaseError::Repository(e.to_string()))?;

        let exists: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM oauth_accounts WHERE user_id=$1 AND provider=$2 AND subject=$3)",
        )
        .bind(user_id).bind(provider_key).bind(provider_user_id)
        .fetch_one(&mut *tx).await.map_err(|e| UseCaseError::Repository(e.to_string()))?;
        if !exists {
            return tx
                .commit()
                .await
                .map_err(|e| UseCaseError::Repository(e.to_string()));
        }

        let (external_identities, password, configured): (i64, Option<String>, bool) =
            sqlx::query_as(&format!(
                "SELECT \
                (SELECT count(*) FROM oauth_accounts oa WHERE oa.user_id = $1 \
                 AND {CONFIGURED_EXTERNAL_IDENTITY}), \
                (SELECT password_hash FROM users WHERE id = $1), \
                EXISTS (SELECT 1 FROM oauth_accounts oa WHERE oa.user_id=$1 \
                        AND oa.provider=$2 AND oa.subject=$3 AND {CONFIGURED_EXTERNAL_IDENTITY})",
            ))
            .bind(user_id)
            .bind(provider_key)
            .bind(provider_user_id)
            .fetch_one(&mut *tx)
            .await
            .map_err(|e| UseCaseError::Repository(e.to_string()))?;

        // 解绑必须保留另一外部身份或本地密码。
        let methods = domain::identity::LoginMethods {
            password_enabled: password.is_some(),
            external_identities,
        };
        if configured && !methods.can_remove(domain::identity::LoginMethod::ExternalIdentity) {
            return Err(UseCaseError::Forbidden);
        }

        let result = sqlx::query(
            "DELETE FROM oauth_accounts \
             WHERE user_id = $1 AND provider = $2 AND subject = $3",
        )
        .bind(user_id)
        .bind(provider_key)
        .bind(provider_user_id)
        .execute(&mut *tx)
        .await
        .map_err(|e| UseCaseError::Repository(e.to_string()))?;
        if result.rows_affected() > 0 {
            sqlx::query("UPDATE users SET version = version + 1, auth_version = auth_version + 1, updated_at = now() WHERE id = $1")
                .bind(user_id)
                .execute(&mut *tx)
                .await
                .map_err(|e| UseCaseError::Repository(e.to_string()))?;
            sqlx::query("DELETE FROM sessions WHERE user_id=$1")
                .bind(user_id)
                .execute(&mut *tx)
                .await
                .map_err(|e| UseCaseError::Repository(e.to_string()))?;
        }
        let (version, auth_version): (i64, i64) =
            sqlx::query_as("SELECT version,auth_version FROM users WHERE id=$1")
                .bind(user_id)
                .fetch_one(&mut *tx)
                .await
                .map_err(|e| UseCaseError::Repository(e.to_string()))?;
        record_change(
            &mut tx,
            audit_actor,
            "user.oauth.unbind",
            "user",
            &user_id.to_string(),
            serde_json::json!({
                "provider": provider_key, "version": version, "auth_version": auth_version,
            }),
        )
        .await?;
        tx.commit()
            .await
            .map_err(|e| UseCaseError::Repository(e.to_string()))
    }

    async fn list_for_user(&self, user_id: Uuid) -> Result<Vec<ExternalIdentity>, UseCaseError> {
        let rows = sqlx::query(
            "SELECT provider, subject FROM oauth_accounts WHERE user_id = $1 \
             ORDER BY provider, subject",
        )
        .bind(user_id)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| UseCaseError::Repository(e.to_string()))?;
        rows.iter()
            .map(|row| {
                Ok(ExternalIdentity {
                    provider_key: row
                        .try_get("provider")
                        .map_err(|e| UseCaseError::Repository(e.to_string()))?,
                    provider_user_id: row
                        .try_get("subject")
                        .map_err(|e| UseCaseError::Repository(e.to_string()))?,
                    email: None,
                })
            })
            .collect()
    }
}

#[cfg(test)]
mod outbound_security_tests {
    use super::*;

    fn signed_id_token(claims: &serde_json::Value) -> String {
        let key = jsonwebtoken::EncodingKey::from_rsa_pem(include_bytes!(
            "fixtures/oidc-test-rsa-key.pem"
        ))
        .unwrap();
        let mut header = jsonwebtoken::Header::new(Algorithm::RS256);
        header.kid = Some("oidc-test".into());
        jsonwebtoken::encode(&header, claims, &key).unwrap()
    }

    fn id_token_claims() -> serde_json::Value {
        serde_json::json!({
            "iss": "https://accounts.google.com",
            "aud": "test-client",
            "sub": "subject-42",
            "exp": time::OffsetDateTime::now_utc().unix_timestamp() + 300,
            "nonce": "test-nonce",
            "email": "member@example.test",
        })
    }

    fn verify_signed_id_token(token: &str) -> Result<(String, Option<String>), UseCaseError> {
        let jwks = serde_json::from_str(include_str!("fixtures/oidc-test-jwks.json")).unwrap();
        validate_id_token_claims(
            &jwks,
            &decode_header(token).unwrap(),
            token,
            "https://accounts.google.com",
            "test-client",
            Some("test-nonce"),
        )
    }

    #[test]
    fn valid_rs256_id_token_is_verified_with_matching_jwks() {
        assert_eq!(
            verify_signed_id_token(&signed_id_token(&id_token_claims())).unwrap(),
            ("subject-42".into(), Some("member@example.test".into()))
        );
        let mut claims = id_token_claims();
        claims["aud"] = serde_json::json!(["other-client", "test-client"]);
        claims["azp"] = serde_json::json!("test-client");
        claims.as_object_mut().unwrap().remove("email");
        assert_eq!(
            verify_signed_id_token(&signed_id_token(&claims)).unwrap(),
            ("subject-42".into(), None)
        );
    }

    #[test]
    fn rs256_id_token_rejects_an_invalid_signature() {
        let token = signed_id_token(&id_token_claims());
        let (message, signature) = token.rsplit_once('.').unwrap();
        let mut signature = signature.as_bytes().to_vec();
        signature[0] = if signature[0] == b'A' { b'B' } else { b'A' };
        let tampered = format!("{message}.{}", String::from_utf8(signature).unwrap());
        assert!(verify_signed_id_token(&tampered).is_err());
    }

    #[test]
    fn signed_id_tokens_require_issuer_audience_expiry_subject_and_nonce() {
        for field in ["iss", "aud", "exp", "sub", "nonce"] {
            let mut claims = id_token_claims();
            claims.as_object_mut().unwrap().remove(field);
            assert!(
                verify_signed_id_token(&signed_id_token(&claims)).is_err(),
                "missing {field} must fail even with a valid signature"
            );
        }
        for (field, value) in [
            ("iss", serde_json::json!("https://other-issuer.example")),
            ("aud", serde_json::json!("other-client")),
            ("nonce", serde_json::json!("other-nonce")),
            ("sub", serde_json::json!("")),
            ("exp", serde_json::json!(0)),
        ] {
            let mut claims = id_token_claims();
            claims[field] = value;
            assert!(
                verify_signed_id_token(&signed_id_token(&claims)).is_err(),
                "invalid {field} must fail even with a valid signature"
            );
        }
    }

    #[test]
    fn signed_id_tokens_reject_other_authorized_parties() {
        for audience in [
            serde_json::json!("test-client"),
            serde_json::json!(["test-client", "other-client"]),
        ] {
            for azp in [serde_json::json!("other-client"), serde_json::json!(null)] {
                let mut claims = id_token_claims();
                claims["aud"] = audience.clone();
                claims["azp"] = azp;
                assert!(verify_signed_id_token(&signed_id_token(&claims)).is_err());
            }
        }
        let mut claims = id_token_claims();
        claims["aud"] = serde_json::json!(["test-client", "other-client"]);
        assert!(verify_signed_id_token(&signed_id_token(&claims)).is_err());
    }

    fn discovery() -> OidcDiscovery {
        OidcDiscovery {
            issuer: "https://accounts.google.com".into(),
            authorization_endpoint: "https://accounts.google.com/o/oauth2/v2/auth".into(),
            token_endpoint: "https://oauth2.googleapis.com/token".into(),
            jwks_uri: "https://www.googleapis.com/oauth2/v3/certs".into(),
            userinfo_endpoint: Some("https://openidconnect.googleapis.com/v1/userinfo".into()),
        }
    }

    #[test]
    fn discovery_allows_cross_host_https_but_validates_every_endpoint() {
        assert!(discovery().validate_endpoints().is_ok());
        for bad in [
            "http://127.0.0.1/token",
            "/token",
            "https://idp.example:invalid/token",
            "https://user:secret@idp.example/token",
            "https://idp.example/token#fragment",
            "https://idp.example/\npath",
        ] {
            for field in [
                "authorization_endpoint",
                "token_endpoint",
                "jwks_uri",
                "userinfo_endpoint",
            ] {
                let mut doc = discovery();
                match field {
                    "authorization_endpoint" => doc.authorization_endpoint = bad.into(),
                    "token_endpoint" => doc.token_endpoint = bad.into(),
                    "jwks_uri" => doc.jwks_uri = bad.into(),
                    _ => doc.userinfo_endpoint = Some(bad.into()),
                }
                let error = doc.validate_endpoints().unwrap_err().to_string();
                assert!(error.contains(field));
                assert!(!error.contains("secret"));
            }
        }
        assert!(
            validate_endpoint("https://tokens.example/token?tenant=a", "token_endpoint").is_ok()
        );
    }

    #[tokio::test]
    async fn identity_client_rejects_plain_http() {
        assert!(
            identity_http_builder()
                .build()
                .unwrap()
                .get("http://127.0.0.1:1/token")
                .send()
                .await
                .unwrap_err()
                .is_builder()
        );
    }

    #[tokio::test]
    async fn token_posts_do_not_follow_307_or_308_redirects() {
        use std::io::{Read, Write};
        for status in [307, 308] {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let address = listener.local_addr().unwrap();
            let server = std::thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                    .unwrap();
                let mut buffer = [0; 8192];
                assert!(stream.read(&mut buffer).unwrap() > 0);
                write!(stream, "HTTP/1.1 {status} Redirect\r\nLocation: http://{address}/leak\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
                // 只服务一次；若客户端跟随跳转，第二次连接会失败而非返回 3xx。
            });
            // 仅测试传输改用 loopback HTTP，保留生产构造器的重定向策略。
            let result = identity_http_builder()
                .https_only(false)
                .no_proxy()
                .build()
                .unwrap()
                .post(format!("http://{address}/token"))
                .form(&[("client_secret", "test-secret")])
                .send()
                .await;
            server.join().unwrap();
            assert_eq!(result.unwrap().status().as_u16(), status);
        }
    }
}
