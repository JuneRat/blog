//! OAuth 配置的共享语义校验；入站输入和持久化重建使用同一套规则。
use crate::{
    UseCaseError,
    ports::{ProviderConfig, ProviderKind},
};

/// 集合完整性也需要校验，避免同一 id 在读取时被 first-match 隐式选择。
pub fn validate_provider_configs(providers: &[ProviderConfig]) -> Result<(), UseCaseError> {
    let mut ids = std::collections::BTreeSet::new();
    for config in providers {
        validate_provider_config(config)?;
        if !ids.insert(&config.id) {
            return Err(UseCaseError::Invalid("OAuth 提供商 id 不能重复".into()));
        }
    }
    Ok(())
}

/// 持久化损坏是服务端错误，不能作为用户请求错误返回，也不输出原始 JSON/秘密引用。
pub fn validate_stored_providers(providers: &[ProviderConfig]) -> Result<(), UseCaseError> {
    validate_provider_configs(providers)
        .map_err(|error| UseCaseError::Repository(format!("settings.oauth 配置无效：{error}")))
}

pub fn validate_provider_config(config: &ProviderConfig) -> Result<(), UseCaseError> {
    if config.id.is_empty() || config.id.len() > 64 {
        return Err(UseCaseError::Invalid("提供商 id 不合法".into()));
    }
    if config.id.contains('/') || config.id.contains('.') {
        return Err(UseCaseError::Invalid("提供商 id 不能包含 / 或 .".into()));
    }
    if let Some(name) = config.name.as_deref() {
        let name = name.trim();
        if name.is_empty() {
            return Err(UseCaseError::Invalid("提供商展示名不能为空白".into()));
        }
        if name.chars().count() > 100 {
            return Err(UseCaseError::Invalid("提供商展示名过长".into()));
        }
    }
    if config.client_id.trim().is_empty() {
        return Err(UseCaseError::Invalid("client_id 不能为空".into()));
    }
    if config.secret_ref.trim().is_empty() {
        return Err(UseCaseError::Invalid("secret_ref 不能为空".into()));
    }
    if matches!(config.kind, ProviderKind::Oidc) {
        let issuer = config.issuer.as_deref().unwrap_or("");
        if !issuer.starts_with("https://")
            || issuer.len() < 12
            || issuer.chars().any(|c| c.is_whitespace() || c.is_control())
        {
            return Err(UseCaseError::Invalid(
                "OIDC issuer 必须是精确 https URL".into(),
            ));
        }
    }
    Ok(())
}
