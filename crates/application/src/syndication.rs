//! RSS 2.0 与 sitemap.xml 的纯函数渲染。
//!
//! 输出不经过主题模板：feed 与 sitemap 是给机器读的协议契约，不应随主题变化，
//! 也不该让主题作者有机会产出不合规范的 XML。这一层只做「结构化数据 → 文本」，
//! 不做 IO，因此可直接单元测试转义与边界。
//!
//! 所有文本都经 [`escape_xml`]：标题/摘要来自用户输入，一个 `&` 或控制字符就能
//! 让整个 feed 无法解析——转义不是美化，是正确性。

use time::OffsetDateTime;
use time::format_description::well_known::{Rfc2822, Rfc3339};

use std::fmt::Write as _;

/// RSS 条目上限：feed 是「最近更新」入口，不求穷举（sitemap 才负责完整收录）。
pub const FEED_ITEM_LIMIT: i64 = 20;

/// sitemap 单文件 URL 上限（sitemap 协议约束）；超出部分需要 sitemap index，
/// 属后续范围，当前直接截断并在文档中标注。
pub const SITEMAP_URL_LIMIT: i64 = 50_000;

/// feed 语言标注（与主题 `lang="zh-CN"` 保持一致）。
const FEED_LANGUAGE: &str = "zh-CN";

/// 一条 RSS 条目。`url` 必须是绝对 URL，同时用作 `<link>` 与 `<guid>`。
#[derive(Debug, Clone, PartialEq)]
pub struct FeedItem {
    pub title: String,
    pub url: String,
    /// 摘要（可为空）；正文不进 feed，避免读者端与站点渲染结果不一致。
    pub description: Option<String>,
    pub published_at: Option<OffsetDateTime>,
}

/// 一个 feed 频道。`link` 是站点根（带尾斜杠），`self_url` 是 feed 自身地址。
#[derive(Debug, Clone, PartialEq)]
pub struct FeedChannel {
    pub title: String,
    pub description: String,
    pub link: String,
    pub self_url: String,
    pub items: Vec<FeedItem>,
}

/// 一条 sitemap 记录。`loc` 必须是绝对 URL。
#[derive(Debug, Clone, PartialEq)]
pub struct SitemapEntry {
    pub loc: String,
    pub lastmod: Option<OffsetDateTime>,
}

/// 渲染 RSS 2.0。
///
/// `guid` 取文章 canonical URL 且 `isPermaLink="true"`：slug 首次发布后锁定
/// （见 docs/content-lifecycle.md），因此 URL 就是该内容的稳定标识；读者端据此
/// 去重，改标题不会重复推送。
pub fn render_feed(channel: &FeedChannel) -> String {
    let mut out = String::new();
    out.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    out.push_str("<rss version=\"2.0\" xmlns:atom=\"http://www.w3.org/2005/Atom\">\n");
    out.push_str("  <channel>\n");
    push_element(&mut out, 4, "title", &channel.title);
    push_element(&mut out, 4, "link", &channel.link);
    push_element(&mut out, 4, "description", &channel.description);
    push_element(&mut out, 4, "language", FEED_LANGUAGE);
    let _ = writeln!(
        out,
        "    <atom:link href=\"{}\" rel=\"self\" type=\"application/rss+xml\"/>",
        escape_xml(&channel.self_url)
    );
    // lastBuildDate 取最新条目的发布时间，而不是「当前时间」：后者会让每次请求
    // 都产生不同输出，读者端与缓存层无法判断内容到底有没有变。
    if let Some(latest) = channel.items.iter().filter_map(|i| i.published_at).max() {
        push_element(&mut out, 4, "lastBuildDate", &rss_datetime(latest));
    }
    for item in &channel.items {
        out.push_str("    <item>\n");
        push_element(&mut out, 6, "title", &item.title);
        push_element(&mut out, 6, "link", &item.url);
        let _ = writeln!(
            out,
            "      <guid isPermaLink=\"true\">{}</guid>",
            escape_xml(&item.url)
        );
        if let Some(published_at) = item.published_at {
            push_element(&mut out, 6, "pubDate", &rss_datetime(published_at));
        }
        if let Some(description) = item
            .description
            .as_deref()
            .map(str::trim)
            .filter(|d| !d.is_empty())
        {
            push_element(&mut out, 6, "description", description);
        }
        out.push_str("    </item>\n");
    }
    out.push_str("  </channel>\n");
    out.push_str("</rss>\n");
    out
}

