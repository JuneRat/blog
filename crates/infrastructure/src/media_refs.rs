//! 从正文提取站内媒体引用。
//!
//! 只在**保存内容时**运行一次：把正文里会渲染成图片的站内地址固化成
//! `media_refs` 关系。物理清理依据关系表保护站内引用，不回头搜索文本。
//!
//! # 为什么以「清洗后的 HTML」为准
//!
//! 提取与渲染必须给出**同一个集合**——两者一旦漂移，无论偏向哪边都是线上故障：
//!
//! - 漏掉引用 → 图片仍在展示却能被删除（破图）；
//! - 多出引用 → 图片永远删不掉（幽灵占用）。
//!
//! 因此这里不再自己解析 Markdown 事件或扫描原文本，而是直接走**主题渲染管线**
//! （Markdown → 清洗 HTML），再用 HTML5 分词器读出 `<img src>`：
//! 渲染成 `<img>` 的才算引用，被清洗掉的自然不算。这样「能渲染出来」与
//! 「建立引用」在结构上就是同一件事，不存在需要靠测试去追的两条路径。
//!
//! 手写字符串扫描在下面两类输入上都会判断错（都已作为回归用例固定）：
//!
//! - `<!-- <img src="/media/{id}"> -->`：注释会被清洗掉、图片不渲染，
//!   扫描器却建立引用，图片从此删不掉；
//! - `<img alt=">" src="/media/{id}">`：属性值里的 `>` 并不结束标签，
//!   图片正常渲染，扫描器却在 `>` 处截断而漏掉引用，图片仍可能被删除。
//!
//! 分词器天然处理注释、CDATA、引号属性、实体转义与 `script`/`style` 原始文本，
//! 这些规则不需要（也不应该）由本模块自己维护。

use std::cell::RefCell;
use std::collections::BTreeSet;

use html5ever::tendril::StrTendril;
use html5ever::tokenizer::{
    BufferQueue, TagKind, Token, TokenSink, TokenSinkResult, Tokenizer, TokenizerOpts,
};
use uuid::Uuid;

/// 站内媒体地址前缀，与 `application::media::MEDIA_URL_PREFIX` 一致。
const MEDIA_URL_PREFIX: &str = "/media/";

/// 提取正文中会渲染成站内图片的媒体 id（去重、按 id 升序）。
///
/// 只认 `<img src>`：正文里作为普通链接或纯文本出现的 `/media/...` 不会渲染成图片，
/// 也就不需要保证文件可读。
#[cfg(test)]
fn extract_media_ids(markdown: &str) -> Vec<Uuid> {
    let html = crate::rendering::SanitizingMarkdownRenderer::new().render_markdown(markdown);
    extract_media_ids_from_html(&html)
}

/// 从已清洗、将与正文一起落库的 HTML 中提取图片引用，不重复渲染。
pub(crate) fn extract_media_ids_from_html(html: &str) -> Vec<Uuid> {
    let mut ids = BTreeSet::new();
    for src in img_srcs_in_html(html) {
        if let Some(id) = parse_media_url(&src) {
            ids.insert(id);
        }
    }
    ids.into_iter().collect()
}

/// 解析 `/media/<uuid>`。
///
/// 只接受恰好一个路径片段且为合法 UUID：插入功能生成的地址就是这个形状，
/// 允许额外后缀或查询串会让「引用的是哪个资产」产生歧义。
fn parse_media_url(url: &str) -> Option<Uuid> {
    let rest = url.strip_prefix(MEDIA_URL_PREFIX)?;
    Uuid::parse_str(rest).ok()
}

/// 收集 `<img>` 起始标签 `src` 属性的 sink。
///
/// 分词器不会把注释内容、`<script>` 文本或属性值里的 `>` 当作标签边界，
/// 因此这里只看 `TagToken` 就够了，不需要额外的状态机。
#[derive(Default)]
struct ImgSrcSink {
    srcs: RefCell<Vec<StrTendril>>,
}

impl TokenSink for ImgSrcSink {
    type Handle = ();

