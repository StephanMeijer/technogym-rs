//! Tool results: JSON, compacted where it matters, and never bigger than a client can take.

use rmcp::model::{CallToolResult, ContentBlock};
use serde::Serialize;
use serde_json::Value;

/// One tool result may not exceed this; 1 Hz analytics of a long run reach megabytes.
pub const MAX_RESULT_CHARS: usize = 200_000;

/// Cuts `text` at [`MAX_RESULT_CHARS`] with a note saying so.
pub fn truncate(text: String) -> String {
    if text.chars().count() <= MAX_RESULT_CHARS {
        return text;
    }
    let total = text.chars().count();
    let kept: String = text.chars().take(MAX_RESULT_CHARS).collect();
    format!(
        "{kept}\n[truncated: {MAX_RESULT_CHARS} of {total} characters; narrow the window or raise sample_every_seconds]"
    )
}

/// A successful text result.
pub fn text_result(text: String) -> CallToolResult {
    CallToolResult::success(vec![ContentBlock::text(truncate(text))])
}

/// A failed result with a plain-language message.
pub fn error_result(message: String) -> CallToolResult {
    CallToolResult::error(vec![ContentBlock::text(message)])
}

/// Pretty-printed JSON.
pub fn json_result(value: &impl Serialize) -> CallToolResult {
    text_result(
        serde_json::to_string_pretty(value)
            .unwrap_or_else(|e| format!("serialisation failed: {e}")),
    )
}

/// Pretty-printed JSON, except that the time-series arrays named in `row_keys` keep one compact
/// row per line. Pretty-printing a `{t, Speed, Grade, ...}` sample costs ~8 lines; at 1 Hz that
/// triples the payload for no gain in readability.
pub fn compact_json(mut value: Value, row_keys: &[&str]) -> String {
    let mut rows: Vec<(String, Vec<Value>)> = vec![];
    if let Some(obj) = value.as_object_mut() {
        for key in row_keys {
            if let Some(Value::Array(list)) = obj.get(*key)
                && !list.is_empty()
            {
                let token = format!("__ROWS_{key}__");
                let list = obj
                    .insert((*key).to_string(), Value::String(token.clone()))
                    .and_then(|v| v.as_array().cloned());
                rows.push((token, list.unwrap_or_default()));
            }
        }
    }
    let mut text = serde_json::to_string_pretty(&value).unwrap_or_default();
    for (token, list) in rows {
        let body = list
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(",\n    ");
        text = text.replace(&format!("\"{token}\""), &format!("[\n    {body}\n  ]"));
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn rows_stay_on_one_line() {
        let text = compact_json(
            json!({ "a": 1, "samples": [{ "t": 1, "Speed": 10.0 }, { "t": 2 }], "hr": [] }),
            &["samples", "hr"],
        );
        assert!(
            text.contains("\"samples\": [\n    {\"Speed\":10.0,\"t\":1},\n    {\"t\":2}\n  ]"),
            "{text}"
        );
        assert!(text.contains("\"hr\": []"), "{text}");
    }

    #[test]
    fn long_results_are_cut_with_a_note() {
        let text = truncate("x".repeat(MAX_RESULT_CHARS + 5));
        assert!(text.starts_with(&"x".repeat(MAX_RESULT_CHARS)));
        assert!(text.ends_with("raise sample_every_seconds]"));
        assert_eq!(truncate("short".into()), "short");
    }
}
