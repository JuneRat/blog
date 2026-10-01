use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use application::{
    UseCaseError,
    content_preview::ContentPreview,
    identity::{Actor, ActorChannel},
    ports::{ContentRenderer, RenderedContent},
};
use domain::identity::{PermissionSet, UserId};
use uuid::Uuid;

struct Renderer(AtomicUsize);

#[async_trait::async_trait]
impl ContentRenderer for Renderer {
    async fn render_content(&self, source: &str) -> Result<RenderedContent, UseCaseError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(RenderedContent {
            render_version: application::ports::CONTENT_RENDER_VERSION,
            content_html: source.into(),
            media_ids: vec![Uuid::nil()],
        })
    }
}

fn actor(permissions: &[&str]) -> Actor {
    Actor::new(
        UserId(Uuid::now_v7()),
        ActorChannel::Session,
        PermissionSet::from_keys(permissions.iter().copied()),
    )
}

#[tokio::test]
async fn preview_authorizes_before_work_and_enforces_source_and_output_budgets() {
    let renderer = Arc::new(Renderer(AtomicUsize::new(0)));
    let preview = ContentPreview::new(renderer.clone());
    for permissions in [vec![], vec!["settings.manage"], vec!["post.read_any"]] {
        assert!(matches!(
            preview.render(&actor(&permissions), "draft").await,
            Err(UseCaseError::Forbidden)
        ));
    }
    assert_eq!(renderer.0.load(Ordering::SeqCst), 0);
    assert!(matches!(
        preview
            .render(
                &actor(&["post.create"]),
                &"x".repeat(domain::content::budget::MAX_SOURCE_BYTES + 1)
            )
            .await,
        Err(UseCaseError::Invalid(_))
    ));
    assert_eq!(renderer.0.load(Ordering::SeqCst), 0);
    assert!(matches!(
        preview
            .render(
                &actor(&["post.create"]),
                &"x".repeat(application::rendering_budget::MAX_CONTENT_HTML_BYTES + 1)
            )
            .await,
        Err(UseCaseError::Invalid(_))
    ));
    for permission in [
        "post.create",
        "post.update",
        "post.update_any",
        "page.create",
        "page.update",
    ] {
        assert_eq!(
            preview
                .render(&actor(&[permission]), "rendered")
                .await
                .unwrap()
                .content_html,
            "rendered"
        );
    }
}
