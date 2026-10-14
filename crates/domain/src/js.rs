//! JavaScript value semantics that the stored data and the feed depend on.
//!
//! The TypeScript service used `??`, truthiness, and `String(...)` on
//! untyped Mastodon values. These helpers give the same results for the JSON
//! values that can occur, so the Rust service makes the same decisions.

use serde_json::Value;

/// `value ?? fallback`: the first value that is present and not `null`.
pub(crate) fn coalesce<'a>(
    value: Option<&'a Value>,
    fallback: Option<&'a Value>,
) -> Option<&'a Value> {
    match value {
        None | Some(Value::Null) => fallback,
        present => present,
    }
}

/// JavaScript truthiness of a JSON value. A missing value is `undefined`.
pub(crate) fn is_truthy(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) => false,
        Some(Value::Bool(flag)) => *flag,
        Some(Value::Number(number)) => number.as_f64().is_some_and(|n| n != 0.0 && !n.is_nan()),
        Some(Value::String(text)) => !text.is_empty(),
        Some(Value::Array(_) | Value::Object(_)) => true,
    }
}

/// `String(value ?? "")`.
pub(crate) fn to_js_string(value: Option<&Value>) -> String {
    match value {
        None | Some(Value::Null) => String::new(),
        Some(other) => stringify(other),
    }
}

fn stringify(value: &Value) -> String {
    match value {
        Value::Null => "null".to_owned(),
        Value::Bool(flag) => flag.to_string(),
        Value::Number(number) => number_to_string(number),
        Value::String(text) => text.clone(),
        // `String([a, b])` joins the elements with commas, and `null` and
        // `undefined` elements become empty strings.
        Value::Array(items) => items
            .iter()
            .map(|item| match item {
                Value::Null => String::new(),
                other => stringify(other),
            })
            .collect::<Vec<_>>()
            .join(","),
        Value::Object(_) => "[object Object]".to_owned(),
    }
}

/// `Number.prototype.toString` for integers and ordinary decimals. JSON from
/// Mastodon has no exponent forms in the fields that this crate converts.
fn number_to_string(number: &serde_json::Number) -> String {
    if let Some(integer) = number.as_i64() {
        return integer.to_string();
    }
    if let Some(integer) = number.as_u64() {
        return integer.to_string();
    }
    match number.as_f64() {
        Some(float) if float.fract() == 0.0 && float.abs() < 1e21 => format!("{float:.0}"),
        Some(float) => float.to_string(),
        None => number.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn coalesce_skips_only_missing_and_null() {
        let empty = json!("");
        let fallback = json!("fallback");
        assert_eq!(coalesce(None, Some(&fallback)), Some(&fallback));
        assert_eq!(
            coalesce(Some(&Value::Null), Some(&fallback)),
            Some(&fallback)
        );
        assert_eq!(coalesce(Some(&empty), Some(&fallback)), Some(&empty));
    }

    #[test]
    fn truthiness_follows_javascript() {
        for falsy in [json!(null), json!(false), json!(0), json!(0.0), json!("")] {
            assert!(!is_truthy(Some(&falsy)), "{falsy}");
        }
        for truthy in [
            json!(true),
            json!(1),
            json!(-0.5),
            json!("0"),
            json!([]),
            json!({}),
        ] {
            assert!(is_truthy(Some(&truthy)), "{truthy}");
        }
        assert!(!is_truthy(None));
    }

    #[test]
    fn string_conversion_follows_javascript() {
        assert_eq!(to_js_string(None), "");
        assert_eq!(to_js_string(Some(&json!(null))), "");
        assert_eq!(to_js_string(Some(&json!("text"))), "text");
        assert_eq!(
            to_js_string(Some(&json!(113_000_000_000_000_008_u64))),
            "113000000000000008"
        );
        assert_eq!(to_js_string(Some(&json!(1.0))), "1");
        assert_eq!(to_js_string(Some(&json!(1.5))), "1.5");
        assert_eq!(to_js_string(Some(&json!(false))), "false");
        assert_eq!(to_js_string(Some(&json!([1, null, "a"]))), "1,,a");
        assert_eq!(to_js_string(Some(&json!({"a": 1}))), "[object Object]");
    }
}
