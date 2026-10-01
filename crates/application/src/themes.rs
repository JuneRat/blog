//! Installed, validated themes and the persisted active selection.
use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};

use serde::Serialize;

use crate::error::UseCaseError;
use crate::ports::ThemeRenderer;

/// Immutable asset bytes from the same loaded release as the templates.
#[derive(Clone)]
pub struct ThemeAssets {
    pub slug: String,
    pub version: String,
    pub files: Arc<BTreeMap<String, Arc<[u8]>>>,
}

impl ThemeAssets {
    pub fn url(&self, path: &str) -> String {
        let encoded = path
            .split('/')
            .map(crate::seo::encode_path_segment)
            .collect::<Vec<_>>()
            .join("/");
        format!("/assets/{}/{}/{encoded}", self.slug, self.version)
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ThemeOption {
    pub slug: String,
    pub name: String,
    pub release: String,
    pub id: Option<uuid::Uuid>,
    pub config_version: Option<i64>,
    pub config_schema_version: Option<u32>,
}

type ThemeRenderers = BTreeMap<String, (String, Arc<dyn ThemeRenderer>)>;

pub struct ThemeRegistry {
    themes: RwLock<ThemeRenderers>,
    assets: RwLock<BTreeMap<String, ThemeAssets>>,
    fallback: String,
    schemas: RwLock<BTreeMap<String, Arc<crate::theme_config::ThemeSchema>>>,
}

impl ThemeRegistry {
    pub fn new(fallback: String) -> Self {
        Self {
            themes: RwLock::default(),
            assets: RwLock::default(),
            fallback,
            schemas: RwLock::default(),
        }
    }

    pub fn add(
        &self,
        slug: String,
        name: String,
        renderer: Arc<dyn ThemeRenderer>,
    ) -> Result<(), UseCaseError> {
        if slug.is_empty()
            || !slug
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        {
            return Err(UseCaseError::Render("主题 slug 无效".into()));
        }
        let mut themes = self
            .themes
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if themes.contains_key(&slug) {
            return Err(UseCaseError::Render("主题 slug 重复".into()));
        }
        themes.insert(slug, (name, renderer));
        Ok(())
    }

    /// Publish asset bytes before making the matching renderer selectable.
    pub fn add_release(
        &self,
        name: String,
        renderer: Arc<dyn ThemeRenderer>,
        assets: ThemeAssets,
    ) -> Result<(), UseCaseError> {
        let slug = assets.slug.clone();
        let mut themes = self
            .themes
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if themes.contains_key(&slug) {
            return Err(UseCaseError::Invalid("主题已安装，请先卸载同名主题".into()));
        }
        self.assets
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(slug.clone(), assets);
        themes.insert(slug, (name, renderer));
        Ok(())
    }

    pub fn add_configured_release(
        &self,
        name: String,
        renderer: Arc<dyn ThemeRenderer>,
        assets: ThemeAssets,
        schema: Arc<crate::theme_config::ThemeSchema>,
    ) -> Result<(), UseCaseError> {
        let slug = assets.slug.clone();
        let mut themes = self
            .themes
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if themes.contains_key(&slug) {
            return Err(UseCaseError::Invalid("主题已安装，请先卸载同名主题".into()));
        }
        self.assets
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(slug.clone(), assets);
        self.schemas
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(slug.clone(), schema);
        themes.insert(slug, (name, renderer));
        Ok(())
    }

    pub fn schema(&self, slug: &str) -> Arc<crate::theme_config::ThemeSchema> {
        self.schemas
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(slug)
            .cloned()
            .unwrap_or_default()
    }

    /// Select renderer, release and contract under one registry read lock.
    pub fn snapshot(&self, requested: &str) -> Result<ThemeSnapshot, UseCaseError> {
        let themes = self
            .themes
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let slug = if themes.contains_key(requested) {
            requested
        } else {
            &self.fallback
        };
        let renderer = themes
            .get(slug)
            .ok_or_else(|| UseCaseError::Render("默认主题不可用".into()))?
            .1
            .clone();
        let assets = self
            .assets
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        Ok(ThemeSnapshot {
            slug: slug.into(),
            renderer,
            release: assets
                .get(slug)
                .map_or_else(String::new, |a| a.version.clone()),
            schema: self.schema(slug),
        })
    }

    pub fn remove(&self, slug: &str) -> Result<(), UseCaseError> {
        if slug == self.fallback {
            return Err(UseCaseError::Invalid("启动默认主题不能卸载".into()));
        }
        let mut themes = self
            .themes
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.assets
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(slug);
        self.schemas
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(slug);
        themes.remove(slug);
        Ok(())
    }

    pub fn assets(&self, slug: &str, release: &str) -> Option<ThemeAssets> {
        self.assets
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(slug)
            .filter(|assets| assets.version == release)
            .cloned()
    }

    pub fn validate(&self) -> Result<(), UseCaseError> {
        self.renderer(&self.fallback).map(|_| ())
    }

    pub fn fallback(&self) -> &str {
        &self.fallback
    }

    pub fn contains(&self, slug: &str) -> bool {
        self.themes
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains_key(slug)
    }

    pub fn renderer(&self, slug: &str) -> Result<Arc<dyn ThemeRenderer>, UseCaseError> {
        self.themes
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(slug)
            .map(|(_, renderer)| renderer.clone())
            .ok_or_else(|| UseCaseError::Render(format!("主题未安装：{slug}")))
    }

    pub fn options(&self) -> Vec<ThemeOption> {
        let themes = self
            .themes
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let assets = self
            .assets
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        themes
            .iter()
            .map(|(slug, (name, _))| ThemeOption {
                slug: slug.clone(),
                name: name.clone(),
                id: None,
                config_version: None,
                config_schema_version: None,
                release: assets
                    .get(slug)
                    .map_or_else(String::new, |assets| assets.version.clone()),
            })
            .collect()
    }
}

/// A successful preflight describes the exact checked release; failure is an error.
#[derive(Debug, Clone, Serialize)]
pub struct ThemePackageReport {
    pub slug: String,
    pub name: String,
    pub release: String,
    pub template_count: usize,
    pub asset_count: usize,
}

pub const MAX_THEME_PACKAGE_BYTES: usize = 10 * 1024 * 1024;

/// Infrastructure holds an asynchronous lock without exposing runtime types here.
pub trait ThemeOperationGuard: Send + 'static {}

#[async_trait::async_trait]
pub trait ThemePackages: Send + Sync {
    async fn lock(&self) -> Box<dyn ThemeOperationGuard>;
    async fn validate_package(&self, bytes: Vec<u8>) -> Result<ThemePackageReport, UseCaseError>;
    async fn install(
        &self,
        bytes: Vec<u8>,
        actor: crate::audit::AuditContext,
        guard: Box<dyn ThemeOperationGuard>,
    ) -> Result<ThemePackageReport, UseCaseError>;
    async fn validate_installed(&self, slug: &str) -> Result<ThemePackageReport, UseCaseError>;
    async fn uninstall(
        &self,
        slug: &str,
        actor: crate::audit::AuditContext,
        identity: ThemeUninstallIdentity,
        guard: Box<dyn ThemeOperationGuard>,
    ) -> Result<(), UseCaseError>;
}

#[derive(Debug, Clone)]
pub struct ThemeUninstallIdentity {
    pub id: uuid::Uuid,
    pub version: i64,
    pub config_schema_version: u32,
    pub release: String,
    pub selection_version: i64,
}

pub struct ThemeSnapshot {
    pub slug: String,
    pub release: String,
    pub schema: Arc<crate::theme_config::ThemeSchema>,
    pub renderer: Arc<dyn ThemeRenderer>,
}