    fn process_token(&self, token: Token, _line_number: u64) -> TokenSinkResult<()> {
        if let Token::TagToken(tag) = token
            && tag.kind == TagKind::StartTag
            && &*tag.name == "img"
            && let Some(src) = tag
                .attrs
                .iter()
                .find(|attribute| &*attribute.name.local == "src")
        {
            self.srcs.borrow_mut().push(src.value.clone());
        }
        TokenSinkResult::Continue
    }
}

/// 用 HTML5 分词器读出清洗后 HTML 里全部 `<img src>`。
///
/// 输入已经是 ammonia 的输出（不含 `script`/`style`），因此不会进入需要树构建器
/// 配合的 RAWTEXT/PLAINTEXT 状态，一次 `feed` + `end` 即可覆盖整个文档。
fn img_srcs_in_html(html: &str) -> Vec<StrTendril> {
    let tokenizer = Tokenizer::new(ImgSrcSink::default(), TokenizerOpts::default());
    let input = BufferQueue::default();
    input.push_back(StrTendril::from(html));
    let _ = tokenizer.feed(&input);
    tokenizer.end();
    tokenizer.sink.srcs.take()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(byte: u8) -> Uuid {
        let mut bytes = [0u8; 16];
        bytes[15] = byte;
        Uuid::from_bytes(bytes)
    }

    /// 正文经渲染 + 清洗后，是否真的会出现指向该 id 的 `<img>`。
    ///
    /// 这是「提取为什么是这个结果」的直接证据：断言它，就等于断言引用跟随渲染，
    /// 而不是跟随正文里恰好写着的字符。
    fn renders_image_for(markdown: &str, media: Uuid) -> bool {
        let html = crate::rendering::SanitizingMarkdownRenderer::new().render_markdown(markdown);
        img_srcs_in_html(&html)
            .iter()
            .any(|src| parse_media_url(src) == Some(media))
    }

    #[test]
    fn extracts_markdown_image_targets_only() {
        let a = id(1);
        let b = id(2);
        let markdown = format!(
            "开头\n\n![替代文字](/media/{a})\n\n[普通链接](/media/{b})\n\n`![代码块](/media/{b})`\n\n![again](/media/{a} \"标题\")\n"
        );
        assert_eq!(
            extract_media_ids(&markdown),
            vec![a],
            "只有图片语法的目标是引用；链接、行内代码与重复项都不重复计入"
        );
        assert!(renders_image_for(&markdown, a));
        assert!(!renders_image_for(&markdown, b), "链接不渲染成图片");
    }

    #[test]
    fn extracts_raw_html_image_targets() {
        let a = id(3);
        let b = id(4);
        let c = id(5);
        let markdown = format!(
            "<p>正文</p>\n\n<img src=\"/media/{a}\" alt=\"双引号属性\">\n\n行内 <img alt='x' src='/media/{b}'> 继续\n\n<img src=/media/{c}>\n"
        );
        assert_eq!(
            extract_media_ids(&markdown),
            vec![a, b, c],
            "原始 HTML 的 img src 同样必须建立引用（渲染器会把它渲染出来）"
        );
    }

    /// 回归：注释里的 `<img>` 不会渲染，因此绝不能建立引用。
    ///
    /// 手写扫描在这里会错：它看见 `<img` 就当图片，结果图片被注释掉却仍被占用，
    /// 永远无法删除。
    #[test]
    fn comment_wrapped_image_is_not_a_reference() {
        let a = id(6);
        let markdown = format!("正文\n\n<!-- <img src=\"/media/{a}\"> -->\n\n结尾\n");
        assert!(
            !renders_image_for(&markdown, a),
            "注释会被清洗掉，不会渲染成图片"
        );
        assert!(
            extract_media_ids(&markdown).is_empty(),
            "注释里的图片不得建立引用，否则图片永远删不掉"
        );
    }

    /// 回归：属性值里的 `>` 不结束标签，图片照常渲染，因此必须建立引用。
    ///
    /// 手写扫描在这里会错：它在第一个 `>` 处截断标签，漏掉后面的 `src`，
    /// 结果图片仍在页面上展示却可以被删除。
    #[test]
    fn attribute_value_containing_gt_does_not_truncate_the_tag() {
        let a = id(7);
        let markdown = format!("正文\n\n<img alt=\">\" src=\"/media/{a}\">\n");
        assert!(
            renders_image_for(&markdown, a),
            "属性值里的 `>` 属于属性值，标签照常结束并渲染"
        );
        assert_eq!(
            extract_media_ids(&markdown),
            vec![a],
            "属性值含 `>` 的图片必须建立引用，否则会被误删"
        );

        // 同一形状的单引号写法。
        let b = id(8);
        let single = format!("<img alt='>' src='/media/{b}'>\n");
        assert_eq!(extract_media_ids(&single), vec![b]);
    }

    /// 引用跟随渲染：被清洗策略丢掉的图片不建立引用，保留的建立引用。
    #[test]
    fn extraction_follows_the_sanitizer_not_the_raw_text() {
        let dropped = id(9);
        let kept = id(10);
        // script 会连同内容一起被清洗掉。
        let scripted = format!("<script>var s = '<img src=\"/media/{dropped}\">';</script>\n");
        assert!(!renders_image_for(&scripted, dropped));
        assert!(extract_media_ids(&scripted).is_empty());

        // javascript: 的 src 会被清洗策略丢弃。
        let hostile = format!("<img src=\"javascript:/media/{dropped}\">\n");
        assert!(!renders_image_for(&hostile, dropped));
        assert!(extract_media_ids(&hostile).is_empty());

        // 同一条正文里被保留的那张必须仍然被引用。
        let mixed = format!("<script>x</script>\n\n<img src=\"/media/{kept}\">\n");
        assert_eq!(extract_media_ids(&mixed), vec![kept]);
    }

    #[test]
    fn html_inside_a_code_block_is_not_a_reference() {
        let markdown = format!("```html\n<img src=\"/media/{}\">\n```\n", id(11));
        assert!(
            extract_media_ids(&markdown).is_empty(),
            "代码块里的示例不会被渲染成图片，不能建立引用"
        );
    }

    #[test]
    fn ignores_non_images_and_malformed_urls() {
        let markdown = concat!(
            "<a href=\"/media/00000000-0000-0000-0000-000000000001\">链接</a>\n",
            "<img data-src=\"/media/00000000-0000-0000-0000-000000000003\">\n",
            "<img src=\"/media/not-a-uuid\">\n",
            "<img src=\"/media/00000000-0000-0000-0000-000000000004?size=large\">\n",
            "<img src=\"https://cdn.example.com/media/00000000-0000-0000-0000-000000000005\">\n",
            "<img src=\"/assets/theme/photo.png\">\n",
        );
        assert!(
            extract_media_ids(markdown).is_empty(),
            "只认 <img> 的精确站内 src：链接、data-src、畸形值与外部地址都不算"
        );
    }

    /// 过时的 `<image>` 标签按规范等同于 `<img>`，**确实会渲染**，因此必须建立引用。
    ///
    /// 这条曾经写成「不建立引用」——那是照着旧的手写扫描器的行为写的，而不是照着
    /// 渲染结果写的。清洗后的 HTML 已经是 `<img>`，所以按渲染为准就必须计入。
    #[test]
    fn obsolete_image_tag_is_treated_as_img_because_it_renders() {
        let a = id(13);
        let markdown = format!("<image src=\"/media/{a}\">\n");
        assert!(
            renders_image_for(&markdown, a),
            "HTML5 规定 <image> 按 <img> 处理，ammonia 输出即为 <img>"
        );
        assert_eq!(extract_media_ids(&markdown), vec![a]);
    }

    #[test]
    fn attribute_name_matching_is_case_insensitive_like_html() {
        let a = id(12);
        let markdown = format!("<IMG SRC=\"/media/{a}\">\n");
        assert_eq!(
            extract_media_ids(&markdown),
            vec![a],
            "HTML 的标签名与属性名大小写不敏感，分词器已按规范处理"
        );
    }

    #[test]
    fn returns_sorted_deduped_ids() {
        let (a, b, c) = (id(23), id(20), id(21));
        let markdown = format!(
            "![1](/media/{a})\n<img src=\"/media/{b}\">\n![3](/media/{c})\n![4](/media/{a})\n"
        );
        let ids = extract_media_ids(&markdown);
        assert_eq!(ids.len(), 3);
        let mut sorted = ids.clone();
        sorted.sort();
        assert_eq!(ids, sorted, "输出必须已排序，便于仓储按同一顺序校验");
    }
}
