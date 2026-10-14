//! The camelCase key conversion of masto.js.
//!
//! The TypeScript service stored each status with the keys that masto.js
//! produced: `change-case` 5 `camelCase` on every object key, except keys
//! that contain `:` or start with `_`. The feed and the change guard read
//! the stored keys, so the Rust service stores the same keys (docs/adr/0001).
//!
//! The word rules follow `change-case` exactly for ASCII keys. For other
//! characters, the Unicode properties of the standard library approximate
//! the JavaScript general categories. Mastodon keys are ASCII.

use serde_json::{Map, Value};

/// Converts every object key in `value`, at every depth. Values are not
/// changed. When two keys convert to the same key, the later value wins and
/// keeps the position of the first key, like `Object.fromEntries`.
#[must_use]
pub fn camel_case_keys(value: Value) -> Value {
    match value {
        Value::Array(items) => Value::Array(items.into_iter().map(camel_case_keys).collect()),
        Value::Object(map) => {
            let mut converted = Map::with_capacity(map.len());
            for (key, item) in map {
                let key = if key.contains(':') || key.starts_with('_') {
                    key
                } else {
                    camel_case(&key)
                };
                converted.insert(key, camel_case_keys(item));
            }
            Value::Object(converted)
        }
        other => other,
    }
}

/// `change-case` 5 `camelCase` with the default options.
#[must_use]
pub fn camel_case(input: &str) -> String {
    let mut result = String::with_capacity(input.len());
    for (index, word) in split_words(input).iter().enumerate() {
        let mut characters = word.chars();
        let Some(first) = characters.next() else {
            continue;
        };
        if index == 0 {
            result.extend(word.chars().flat_map(char::to_lowercase));
            continue;
        }
        if first.is_ascii_digit() {
            result.push('_');
            result.push(first);
        } else {
            result.extend(first.to_uppercase());
        }
        result.extend(characters.flat_map(char::to_lowercase));
    }
    result
}

const DELIMITER: char = '\0';

/// `change-case` 5 `split`: case boundaries, then runs of characters that are
/// neither letters nor ASCII digits, become word delimiters.
fn split_words(input: &str) -> Vec<String> {
    let trimmed: Vec<char> = input.trim_matches(is_js_whitespace).chars().collect();

    // `([\p{Ll}\d])(\p{Lu})` -> "$1\0$2"
    let mut lower_upper = Vec::with_capacity(trimmed.len() * 2);
    for (index, &character) in trimmed.iter().enumerate() {
        lower_upper.push(character);
        if let Some(&next) = trimmed.get(index + 1)
            && (character.is_lowercase() || character.is_ascii_digit())
            && next.is_uppercase()
        {
            lower_upper.push(DELIMITER);
        }
    }

    // `(\p{Lu})([\p{Lu}][\p{Ll}])` -> "$1\0$2"
    let mut upper_upper = Vec::with_capacity(lower_upper.len() * 2);
    for (index, &character) in lower_upper.iter().enumerate() {
        upper_upper.push(character);
        if let (Some(&next), Some(&after)) =
            (lower_upper.get(index + 1), lower_upper.get(index + 2))
            && character.is_uppercase()
            && next.is_uppercase()
            && after.is_lowercase()
        {
            upper_upper.push(DELIMITER);
        }
    }

    // `[^\p{L}\d]+` -> "\0", then trim the delimiters and split on them.
    upper_upper
        .split(|&character| !(character.is_alphabetic() || character.is_ascii_digit()))
        .filter(|word| !word.is_empty())
        .map(|word| word.iter().collect())
        .collect()
}

/// The characters that `String.prototype.trim` removes.
fn is_js_whitespace(character: char) -> bool {
    character.is_whitespace() || character == '\u{feff}'
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn converts_like_change_case() {
        let cases = [
            ("avatar_static", "avatarStatic"),
            ("media_attachments", "mediaAttachments"),
            ("avatarStatic", "avatarStatic"),
            ("already_camelCase", "alreadyCamelCase"),
            ("double__under", "doubleUnder"),
            ("trailing_", "trailing"),
            ("x__", "x"),
            ("v2_3d_model", "v2_3dModel"),
            ("key_with_2nd_word", "keyWith_2ndWord"),
            ("UPPER_case", "upperCase"),
            ("HTMLParser_mode", "htmlParserMode"),
            ("h264_codec", "h264Codec"),
            ("kebab-key", "kebabKey"),
            ("dot.key", "dotKey"),
            ("  spaced key  ", "spacedKey"),
            ("", ""),
            ("a1B2c", "a1B2c"),
            ("aBCd", "aBCd"),
            ("16x9", "16x9"),
        ];
        for (input, expected) in cases {
            assert_eq!(camel_case(input), expected, "{input:?}");
        }
    }

    #[test]
    fn keeps_masto_exceptions_and_converts_nested_keys() {
        let input = json!({
            "_private_key": {"inner_key": 1},
            "ns:thing_name": {"inner_key": 2},
            "_": 3,
            "nested_list": [{"list_item_key": [{"deep_key": "value_stays_snake"}]}],
        });
        let expected = json!({
            "_private_key": {"innerKey": 1},
            "ns:thing_name": {"innerKey": 2},
            "_": 3,
            "nestedList": [{"listItemKey": [{"deepKey": "value_stays_snake"}]}],
        });
        assert_eq!(camel_case_keys(input), expected);
    }

    #[test]
    fn a_later_colliding_key_wins_at_the_first_position() {
        let input: Value =
            serde_json::from_str(r#"{"foo_bar": 1, "other": 2, "fooBar": 3}"#).unwrap();
        let output = serde_json::to_string(&camel_case_keys(input)).unwrap();
        assert_eq!(output, r#"{"fooBar":3,"other":2}"#);
    }
}
