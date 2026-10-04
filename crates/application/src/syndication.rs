//! 公开发现的数据契约与查询预算；不包含 XML 或 HTTP 编码。

use time::OffsetDateTime;

/// RSS 条目上限：feed 是「最近更新」入口，不求穷举（sitemap 才负责完整收录）。
pub const FEED_ITEM_LIMIT: i64 = 20;

/// sitemap 单文件 URL 上限（sitemap 协议约束）；超出部分需要 sitemap index，
/// 属后续范围，当前直接截断并在文档中标注。
pub const SITEMAP_URL_LIMIT: i64 = 50_000;

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

/// 已收录 `used` 条之后还剩多少条可收录（sitemap 单文件硬上限）。
///
/// 上限是**整个文件**的预算，不是每个来源各自的额度：首页 + 文章 + Page + 目录
/// 共享同一份 50,000。调用方用返回值限制所有来源查询，并在预算耗尽时跳过
/// 后续来源。
/// 已用满或超出时返回 0（不返回负数）。
pub fn remaining_slots(used: usize) -> i64 {
    (SITEMAP_URL_LIMIT as usize).saturating_sub(used) as i64
}

#[cfg(test)]
mod tests {
    use super::*;
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