/// 已收录 `used` 条之后还剩多少条可收录（sitemap 单文件硬上限）。
///
/// 上限是**整个文件**的预算，不是每个来源各自的额度：首页 + 文章 + Page + 目录
/// 共享同一份 50,000。调用方用返回值限制文章与 Page 查询，并在预算耗尽时跳过
/// 后续来源；目录查询尚无 limit，有剩余名额时仍全量读取，输出由渲染层兜底截断。
/// 已用满或超出时返回 0（不返回负数）。
pub fn remaining_slots(used: usize) -> i64 {
    (SITEMAP_URL_LIMIT as usize).saturating_sub(used) as i64
}

/// 渲染 sitemap.xml（urlset，协议 0.9）。
///
/// 超过 [`SITEMAP_URL_LIMIT`] 的条目在此**兜底截断**：超限文件会被抓取器整体
/// 拒绝，少收录尾部也比产出一个非法文件好。调用方应先用 [`remaining_slots`]
/// 按预算分配（见 `PublicSiteInteractor::render_sitemap`）。
pub fn render_sitemap(entries: &[SitemapEntry]) -> String {
    let mut out = String::new();
    out.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    out.push_str("<urlset xmlns=\"http://www.sitemaps.org/schemas/sitemap/0.9\">\n");
    for entry in entries.iter().take(SITEMAP_URL_LIMIT as usize) {
        out.push_str("  <url>\n");
        push_element(&mut out, 4, "loc", &entry.loc);
        if let Some(lastmod) = entry.lastmod {
            push_element(&mut out, 4, "lastmod", &w3c_datetime(lastmod));
        }
        out.push_str("  </url>\n");
    }
    out.push_str("</urlset>\n");
    out
}

fn push_element(out: &mut String, indent: usize, name: &str, value: &str) {
    let pad = " ".repeat(indent);
    let _ = writeln!(out, "{pad}<{name}>{}</{name}>", escape_xml(value));
}

/// RSS 2.0 的 `<pubDate>` / `<lastBuildDate>` 用 RFC 822 格式。
fn rss_datetime(t: OffsetDateTime) -> String {
    t.format(&Rfc2822).unwrap_or_else(|_| t.to_string())
}

/// sitemap 的 `<lastmod>` 用 W3C datetime（RFC 3339 是它的常用子集）。
fn w3c_datetime(t: OffsetDateTime) -> String {
    t.format(&Rfc3339).unwrap_or_else(|_| t.to_string())
}

/// XML 文本转义，并丢弃 XML 1.0 不允许的控制字符。
///
/// 五个预定义实体之外的控制字符（如 `0x00`、`0x1F`）即使转义也无法出现在
/// XML 1.0 文档里，解析器会直接报错；它们只可能来自用户输入，且没有合法的
/// 表示方式，因此丢弃而不是替换。
pub fn escape_xml(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for ch in raw.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            c if is_xml_invalid(c) => {}
            c => out.push(c),
        }
    }
    out
}

