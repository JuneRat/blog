//! Public-only data facade for theme functions. No Actor or management repository enters it.
use std::sync::Arc;

use serde::Serialize;

use crate::error::UseCaseError;
use crate::ports::{PublishedCategoryQuery, PublishedPostQuery, PublishedTagQuery};
use crate::public_site::format_datetime;
use domain::content::post::Slug;

/// All public article lists share one display contract.
pub type ThemePostSummary = crate::public_site::PostCard;

#[derive(Debug, Clone, Serialize)]
pub struct ThemePostDetail {
    #[serde(flatten)]
    pub summary: ThemePostSummary,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ThemeDirectory {
    pub name: String,
    pub slug: String,
    pub url: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ThemeList<T: Serialize> {
    pub items: Vec<T>,
}

pub struct ThemeData {
    posts: Arc<dyn PublishedPostQuery>,
    tags: Arc<dyn PublishedTagQuery>,
    categories: Arc<dyn PublishedCategoryQuery>,
}

impl ThemeData {
    pub fn new(
        posts: Arc<dyn PublishedPostQuery>,
        tags: Arc<dyn PublishedTagQuery>,
        categories: Arc<dyn PublishedCategoryQuery>,
    ) -> Self {
        Self {
            posts,
            tags,
            categories,
        }
    }

    /// Up to 50 published, public, non-deleted summaries. A tag and category cannot be combined.
    pub async fn get_posts(
        &self,
        limit: i64,
        tag: Option<&str>,
        category: Option<&str>,
    ) -> Result<ThemeList<ThemePostSummary>, UseCaseError> {
        if !(1..=50).contains(&limit) {
            return Err(UseCaseError::Invalid(
                "get_posts limit 必须在 1..=50".into(),
            ));
        }
        if tag.is_some() && category.is_some() {
            return Err(UseCaseError::Invalid(
                "get_posts 只能使用一个目录过滤器".into(),
            ));
        }
        for filter in [tag, category].into_iter().flatten() {
            Slug::new(filter).map_err(|e| UseCaseError::Invalid(e.to_string()))?;
        }
        let rows = if let Some(slug) = tag {
            self.tags.list_public_posts_by_tag(slug, limit, 0).await?.0
        } else if let Some(slug) = category {
            self.categories
                .list_public_posts_by_category(slug, limit, 0)
                .await?
                .0
        } else {
            self.posts.list_public(limit, 0).await?
        };
        Ok(ThemeList {
            items: rows.into_iter().map(ThemePostSummary::from).collect(),
        })
    }

    pub async fn get_post(&self, slug: &str) -> Result<Option<ThemePostDetail>, UseCaseError> {
        Slug::new(slug).map_err(|e| UseCaseError::Invalid(e.to_string()))?;
        let result = self.posts.find_public_by_slug(slug).await?;
        Ok(result.map(|p| ThemePostDetail {
            updated_at: format_datetime(p.updated_at),
            summary: ThemePostSummary {
                url: crate::seo::post_path(&p.slug),
                title: p.title,
                slug: p.slug,
                excerpt: p.excerpt,
                published_at: p.published_at.map(format_datetime),
                author_display: p.author_display,
                author_avatar_url: p.author_avatar_media_id.map(crate::media::media_url),
            },
        }))
    }

    pub async fn get_categories(
        &self,
        limit: i64,
    ) -> Result<ThemeList<ThemeDirectory>, UseCaseError> {
        if !(1..=50).contains(&limit) {
            return Err(UseCaseError::Invalid(
                "get_categories limit 必须在 1..=50".into(),
            ));
        }
        Ok(ThemeList {
            items: self
                .categories
                .list_public_categories(limit)
                .await?
                .into_iter()
                .map(|c| ThemeDirectory {
                    url: crate::seo::category_path(&c.slug, 1),
                    name: c.name,
                    slug: c.slug,
                })
                .collect(),
        })
    }

    pub async fn get_tags(&self, limit: i64) -> Result<ThemeList<ThemeDirectory>, UseCaseError> {
        if !(1..=50).contains(&limit) {
            return Err(UseCaseError::Invalid("get_tags limit 必须在 1..=50".into()));
        }
        Ok(ThemeList {
            items: self
                .tags
                .list_public_tags(limit)
                .await?
                .into_iter()
                .map(|t| ThemeDirectory {
                    url: crate::seo::tag_path(&t.slug, 1),
                    name: t.name,
                    slug: t.slug,
                })
                .collect(),
        })
    }
}
