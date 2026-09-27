//! A deliberately small Markdown dialect. Never use the article renderer here.
use std::collections::{HashMap, HashSet};

use pulldown_cmark::{Event, LinkType, Options, Parser, Tag, TagEnd, html};

pub const COMMENT_RENDER_VERSION: i32 = 1;

fn web_url(raw: &str) -> bool {
    reqwest::Url::parse(raw)
        .is_ok_and(|url| matches!(url.scheme(), "http" | "https") && url.host_str().is_some())
}

fn autolink(text: &str, output: &mut Vec<Event<'static>>) {
    let mut start = 0;
    let mut cursor = 0;
    while cursor < text.len() {
        let rest = &text[cursor..];
        let boundary = cursor == 0
            || text[..cursor]
                .chars()
                .next_back()
                .is_some_and(|c| !c.is_alphanumeric() && !matches!(c, '_' | '@' | '/' | '.'));
        if boundary
            && (rest.starts_with("https://")
                || rest.starts_with("http://")
                || rest.starts_with("www."))
        {
            let length = rest
                .find(|c: char| c.is_whitespace() || matches!(c, '<' | '>' | '"' | '\''))
                .unwrap_or(rest.len());
            let mut candidate = &rest[..length];
            while let Some(last) = candidate.chars().next_back() {
                let unbalanced = match last {
                    ')' => candidate.matches(')').count() > candidate.matches('(').count(),
                    ']' => candidate.matches(']').count() > candidate.matches('[').count(),
                    '}' => candidate.matches('}').count() > candidate.matches('{').count(),
                    _ => false,
                };
                if unbalanced
                    || matches!(
                        last,
                        '.' | ',' | '!' | '?' | ':' | ';' | '。' | '，' | '！' | '？' | '；' | '：'
                    )
                {
                    candidate = &candidate[..candidate.len() - last.len_utf8()];
                } else {
                    break;
                }
            }
            let href = if candidate.starts_with("www.") {
                format!("https://{candidate}")
            } else {
                candidate.to_owned()
            };
            if web_url(&href) {
                output.push(Event::Text(text[start..cursor].to_owned().into()));
                output.push(Event::Start(Tag::Link {
                    link_type: LinkType::Autolink,
                    dest_url: reqwest::Url::parse(&href).unwrap().to_string().into(),
                    title: "".into(),
                    id: "".into(),
                }));
                output.push(Event::Text(candidate.to_owned().into()));
                output.push(Event::End(TagEnd::Link));
                cursor += candidate.len();
                start = cursor;
                continue;
            }
        }
        cursor += rest.chars().next().unwrap().len_utf8();
    }
    output.push(Event::Text(text[start..].to_owned().into()));
}

pub(crate) fn render(source: &str) -> String {
    let mut events = Vec::new();
    let mut links = Vec::new();
    let mut images = 0;
    let mut code = false;
    for event in
        pulldown_cmark::TextMergeStream::new(Parser::new_ext(source, Options::ENABLE_STRIKETHROUGH))
    {
        match event {
            Event::Start(Tag::Heading { .. }) => events.push(Event::Start(Tag::Paragraph)),
            Event::End(TagEnd::Heading(_)) => events.push(Event::End(TagEnd::Paragraph)),
            Event::Start(Tag::Image { .. }) => images += 1,
            Event::End(TagEnd::Image) => images -= 1,
            Event::Start(Tag::Link {
                link_type,
                dest_url,
                title,
                id,
            }) => {
                let allowed = images == 0 && web_url(&dest_url);
                links.push(allowed);
                if allowed {
                    events.push(Event::Start(Tag::Link {
                        link_type,
                        dest_url: dest_url.into_static(),
                        title: title.into_static(),
                        id: id.into_static(),
                    }));
                }
            }
            Event::End(TagEnd::Link) => {
                if links.pop() == Some(true) {
                    events.push(Event::End(TagEnd::Link));
                }
            }
            Event::Start(Tag::CodeBlock(kind)) => {
                code = true;
                events.push(Event::Start(Tag::CodeBlock(kind.into_static())));
            }
            Event::End(TagEnd::CodeBlock) => {
                code = false;
                events.push(Event::End(TagEnd::CodeBlock));
            }
            Event::Html(text) | Event::InlineHtml(text) => {
                events.push(Event::Text(text.into_static()))
            }
            Event::Text(text) if links.is_empty() && images == 0 && !code => {
                autolink(&text, &mut events)
            }
            Event::SoftBreak => events.push(Event::HardBreak),
            other => events.push(other.into_static()),
        }
    }
    let mut rendered = String::new();
    html::push_html(&mut rendered, events.into_iter());
    ammonia::Builder::new()
        .tags(HashSet::from([
            "p",
            "br",
            "strong",
            "em",
            "del",
            "code",
            "pre",
            "blockquote",
            "ul",
            "ol",
            "li",
            "a",
        ]))
        .generic_attributes(HashSet::new())
        .tag_attributes(HashMap::from([
            ("a", HashSet::from(["href", "title"])),
            ("ol", HashSet::from(["start"])),
        ]))
        .url_schemes(HashSet::from(["http", "https"]))
        .url_relative(ammonia::UrlRelative::Deny)
        .link_rel(Some("nofollow ugc noopener noreferrer"))
        .clean(&rendered)
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn supported_formatting_and_literal_html() {
        let html = render(
            "# Title\n\none\ntwo **bold** *em* ~~old~~ `http://code.test`\n\n> quote\n\n- list 😀\n\n```html\n<script>alert(1)</script>\n```\n\n<img src=x onerror=alert(1)>\n\n![alt](https://example.com/image)",
        );
        for expected in [
            "<p>Title</p>",
            "one<br>",
            "<strong>bold</strong>",
            "<em>em</em>",
            "<del>old</del>",
            "<blockquote>",
            "<ul>",
            "&lt;script&gt;",
            "&lt;img",
            "alt",
        ] {
            assert!(html.contains(expected), "{expected}: {html}");
        }
        for forbidden in [
            "<h1",
            "<img",
            "<script",
            "href=\"http://code.test",
            "class=",
        ] {
            assert!(!html.contains(forbidden), "{html}");
        }
    }
    #[test]
    fn links_are_http_only_and_bare_urls_do_not_nest() {
        let html = render(
            "[x](javascript:alert%281%29) [relative](/x) [mail](mailto:a@b.test) [https://label.test](https://target.test)\n\nhttps://例子.测试/a_(b). www.example.com, `https://code.test`\n\n| a | b |\n| - | - |\n| x | y |",
        );
        assert_eq!(html.matches("<a ").count(), 3, "{html}");
        assert!(
            html.contains(&format!(
                "href=\"{}\"",
                reqwest::Url::parse("https://例子.测试/a_(b)").unwrap()
            )),
            "{html}"
        );
        assert!(html.contains("href=\"https://www.example.com/\""), "{html}");
        assert!(html.contains("nofollow ugc noopener noreferrer"));
        for forbidden in [
            "javascript:",
            "mailto:",
            "href=\"/",
            "<table",
            "href=\"https://label.test",
        ] {
            assert!(!html.contains(forbidden), "{html}");
        }
    }
}