/// XML 1.0 `Char` 产生式之外的码点：除制表/换行/回车以外的 C0 控制字符，
/// 以及两个永久未分配的 BMP 非字符。
fn is_xml_invalid(c: char) -> bool {
    matches!(
        c as u32,
        0x00..=0x08 | 0x0B | 0x0C | 0x0E..=0x1F | 0xFFFE | 0xFFFF
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(y: i32, m: u8, d: u8) -> OffsetDateTime {
        let date = time::Date::from_calendar_date(y, time::Month::try_from(m).unwrap(), d).unwrap();
        date.midnight().assume_utc()
    }

    fn channel(items: Vec<FeedItem>) -> FeedChannel {
        FeedChannel {
            title: "站点 & 名字".into(),
            description: "描述".into(),
            link: "https://example.com/".into(),
            self_url: "https://example.com/feed.xml".into(),
            items,
        }
    }

    #[test]
    fn escape_covers_markup_and_entities() {
        assert_eq!(
            escape_xml("a & b < c > d \" e ' f"),
            "a &amp; b &lt; c &gt; d &quot; e &apos; f"
        );
        // 已是实体的文本会被再次转义，输出仍然是「字面量 &」，不会二次解析。
        assert_eq!(escape_xml("&amp;"), "&amp;amp;");
    }

    #[test]
    fn escape_drops_xml_invalid_control_characters() {
        // 0x00–0x08 与 0x0E–0x1F 非法；\t \n \r 合法，必须保留。
        assert_eq!(escape_xml("a\u{0}b\u{1F}c"), "abc");
        assert_eq!(escape_xml("a\tb\nc\rd"), "a\tb\nc\rd");
        assert_eq!(escape_xml("emoji😀保留"), "emoji😀保留");
    }

    #[test]
    fn feed_has_stable_guid_absolute_link_and_pubdate() {
        let xml = render_feed(&channel(vec![FeedItem {
            title: "标题 & 实体".into(),
            url: "https://example.com/posts/hello".into(),
            description: Some("  摘要  ".into()),
            published_at: Some(at(2024, 3, 4)),
        }]));
        assert!(
            xml.starts_with("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<rss version=\"2.0\"")
        );
        assert!(xml.contains("<title>站点 &amp; 名字</title>"), "{xml}");
        assert!(
            xml.contains("<guid isPermaLink=\"true\">https://example.com/posts/hello</guid>"),
            "{xml}"
        );
        assert!(
            xml.contains("<link>https://example.com/posts/hello</link>"),
            "{xml}"
        );
        assert!(
            xml.contains("<pubDate>Mon, 04 Mar 2024 00:00:00 +0000</pubDate>"),
            "{xml}"
        );
        assert!(xml.contains("<description>摘要</description>"), "{xml}");
        assert!(
            xml.contains("<atom:link href=\"https://example.com/feed.xml\" rel=\"self\""),
            "{xml}"
        );
        assert!(xml.contains("<lastBuildDate>Mon, 04 Mar 2024 00:00:00 +0000</lastBuildDate>"));
        assert!(xml.ends_with("</rss>\n"));
    }

    #[test]
    fn feed_omits_missing_pubdate_and_empty_description() {
        let xml = render_feed(&channel(vec![FeedItem {
            title: "t".into(),
            url: "https://example.com/posts/t".into(),
            description: Some("   ".into()),
            published_at: None,
        }]));
        assert!(!xml.contains("<pubDate>"), "{xml}");
        assert!(!xml.contains("<description>摘要"), "{xml}");
        assert!(
            !xml.contains("<lastBuildDate>"),
            "没有条目时间就不该编造一个"
        );
    }

    #[test]
    fn empty_feed_is_still_a_valid_channel() {
        let xml = render_feed(&channel(Vec::new()));
        assert!(xml.contains("<channel>") && xml.contains("</channel>"));
        assert!(!xml.contains("<item>"));
    }

    #[test]
    fn sitemap_renders_loc_and_optional_lastmod() {
        let xml = render_sitemap(&[
            SitemapEntry {
                loc: "https://example.com/".into(),
                lastmod: None,
            },
            SitemapEntry {
                loc: "https://example.com/posts/a?x=1&y=2".into(),
                lastmod: Some(at(2024, 3, 4)),
            },
        ]);
        assert!(xml.contains("<urlset xmlns=\"http://www.sitemaps.org/schemas/sitemap/0.9\">"));
        assert!(xml.contains("<loc>https://example.com/</loc>"));
        assert!(
            xml.contains("<loc>https://example.com/posts/a?x=1&amp;y=2</loc>"),
            "loc 里的 & 必须转义：{xml}"
        );
        assert!(
            xml.contains("<lastmod>2024-03-04T00:00:00Z</lastmod>"),
            "{xml}"
        );
        // 首页无 lastmod 时不留空元素。
        let home_block = xml.split("</url>").next().unwrap();
        assert!(!home_block.contains("<lastmod>"), "{home_block}");
        assert!(xml.ends_with("</urlset>\n"));
    }

    #[test]
    fn sitemap_without_entries_closes_the_document() {
        let xml = render_sitemap(&[]);
        assert!(xml.ends_with(
            "<urlset xmlns=\"http://www.sitemaps.org/schemas/sitemap/0.9\">\n</urlset>\n"
        ));
    }

    #[test]
    fn sitemap_truncates_at_the_whole_file_url_limit() {
        // 超出上限的文件会被抓取器整体拒绝，渲染层必须兜底截断。
        let entries: Vec<SitemapEntry> = (0..SITEMAP_URL_LIMIT as usize + 3)
            .map(|i| SitemapEntry {
                loc: format!("https://example.com/p/{i}"),
                lastmod: None,
            })
            .collect();
        let xml = render_sitemap(&entries);
        assert_eq!(xml.matches("<loc>").count(), SITEMAP_URL_LIMIT as usize);
        assert!(xml.contains("<loc>https://example.com/p/49999</loc>"));
        assert!(!xml.contains("<loc>https://example.com/p/50000</loc>"));
        assert!(xml.ends_with("</urlset>\n"));
    }

    #[test]
    fn remaining_slots_is_a_whole_file_budget() {
        assert_eq!(remaining_slots(0), SITEMAP_URL_LIMIT);
        assert_eq!(remaining_slots(1), SITEMAP_URL_LIMIT - 1);
        assert_eq!(remaining_slots(SITEMAP_URL_LIMIT as usize), 0);
        assert_eq!(
            remaining_slots(SITEMAP_URL_LIMIT as usize + 10),
            0,
            "已超限返回 0，不返回负数（否则会被当作查询上限）"
        );
    }
}
