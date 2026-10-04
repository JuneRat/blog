//! Authorized, non-persistent Markdown preview using the publication renderer.
use std::sync::Arc;

use crate::{
    error::UseCaseError,
    identity::Actor,
    ports::{ContentRenderer, RenderedPreview},
};

pub struct ContentPreview {
    renderer: Arc<dyn ContentRenderer>,
}

impl ContentPreview {
    pub fn new(renderer: Arc<dyn ContentRenderer>) -> Self {
        Self { renderer }
    }

    /// Only renders caller-supplied text; never reads a draft, writes content or
    /// registers media references. Existing content still has its own ACL.
    pub async fn render(
        &self,
        actor: &Actor,
        source: &str,
    ) -> Result<RenderedPreview, UseCaseError> {
        if ![
            "post.create",
            "post.update",
            "post.update_any",
            "page.create",
            "page.update",
        ]
        .iter()
        .any(|permission| actor.has_permission(permission))
        {
            return Err(UseCaseError::Forbidden);
        }
        domain::content::budget::validate_source(source)
            .map_err(|error| UseCaseError::Invalid(error.to_string()))?;
        let rendered = self.renderer.render_preview(source).await?;
        crate::rendering_budget::validate_html(&rendered.content_html)
            .map_err(|error| UseCaseError::Invalid(error.to_string()))?;
        crate::rendering_budget::validate_html(&rendered.head_html)
            .map_err(|error| UseCaseError::Invalid(error.to_string()))?;
        Ok(rendered)
    }
}
