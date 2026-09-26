//! Installed, validated themes and the persisted active selection.
use std::collections::BTreeMap;
use std::sync::Arc;

use serde::Serialize;

use crate::error::UseCaseError;
use crate::ports::ThemeRenderer;

#[derive(Debug, Clone, Serialize)]
pub struct ThemeOption {
    pub slug: String,
    pub name: String,
}

pub struct ThemeRegistry {
    themes: BTreeMap<String, (String, Arc<dyn ThemeRenderer>)>,
    fallback: String,
}

impl ThemeRegistry {
    pub fn new(fallback: String) -> Self {
        Self {
            themes: BTreeMap::new(),
            fallback,
        }
    }

    pub fn add(
        &mut self,
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
        if self.themes.contains_key(&slug) {
            return Err(UseCaseError::Render("主题 slug 重复".into()));
        }
        self.themes.insert(slug, (name, renderer));
        Ok(())
    }

    pub fn validate(&self) -> Result<(), UseCaseError> {
        self.renderer(&self.fallback).map(|_| ())
    }

    pub fn fallback(&self) -> &str {
        &self.fallback
    }

    pub fn contains(&self, slug: &str) -> bool {
        self.themes.contains_key(slug)
    }

    pub fn renderer(&self, slug: &str) -> Result<Arc<dyn ThemeRenderer>, UseCaseError> {
        self.themes
            .get(slug)
            .map(|(_, renderer)| renderer.clone())
            .ok_or_else(|| UseCaseError::Render(format!("主题未安装：{slug}")))
    }

    pub fn options(&self) -> Vec<ThemeOption> {
        self.themes
            .iter()
            .map(|(slug, (name, _))| ThemeOption {
                slug: slug.clone(),
                name: name.clone(),
            })
            .collect()
    }
}
