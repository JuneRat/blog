use crate::UseCaseError;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PublicRequest {
    OAuthStart,
    CommentSubmit,
    CommentPreview,
}

/// Atomic admission before expensive anonymous work. Accepted requests consume
/// capacity regardless of their eventual outcome. Each action has independent
/// client and global budgets; missing source addresses share a fallback bucket.
pub trait RequestAdmission: Send + Sync {
    fn admit(&self, action: PublicRequest, client: Option<&str>) -> Result<(), UseCaseError>;
}
