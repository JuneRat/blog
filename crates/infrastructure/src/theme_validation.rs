//! Deterministic, database-free checks using the real theme functions and executor.
use std::sync::Arc;

use application::error::UseCaseError;
use application::ports::{
    PublicCategoryRef, PublicCategorySummary, PublicPostDetail, PublicPostSummary, PublicSeriesRef,
    PublicTagRef, PublicTagSummary, PublicUrlEntry, PublishedCategoryQuery, PublishedPostQuery,
    PublishedTagQuery,
};
use application::public_site::{
    CATEGORY_PAGE_SIZE, CategoryCard, CategoryView, PageView, PostCard, PostView, SERIES_PAGE_SIZE,
    SeriesCard, SeriesPostCard, SeriesView, SiteInfo, TAG_PAGE_SIZE, TagCard, TagView,
    format_datetime,
};
use application::seo::{PublicBaseUrl, SeoMeta};
use application::theme_data::ThemeData;
use async_trait::async_trait;
use time::OffsetDateTime;

use crate::rendering::{MiniJinjaThemeRenderer, RenderingRuntime};

#[derive(Clone)]
struct Fixtures {
    count: usize,
    optional: bool,
}

impl Fixtures {
    fn summary(&self, index: usize) -> PublicPostSummary {
        PublicPostSummary {
            title: format!("示例文章 <标题> {index}"),
            slug: format!("示例-{index}"),
            excerpt: self.optional.then(|| "摘要 & 内容".into()),
            published_at: self.optional.then_some(OffsetDateTime::UNIX_EPOCH),
            author_display: "示例作者".into(),
            author_avatar_media_id: self.optional.then_some(uuid::Uuid::from_u128(1)),
        }
    }

    fn rows(&self, limit: i64, offset: i64) -> Vec<PublicPostSummary> {
        (0..self.count)
            .skip(offset as usize)
            .take(limit as usize)
            .map(|index| self.summary(index))
            .collect()
    }

    fn detail(&self, slug: &str) -> PublicPostDetail {
        let post = self.summary(0);
        PublicPostDetail {
            title: post.title,
            slug: slug.into(),
            excerpt: post.excerpt,
            published_at: post.published_at,
            updated_at: OffsetDateTime::UNIX_EPOCH,
            author_display: post.author_display,
            author_username: "author".into(),
            author_avatar_media_id: post.author_avatar_media_id,
            content_html: "<p>已清洗的示例正文</p>".into(),
            cover_media_id: self.optional.then_some(uuid::Uuid::from_u128(2)),
            tags: if self.optional {
                vec![PublicTagRef {
                    slug: "tag".into(),
                    name: "标签".into(),
                }]
            } else {
                vec![]
            },
            category: self.optional.then(|| PublicCategoryRef {
                slug: "category".into(),
                name: "分类".into(),
            }),
            series: self
                .optional
                .then(|| PublicSeriesRef {
                    slug: "series".into(),
                    name: "系列".into(),
                    position: 0,
                })
                .into_iter()
                .collect(),
        }
    }
}

