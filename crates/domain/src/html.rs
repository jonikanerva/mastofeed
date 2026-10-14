//! HTML escaping for values that the feed puts into item HTML.

/// Escapes `&`, `<`, `>`, and `"`. The feed puts every interpolated value
/// inside element content or a double-quoted attribute, so these four
/// characters are sufficient. The set is the same as in the TypeScript
/// service, so the item HTML does not change.
#[must_use]
pub fn escape_html(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            other => escaped.push(other),
        }
    }
    escaped
}

#[cfg(test)]
mod tests {
    use super::escape_html;

    #[test]
    fn escapes_the_four_characters() {
        assert_eq!(
            escape_html(r#"a & b < c > d " e"#),
            "a &amp; b &lt; c &gt; d &quot; e"
        );
    }

    #[test]
    fn keeps_apostrophes_and_existing_entities_literal() {
        assert_eq!(escape_html("it's &amp;"), "it's &amp;amp;");
    }

    #[test]
    fn keeps_non_ascii_text() {
        assert_eq!(escape_html("äö 😀"), "äö 😀");
    }
}
