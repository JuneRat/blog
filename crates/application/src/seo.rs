//! 站点公开地址与页面 SEO 元数据：canonical / title / description 的唯一规则来源。
//!
//! 站点公开地址来自**可信配置**（`BLOG_PUBLIC_BASE_URL`），绝不用请求的 Host 头
//! 推导：Host 由客户端控制，一旦把它写进 canonical、RSS 链接与 sitemap，抓取器
//! 就会索引到伪造域名。配置在装配期校验一次（绝对 http/https、无凭据、无路径
//! 前缀、无查询与片段），之后全进程复用。
//!
//! 标题与描述的规则集中在这里，而不是散落在各主题模板里：所有页面（首页、文章、
//! Page、标签/分类/系列列表）都必须产出同构的一对 title/description，模板只负责
//! 渲染。描述的空白会被折叠成单行并截断，避免换行与超长内容进入 `<meta>`。

use std::fmt::Write as _;

use url::{Position, Url};

use crate::site_info::SiteInfo;

/// `<meta name="description">` 的字符上限（超出截断并加省略号）。
pub const META_DESCRIPTION_MAX_CHARS: usize = 160;

/// 站点公开基础 URL（绝对 http/https、无凭据、无路径前缀、无查询/片段）。
///
/// 路径前缀（`https://example.com/blog`）当前**明确不支持**：站点路由注册在域名
/// 根（`/posts/{slug}`、`/assets/...`、`/admin`），主题里的链接也都是根相对路径，
/// 接受前缀只会产出一半带前缀、一半不带的地址，反而更难排查。要支持它必须同时
/// 让路由、模板链接与后台 SPA 都跟随前缀，属独立范围；在那之前拒绝比半支持诚实。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublicBaseUrl(String);

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum PublicBaseUrlError {
    #[error("站点公开地址不能为空")]
    Empty,
    #[error("站点公开地址必须是绝对 http/https URL（当前：{0}）")]
    NotAbsolute(String),
    #[error("站点公开地址不能带查询或片段（当前：{0}）")]
    QueryOrFragment(String),
    #[error("站点公开地址不能带用户名或密码（当前：{0}）")]
    Credentials(String),
    #[error("站点公开地址暂不支持路径前缀（当前：{0}）；请使用独立域名并部署在对外域名根路径")]
    PathPrefix(String),
}

