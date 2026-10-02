//! Shared publication grammar and sanitizer, including inert task-list controls.
use pulldown_cmark::Options;

pub(crate) fn options() -> Options {
    Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS
}

pub(crate) fn sanitizer<'a>() -> ammonia::Builder<'a> {
    let mut sanitizer = ammonia::Builder::default();
    // Markdown task lists generate input nodes. All author-supplied inputs are
    // likewise forced to inert checkboxes; form, value, src and event attributes
    // remain disallowed. Comments keep their separate, stricter sanitizer.
    sanitizer
        .add_tags(&["input"])
        .add_tag_attributes("input", &["checked", "type", "disabled"])
        .set_tag_attribute_value("input", "type", "checkbox")
        .set_tag_attribute_value("input", "disabled", "");
    sanitizer
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn author_inputs_cannot_become_interactive_or_submit_information() {
        let html = sanitizer().clean(
            r#"<form action="https://evil.test"><input type="password" name="password" value="secret" formaction="https://evil.test" autofocus onfocus="alert(1)"><input type="image" src="https://evil.test/pixel" checked></form>"#,
        ).to_string();
        assert_eq!(html.matches("type=\"checkbox\"").count(), 2);
        assert_eq!(html.matches("disabled=\"\"").count(), 2);
        assert_eq!(html.matches("checked").count(), 1);
        for forbidden in [
            "<form",
            "password",
            "secret",
            "evil.test",
            "autofocus",
            "onfocus",
        ] {
            assert!(!html.contains(forbidden), "{html}");
        }
    }
}