#[async_trait]
impl PublishedPostQuery for Fixtures {
    async fn list_public(
        &self,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<PublicPostSummary>, UseCaseError> {
        Ok(self.rows(limit, offset))
    }
    async fn find_public_by_slug(
        &self,
        slug: &str,
    ) -> Result<Option<PublicPostDetail>, UseCaseError> {
        Ok((self.count > 0).then(|| self.detail(slug)))
    }
    async fn list_public_for_sitemap(&self, _: i64) -> Result<Vec<PublicUrlEntry>, UseCaseError> {
        Ok(vec![])
    }
}

#[async_trait]
impl PublishedCategoryQuery for Fixtures {
    async fn list_public_categories(
        &self,
        limit: i64,
    ) -> Result<Vec<PublicCategorySummary>, UseCaseError> {
        Ok((0..self.count.min(limit as usize))
            .map(|i| PublicCategorySummary {
                slug: format!("category-{i}"),
                name: format!("分类 {i}"),
            })
            .collect())
    }
    async fn find_public_by_slug(
        &self,
        slug: &str,
    ) -> Result<Option<PublicCategorySummary>, UseCaseError> {
        Ok((self.count > 0).then(|| PublicCategorySummary {
            slug: slug.into(),
            name: "分类".into(),
        }))
    }
    async fn list_public_posts_by_category(
        &self,
        _: &str,
        limit: i64,
        offset: i64,
    ) -> Result<(Vec<PublicPostSummary>, i64), UseCaseError> {
        Ok((self.rows(limit, offset), self.count as i64))
    }
    async fn list_public_directories(&self) -> Result<Vec<PublicUrlEntry>, UseCaseError> {
        Ok(vec![])
    }
}

#[async_trait]
impl PublishedTagQuery for Fixtures {
    async fn list_public_tags(&self, limit: i64) -> Result<Vec<PublicTagSummary>, UseCaseError> {
        Ok((0..self.count.min(limit as usize))
            .map(|i| PublicTagSummary {
                slug: format!("tag-{i}"),
                name: format!("标签 {i}"),
            })
            .collect())
    }
    async fn find_public_by_slug(
        &self,
        slug: &str,
    ) -> Result<Option<PublicTagSummary>, UseCaseError> {
        Ok((self.count > 0).then(|| PublicTagSummary {
            slug: slug.into(),
            name: "标签".into(),
        }))
    }
    async fn list_public_posts_by_tag(
        &self,
        _: &str,
        limit: i64,
        offset: i64,
    ) -> Result<(Vec<PublicPostSummary>, i64), UseCaseError> {
        Ok((self.rows(limit, offset), self.count as i64))
    }
    async fn list_public_directories(&self) -> Result<Vec<PublicUrlEntry>, UseCaseError> {
        Ok(vec![])
    }
}

pub(crate) async fn validate(
    renderer: &MiniJinjaThemeRenderer,
    runtime: &RenderingRuntime,
) -> Result<(), UseCaseError> {
    let base = PublicBaseUrl::parse("https://theme-check.example").expect("valid fixture URL");
    for (scenario, count, optional, page, total_pages) in [
        ("empty", 0, false, 1, 1),
        ("minimal", 1, false, 1, 1),
        ("maximum-first", 50, true, 1, 3),
        ("maximum-middle", 50, true, 2, 3),
        ("maximum-last", 50, true, 3, 3),
        ("maximum-body", 50, true, 1, 3),
    ] {
        let fixtures = Arc::new(Fixtures { count, optional });
        let data = Arc::new(ThemeData::new(
            fixtures.clone(),
            fixtures.clone(),
            fixtures.clone(),
        ));
        let theme = runtime.theme_renderer(renderer.clone().with_data(data));
        let site = SiteInfo {
            title: "主题校验 <站点>".into(),
            description: "示例描述 & 内容".into(),
            logo_url: optional.then(|| "/media/00000000-0000-0000-0000-000000000001".into()),
        };
        let posts: Vec<PostCard> = fixtures
            .rows(50, 0)
            .into_iter()
            .map(PostCard::from)
            .collect();
        let detail = fixtures.detail("示例-0");
        let mut post = PostView {
            url: application::seo::post_path(&detail.slug),
            title: detail.title,
            slug: detail.slug,
            excerpt: detail.excerpt,
            published_at: detail.published_at.map(format_datetime),
            updated_at: format_datetime(detail.updated_at),
            author_display: detail.author_display,
            author_avatar_url: detail
                .author_avatar_media_id
                .map(application::media::media_url),
            content_html: detail.content_html,
            cover_url: detail.cover_media_id.map(application::media::media_url),
            tags: detail
                .tags
                .into_iter()
                .map(|tag| TagCard {
                    slug: tag.slug,
                    name: tag.name,
                })
                .collect(),
            category: detail.category.map(|c| CategoryCard {
                slug: c.slug,
                name: c.name,
            }),
            series: detail
                .series
                .into_iter()
                .map(|s| SeriesCard {
                    slug: s.slug,
                    name: s.name,
                    position: s.position,
                })
                .collect(),
        };
        if scenario == "maximum-body" {
            post.content_html = format!(
                "<p>{}</p>",
                "x".repeat(application::rendering_budget::MAX_CONTENT_HTML_BYTES - 7)
            );
        }
        let page_view = PageView {
            title: "示例页面".into(),
            slug: "about".into(),
            published_at: post.published_at.clone(),
            updated_at: post.updated_at.clone(),
            content_html: post.content_html.clone(),
        };
        let tag = TagView {
            tag_slug: "tag".into(),
            tag_name: "标签".into(),
            page,
            total_pages,
            posts: posts.iter().take(TAG_PAGE_SIZE as usize).cloned().collect(),
        };
        let category = CategoryView {
            category_slug: "category".into(),
            category_name: "分类".into(),
            page,
            total_pages,
            posts: posts
                .iter()
                .take(CATEGORY_PAGE_SIZE as usize)
                .cloned()
                .collect(),
        };
        let series = SeriesView {
            series_slug: "series".into(),
            series_name: "系列".into(),
            cover_url: post.cover_url.clone(),
            page,
            total_pages,
            posts: posts
                .iter()
                .take(SERIES_PAGE_SIZE as usize)
                .enumerate()
                .map(|(i, card)| SeriesPostCard {
                    card: card.clone(),
                    index: (page - 1) * SERIES_PAGE_SIZE + i as i64 + 1,
                })
                .collect(),
        };
        let check =
            |entry: &str, result: Result<String, UseCaseError>| -> Result<(), UseCaseError> {
                result.map(|_| ()).map_err(|error| {
                    UseCaseError::Render(format!(
                        "主题 {} 校验失败 [{scenario}/{entry}]：{error}",
                        renderer.slug()
                    ))
                })
            };
        check(
            "index.html",
            theme
                .render_index(&site, &SeoMeta::home(&site, &base), &posts)
                .await,
        )?;
        check(
            "post.html",
            theme
                .render_post(
                    &site,
                    &SeoMeta::post(
                        &site,
                        &base,
                        &post.title,
                        &post.slug,
                        post.excerpt.as_deref(),
                    ),
                    &post,
                )
                .await,
        )?;
        check(
            "page.html",
            theme
                .render_page(
                    &site,
                    &SeoMeta::page(&site, &base, &page_view.title, &page_view.slug),
                    &page_view,
                )
                .await,
        )?;
        check(
            "tag.html",
            theme
                .render_tag(
                    &site,
                    &SeoMeta::tag(&site, &base, &tag.tag_name, &tag.tag_slug, page),
                    &tag,
                )
                .await,
        )?;
        check(
            "category.html",
            theme
                .render_category(
                    &site,
                    &SeoMeta::category(
                        &site,
                        &base,
                        &category.category_name,
                        &category.category_slug,
                        page,
                    ),
                    &category,
                )
                .await,
        )?;
        check(
            "series.html",
            theme
                .render_series(
                    &site,
                    &SeoMeta::series(&site, &base, &series.series_name, &series.series_slug, page),
                    &series,
                )
                .await,
        )?;
    }
    Ok(())
}