impl PublicBaseUrl {
    /// 解析并规范化；业务限制在解析结果上逐条检查。
    ///
    /// 语法交给 `url` crate：手写前缀切片既会接受 `https://example.com:abc`
    /// 这类非法端口，又会在 `http://例子.测试` 上按字节切开多字节字符而 panic。
    /// 解析器还会顺带规范化——host 转小写、Unicode 域名转 Punycode、路径按需
    /// 百分号编码，产出可直接进入 canonical、RSS 与 sitemap 的地址。
    pub fn parse(raw: &str) -> Result<Self, PublicBaseUrlError> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Err(PublicBaseUrlError::Empty);
        }
        let parsed = Url::parse(trimmed)
            .map_err(|_| PublicBaseUrlError::NotAbsolute(trimmed.to_string()))?;
        if !matches!(parsed.scheme(), "http" | "https") {
            return Err(PublicBaseUrlError::NotAbsolute(trimmed.to_string()));
        }
        if parsed.host_str().is_none() {
            return Err(PublicBaseUrlError::NotAbsolute(trimmed.to_string()));
        }
        if parsed.query().is_some() || parsed.fragment().is_some() {
            return Err(PublicBaseUrlError::QueryOrFragment(trimmed.to_string()));
        }
        // 凭据会被原样写进公开的 canonical/feed/sitemap，等于对外泄露。
        if !parsed.username().is_empty() || parsed.password().is_some() {
            return Err(PublicBaseUrlError::Credentials(trimmed.to_string()));
        }
        let path = parsed.path().trim_end_matches('/');
        if !path.is_empty() {
            return Err(PublicBaseUrlError::PathPrefix(trimmed.to_string()));
        }
        // 只取 scheme + authority：路径前缀已拒绝，尾斜杠由 root()/join() 决定。
        Ok(Self(parsed[..Position::BeforePath].to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// 站点根 URL（带尾斜杠），RSS channel 的 `<link>` 使用。
    pub fn root(&self) -> String {
        format!("{}/", self.0)
    }

    /// 拼接绝对 URL；`path` 必须以 `/` 开头且已编码。
    pub fn join(&self, path: &str) -> String {
        debug_assert!(path.starts_with('/'), "路径必须以 / 开头：{path}");
        format!("{}{}", self.0, path)
    }
}

/// 路径片段百分号编码：只保留 RFC 3986 unreserved 字符，其余按 UTF-8 字节编码。
///
/// slug 允许 Unicode 字母数字（见 `domain::content::Slug`），所以
/// `/posts/关于` 进入 canonical、RSS 与 sitemap 前必须变成 `%E5%85%B3%E4%BA%8E`：
/// sitemap 协议要求 `<loc>` 是转义后的 URL，不能依赖抓取器的 IRI 宽松解析。
pub fn encode_path_segment(segment: &str) -> String {
    let mut out = String::with_capacity(segment.len());
    for &byte in segment.as_bytes() {
        let unreserved = byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~');
        if unreserved {
            out.push(byte as char);
        } else {
            let _ = write!(out, "%{byte:02X}");
        }
    }
    out
}

/// 分页只对第 2 页起加查询串：第 1 页与列表根地址是同一个 URL，避免重复内容。
fn with_page(path: String, page: i64) -> String {
    if page > 1 {
        format!("{path}?page={page}")
    } else {
        path
    }
}

pub fn post_path(slug: &str) -> String {
    format!("/posts/{}", encode_path_segment(slug))
}

pub fn page_path(slug: &str) -> String {
    format!("/{}", encode_path_segment(slug))
}

pub fn tag_path(slug: &str, page: i64) -> String {
    with_page(format!("/tags/{}", encode_path_segment(slug)), page)
}

pub fn category_path(slug: &str, page: i64) -> String {
    with_page(format!("/categories/{}", encode_path_segment(slug)), page)
}

pub fn series_path(slug: &str, page: i64) -> String {
    with_page(format!("/series/{}", encode_path_segment(slug)), page)
}

pub fn post_url(base: &PublicBaseUrl, slug: &str) -> String {
    base.join(&post_path(slug))
}

pub fn page_url(base: &PublicBaseUrl, slug: &str) -> String {
    base.join(&page_path(slug))
}

pub fn tag_url(base: &PublicBaseUrl, slug: &str, page: i64) -> String {
    base.join(&tag_path(slug, page))
}

pub fn category_url(base: &PublicBaseUrl, slug: &str, page: i64) -> String {
    base.join(&category_path(slug, page))
}

pub fn series_url(base: &PublicBaseUrl, slug: &str, page: i64) -> String {
    base.join(&series_path(slug, page))
}

pub fn feed_url(base: &PublicBaseUrl) -> String {
    base.join("/feed.xml")
}

pub fn sitemap_url(base: &PublicBaseUrl) -> String {
    base.join("/sitemap.xml")
}

/// 一次渲染的 SEO 元数据（序列化进主题模板）。
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct SeoMeta {
    /// 完整 `<title>`（详情页为「页面标题 - 站点标题」，首页只有站点标题）。
    pub title: String,
    /// 单行、截断后的 `<meta name="description">`。
    pub description: String,
    /// 绝对 canonical URL（分页页含 `?page=N`）。
    pub canonical_url: String,
    /// RSS 自动发现链接（全站一致）。
    pub feed_url: String,
    /// Open Graph 类型：文章/Page 为 `article`，其余为 `website`。
    pub og_type: &'static str,
}

impl SeoMeta {
    fn build(
        site: &SiteInfo,
        base: &PublicBaseUrl,
        page_title: Option<&str>,
        description: Option<&str>,
        path: &str,
        og_type: &'static str,
    ) -> Self {
        let title = match page_title.map(str::trim).filter(|t| !t.is_empty()) {
            Some(t) => format!("{t} - {}", site.title),
            None => site.title.clone(),
        };
        let raw_description = description
            .map(str::trim)
            .filter(|d| !d.is_empty())
            .unwrap_or(site.description.as_str());
        Self {
            title,
            description: meta_description(raw_description),
            canonical_url: base.join(path),
            feed_url: feed_url(base),
            og_type,
        }
    }

    /// 首页：标题即站点标题，描述取站点描述。
    pub fn home(site: &SiteInfo, base: &PublicBaseUrl) -> Self {
        Self::build(site, base, None, None, "/", "website")
    }

    /// 文章详情：描述优先摘要，缺失时回退站点描述。
    pub fn post(
        site: &SiteInfo,
        base: &PublicBaseUrl,
        title: &str,
        slug: &str,
        excerpt: Option<&str>,
    ) -> Self {
        Self::build(
            site,
            base,
            Some(title),
            excerpt,
            &post_path(slug),
            "article",
        )
    }

    /// Page 详情：Page 没有摘要字段，描述回退站点描述。
    pub fn page(site: &SiteInfo, base: &PublicBaseUrl, title: &str, slug: &str) -> Self {
        Self::build(site, base, Some(title), None, &page_path(slug), "article")
    }

    pub fn tag(site: &SiteInfo, base: &PublicBaseUrl, name: &str, slug: &str, page: i64) -> Self {
        Self::build(
            site,
            base,
            Some(name),
            None,
            &tag_path(slug, page),
            "website",
        )
    }

    pub fn category(
        site: &SiteInfo,
        base: &PublicBaseUrl,
        name: &str,
        slug: &str,
        page: i64,
    ) -> Self {
        Self::build(
            site,
            base,
            Some(name),
            None,
            &category_path(slug, page),
            "website",
        )
    }

    pub fn series(
        site: &SiteInfo,
        base: &PublicBaseUrl,
        name: &str,
        slug: &str,
        page: i64,
    ) -> Self {
        Self::build(
            site,
            base,
            Some(name),
            None,
            &series_path(slug, page),
            "website",
        )
    }
}

/// 折叠空白并截断到上限：`<meta>` 内容必须是单行，超长描述没有 SEO 价值。
fn meta_description(raw: &str) -> String {
    let collapsed = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut chars = collapsed.chars();
    let head: String = chars.by_ref().take(META_DESCRIPTION_MAX_CHARS).collect();
    if chars.next().is_some() {
        format!("{head}…")
    } else {
        head
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn site() -> SiteInfo {
        SiteInfo {
            time_zone: "UTC".into(),
            title: "站点名".into(),
            description: "站点描述".into(),
            logo_url: None,
        }
    }

    fn base() -> PublicBaseUrl {
        PublicBaseUrl::parse("https://example.com").unwrap()
    }

    #[test]
    fn base_url_requires_absolute_http_scheme() {
        assert_eq!(
            PublicBaseUrl::parse("  ").unwrap_err(),
            PublicBaseUrlError::Empty
        );
        assert_eq!(
            PublicBaseUrl::parse("example.com").unwrap_err(),
            PublicBaseUrlError::NotAbsolute("example.com".into())
        );
        assert_eq!(
            PublicBaseUrl::parse("ftp://example.com").unwrap_err(),
            PublicBaseUrlError::NotAbsolute("ftp://example.com".into())
        );
        assert_eq!(
            PublicBaseUrl::parse("http://").unwrap_err(),
            PublicBaseUrlError::NotAbsolute("http://".into())
        );
        assert_eq!(
            PublicBaseUrl::parse("http://:8080").unwrap_err(),
            PublicBaseUrlError::NotAbsolute("http://:8080".into())
        );
        assert_eq!(
            PublicBaseUrl::parse("https://example.com/a?b=1").unwrap_err(),
            PublicBaseUrlError::QueryOrFragment("https://example.com/a?b=1".into())
        );
        // 非法端口不能通过：否则 `https://example.com:abc` 会一路写进 canonical/feed/sitemap。
        assert_eq!(
            PublicBaseUrl::parse("https://example.com:abc").unwrap_err(),
            PublicBaseUrlError::NotAbsolute("https://example.com:abc".into())
        );
        assert_eq!(
            PublicBaseUrl::parse("https://example.com:99999").unwrap_err(),
            PublicBaseUrlError::NotAbsolute("https://example.com:99999".into())
        );
    }

    #[test]
    fn base_url_rejects_credentials_and_path_prefix() {
        // 凭据会被原样写进公开地址，等于对外泄露。
        assert_eq!(
            PublicBaseUrl::parse("https://user:secret@example.com").unwrap_err(),
            PublicBaseUrlError::Credentials("https://user:secret@example.com".into())
        );
        assert_eq!(
            PublicBaseUrl::parse("https://user@example.com").unwrap_err(),
            PublicBaseUrlError::Credentials("https://user@example.com".into())
        );
        // 路径前缀会让模板里的根相对链接跳出站点，当前明确拒绝而不是半支持。
        assert_eq!(
            PublicBaseUrl::parse("https://example.com/blog").unwrap_err(),
            PublicBaseUrlError::PathPrefix("https://example.com/blog".into())
        );
        assert_eq!(
            PublicBaseUrl::parse("https://example.com/blog/").unwrap_err(),
            PublicBaseUrlError::PathPrefix("https://example.com/blog/".into())
        );
    }

    #[test]
    fn base_url_normalizes_scheme_host_case_and_trailing_slashes() {
        // `url` 解析器负责规范化：scheme 与 host 转小写（host 大小写不敏感）。
        assert_eq!(
            PublicBaseUrl::parse("HTTPS://Example.com//")
                .unwrap()
                .as_str(),
            "https://example.com"
        );
        assert_eq!(base().root(), "https://example.com/");
        assert_eq!(
            base().join("/posts/hello"),
            "https://example.com/posts/hello"
        );
        // 非默认端口必须保留：开发与自托管环境依赖它。
        assert_eq!(
            PublicBaseUrl::parse("http://127.0.0.1:8080/")
                .unwrap()
                .join("/feed.xml"),
            "http://127.0.0.1:8080/feed.xml"
        );
    }

    #[test]
    fn base_url_handles_unicode_hosts_without_panicking() {
        // 手写字节切片会在第一个多字节字符处 panic；交给解析器则得到 Punycode。
        let parsed = PublicBaseUrl::parse("http://例子.测试").unwrap();
        assert_eq!(parsed.as_str(), "http://xn--fsqu00a.xn--0zwm56d");
        assert_eq!(
            parsed.join("/feed.xml"),
            "http://xn--fsqu00a.xn--0zwm56d/feed.xml"
        );
        // 多字节 scheme 前缀同样不能 panic。
        assert!(PublicBaseUrl::parse("ｈｔｔｐ://example.com").is_err());
    }

    #[test]
    fn path_segments_are_percent_encoded() {
        assert_eq!(encode_path_segment("about-us_1"), "about-us_1");
        assert_eq!(encode_path_segment("关于"), "%E5%85%B3%E4%BA%8E");
        assert_eq!(post_path("关于"), "/posts/%E5%85%B3%E4%BA%8E");
        assert_eq!(tag_path("rust", 1), "/tags/rust");
        assert_eq!(tag_path("rust", 2), "/tags/rust?page=2");
        assert_eq!(
            post_url(&base(), "关于"),
            "https://example.com/posts/%E5%85%B3%E4%BA%8E"
        );
    }

    #[test]
    fn home_meta_uses_site_title_and_description() {
        let seo = SeoMeta::home(&site(), &base());
        assert_eq!(seo.title, "站点名");
        assert_eq!(seo.description, "站点描述");
        assert_eq!(seo.canonical_url, "https://example.com/");
        assert_eq!(seo.feed_url, "https://example.com/feed.xml");
        assert_eq!(seo.og_type, "website");
    }

    #[test]
    fn detail_meta_suffixes_site_title_and_prefers_excerpt() {
        let post = SeoMeta::post(
            &site(),
            &base(),
            "一篇文章",
            "hello",
            Some("  摘要\n第二行  "),
        );
        assert_eq!(post.title, "一篇文章 - 站点名");
        assert_eq!(post.description, "摘要 第二行");
        assert_eq!(post.canonical_url, "https://example.com/posts/hello");
        assert_eq!(post.og_type, "article");

        // 摘要缺失 → 回退站点描述，不留空 description。
        let page = SeoMeta::page(&site(), &base(), "关于", "about");
        assert_eq!(page.description, "站点描述");
        assert_eq!(page.canonical_url, "https://example.com/about");
    }

    #[test]
    fn listing_meta_canonicalizes_each_page() {
        let tag = SeoMeta::tag(&site(), &base(), "Rust", "rust", 3);
        assert_eq!(tag.title, "Rust - 站点名");
        assert_eq!(tag.canonical_url, "https://example.com/tags/rust?page=3");
        assert_eq!(
            SeoMeta::tag(&site(), &base(), "Rust", "rust", 1).canonical_url,
            "https://example.com/tags/rust"
        );
        assert_eq!(
            SeoMeta::category(&site(), &base(), "工程", "eng", 2).canonical_url,
            "https://example.com/categories/eng?page=2"
        );
        assert_eq!(
            SeoMeta::series(&site(), &base(), "系列", "s", 1).canonical_url,
            "https://example.com/series/s"
        );
    }

    #[test]
    fn description_is_single_line_and_truncated() {
        let long = "字".repeat(META_DESCRIPTION_MAX_CHARS + 5);
        let seo = SeoMeta::post(&site(), &base(), "t", "s", Some(&long));
        let expected: String = "字".repeat(META_DESCRIPTION_MAX_CHARS);
        assert_eq!(seo.description, format!("{expected}…"));
        assert_eq!(
            seo.description.chars().count(),
            META_DESCRIPTION_MAX_CHARS + 1,
            "省略号额外占一个字符"
        );

        // 恰好在上限内不截断（边界）。
        let exact = "字".repeat(META_DESCRIPTION_MAX_CHARS);
        let seo = SeoMeta::post(&site(), &base(), "t", "s", Some(&exact));
        assert_eq!(seo.description, exact);
    }
}
