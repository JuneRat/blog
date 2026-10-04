//! RSS、sitemap 与 robots 的协议编码；只消费应用层数据，不读取数据库或主题。

use application::syndication::{FeedChannel, SITEMAP_URL_LIMIT, SitemapEntry};
use std::fmt::Write as _;
use time::OffsetDateTime;
use time::format_description::well_known::{Rfc2822, Rfc3339};

const FEED_LANGUAGE: &str = "zh-CN";

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

/// 渲染 sitemap.xml（urlset，协议 0.9）。
///
/// 超过 [`SITEMAP_URL_LIMIT`] 的条目在此**兜底截断**：超限文件会被抓取器整体
/// 拒绝，少收录尾部也比产出一个非法文件好。调用方应先用 [`application::syndication::remaining_slots`]
/// 按预算分配（见 `PublicSiteInteractor::sitemap_entries`）。
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
fn escape_xml(raw: &str) -> String {
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

pub fn render_robots(sitemap_url: &str) -> String {
    format!(
        "User-agent: *\n\
             Allow: /\n\
             Disallow: /admin\n\
             Disallow: /api\n\
             Disallow: /auth\n\
             \n\
             Sitemap: {}\n",
        sitemap_url
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use application::syndication::FeedItem;

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
}
