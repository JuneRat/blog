//! Bounded public discovery; adapters apply the same visibility rule as detail pages.
use crate::{
    UseCaseError,
    ports::PublicPostSummary,
    public_site::{IndexPagination, PostCard},
};
use async_trait::async_trait;

pub const PAGE_SIZE: i64 = 20;
#[derive(Debug, Clone)]
pub enum DiscoveryFilter {
    Search(String),
    Author(String),
    Archive(Option<String>),
}
impl DiscoveryFilter {
    pub fn validate(&self) -> Result<(), UseCaseError> {
        match self {
            Self::Search(q) if q.chars().count() > 120 || q.chars().any(char::is_control) => Err(
                UseCaseError::Invalid("搜索词最多 120 个字符，不允许控制字符".into()),
            ),
            Self::Author(name) => domain::identity::normalize_username(name)
                .map(|_| ())
                .map_err(|_| UseCaseError::NotFound("作者".into())),
            Self::Archive(Some(month)) => {
                let valid = month.len() == 7
                    && month.as_bytes()[4] == b'-'
                    && month[..4].bytes().all(|c| c.is_ascii_digit())
                    && month[5..].bytes().all(|c| c.is_ascii_digit());
                if !valid
                    || !matches!(month[..4].parse::<u16>(), Ok(1..=9999))
                    || !matches!(month[5..].parse::<u8>(), Ok(1..=12))
                {
                    return Err(UseCaseError::Invalid("归档月份须为 YYYY-MM".into()));
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }
    pub fn path(&self, page: i64) -> String {
        let (path, mut query) = match self {
            Self::Search(q) => ("/search/".to_owned(), vec![("q", q.clone())]),
            Self::Author(name) => (
                format!("/authors/{}", crate::seo::encode_path_segment(name)),
                vec![],
            ),
            Self::Archive(month) => (
                "/archive/".to_owned(),
                month.iter().map(|m| ("month", m.clone())).collect(),
            ),
        };
        if page > 1 {
            query.push(("page", page.to_string()));
        }
        if query.is_empty() {
            path
        } else {
            format!(
                "{path}?{}",
                url::form_urlencoded::Serializer::new(String::new())
                    .extend_pairs(query)
                    .finish()
            )
        }
    }
}
pub struct DiscoveryRow {
    pub post: PublicPostSummary,
    pub is_page: bool,
}
#[derive(Debug, Clone, serde::Serialize)]
pub struct ArchiveMonth {
    pub month: String,
    pub count: i64,
}
#[derive(Default)]
pub struct DiscoveryPage {
    pub items: Vec<DiscoveryRow>,
    pub total: i64,
    pub months: Vec<ArchiveMonth>,
}
#[async_trait]
pub trait PublicDiscoveryQuery: Send + Sync {
    /// Results, counts, and archive months come from one read snapshot. Archive
    /// boundaries follow the supplied validated IANA site zone. No hidden counts.
    async fn list(
        &self,
        filter: &DiscoveryFilter,
        time_zone: &str,
        limit: i64,
        offset: i64,
    ) -> Result<DiscoveryPage, UseCaseError>;
}
#[derive(Debug, Clone, serde::Serialize)]
pub struct DiscoveryView {
    pub kind: &'static str,
    pub title: String,
    pub query: String,
    pub month: String,
    pub months: Vec<ArchiveMonth>,
    pub total: i64,
    pub posts: Vec<PostCard>,
    pub pagination: IndexPagination,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn validates_untrusted_months_and_encodes_pagination_without_losing_filters() {
        for value in [
            "2026-00",
            "2026-13",
            "0000-01",
            "aaaa-10",
            "中中a",
            "2026-1",
            "2026-01';",
        ] {
            assert!(
                DiscoveryFilter::Archive(Some(value.into()))
                    .validate()
                    .is_err()
            );
        }
        let filter = DiscoveryFilter::Search("中文 & 100%".into());
        assert_eq!(
            filter.path(2),
            "/search/?q=%E4%B8%AD%E6%96%87+%26+100%25&page=2"
        );
        assert_eq!(
            DiscoveryFilter::Archive(Some("2026-10".into())).path(2),
            "/archive/?month=2026-10&page=2"
        );
    }
}
