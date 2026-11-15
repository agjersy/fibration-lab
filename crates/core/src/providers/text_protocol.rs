//! Degraded tier for models without native tool calling: tool calls are fenced
//! ```` ```tool_call ```` blocks containing `{"name": ..., "arguments": {...}}`, and results go
//! back as user messages prefixed `tool_result <name>:`.

use analyst_protocol::model::ToolDef;
use serde_json::Value;

#[derive(Debug, Clone, PartialEq)]
pub struct ParsedCall {
    pub name: String,
    pub arguments: Value,
}

const OPEN_FENCE: &str = "```tool_call";

fn parse_call(json_text: &str) -> Option<ParsedCall> {
    let v: Value = serde_json::from_str(json_text.trim()).ok()?;
    let name = v.get("name").and_then(Value::as_str)?.to_string();
    let arguments = v
        .get("arguments")
        .cloned()
        .unwrap_or(Value::Object(Default::default()));
    Some(ParsedCall { name, arguments })
}

/// Byte offsets in `body` of every closing-fence candidate: a ``` at the start of a line.
fn closing_fences(body: &str) -> Vec<usize> {
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(i) = body[from..].find("```") {
        let at = from + i;
        if at == 0 || body[..at].trim_end_matches([' ', '\t']).ends_with('\n') {
            out.push(at);
        }
        from = at + 3;
    }
    out
}

/// Extract fenced tool calls from assistant text. Returns the text with the blocks removed
/// and the calls in order. Malformed blocks are left in the text untouched.
///
/// The arguments may themselves contain ``` (a file with a Markdown code block, or even the
/// text "```tool_call"), so a block ends at the first line-initial closing fence for which
/// the enclosed text is valid JSON, not simply at the next ```. A final block the model left
/// unterminated is accepted when its body parses.
pub fn parse_tool_calls(text: &str) -> (String, Vec<ParsedCall>) {
    let mut calls = Vec::new();
    let mut clean = String::new();
    let mut rest = text;
    loop {
        let Some(start) = rest.find(OPEN_FENCE) else {
            clean.push_str(rest);
            break;
        };
        let after_fence = &rest[start + OPEN_FENCE.len()..];
        let body_start = after_fence.find('\n').map(|i| i + 1).unwrap_or(0);
        let body = &after_fence[body_start..];
        let fences = closing_fences(body);
        let found = fences
            .iter()
            .find_map(|end| parse_call(&body[..*end]).map(|c| (c, *end + 3)))
            .or_else(|| parse_call(body).map(|c| (c, body.len())));
        match found {
            Some((call, consumed)) => {
                calls.push(call);
                clean.push_str(&rest[..start]);
                rest = &body[consumed..];
            }
            None => {
                // Leave the malformed block in place and continue after its first fence.
                let skip = match fences.first() {
                    Some(end) => start + OPEN_FENCE.len() + body_start + end + 3,
                    None => rest.len(),
                };
                clean.push_str(&rest[..skip]);
                rest = &rest[skip..];
            }
        }
    }
    (clean.trim().to_string(), calls)
}

/// System prompt addendum describing the protocol and the available tools.
pub fn instructions(tools: &[ToolDef]) -> String {
    let mut s = String::from(
        "TOOL CALLS: this interface has no native tool calling. To call a tool, emit a fenced block exactly like:\n\n```tool_call\n{\"name\": \"<tool name>\", \"arguments\": { ... }}\n```\n\nEmit one block per call, then stop and wait; results arrive as a user message starting with `tool_result <name>:`. Available tools:\n",
    );
    for t in tools {
        s.push_str(&format!(
            "- {}: {}\n  parameters: {}\n",
            t.name,
            t.description,
            serde_json::to_string(&t.parameters).unwrap_or_default()
        ));
    }
    s
}

/// Format a result for the user message.
pub fn format_result(name: &str, result: &Value) -> String {
    format!(
        "tool_result {name}:\n{}",
        serde_json::to_string_pretty(result).unwrap_or_default()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_fenced_calls_and_keeps_text() {
        let text = "I will read it.\n```tool_call\n{\"name\": \"fs_read\", \"arguments\": {\"path\": \"a.py\"}}\n```\nThen done.";
        let (clean, calls) = parse_tool_calls(text);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "fs_read");
        assert_eq!(calls[0].arguments, json!({"path":"a.py"}));
        assert_eq!(clean, "I will read it.\n\nThen done.");
    }

    #[test]
    fn malformed_block_is_left_in_text() {
        let text = "```tool_call\nnot json\n```";
        let (clean, calls) = parse_tool_calls(text);
        assert!(calls.is_empty());
        assert_eq!(clean, text);
    }

    /// The fence bug: arguments containing ``` ended the block early, so every edit that
    /// wrote a Markdown code block was silently dropped.
    #[test]
    fn arguments_may_contain_code_fences() {
        let content = "# Notes\n\n```python\nprint(1)\n```\n\nSee ```tool_call for details.\n";
        let call = json!({"name": "fs_propose_create", "arguments": {"path": "notes.md", "content": content}});
        let text = format!(
            "Writing notes.\n```tool_call\n{}\n```\nand then\n```tool_call\n{{\"name\": \"fs_list\", \"arguments\": {{}}}}\n```\n",
            serde_json::to_string_pretty(&call).unwrap()
        );
        let (clean, calls) = parse_tool_calls(&text);
        assert_eq!(calls.len(), 2, "{clean}");
        assert_eq!(calls[0].arguments["content"], content);
        assert_eq!(calls[1].name, "fs_list");
        assert_eq!(clean, "Writing notes.\n\nand then");
    }

    #[test]
    fn unterminated_final_block_and_malformed_then_valid() {
        let (clean, calls) =
            parse_tool_calls("ok\n```tool_call\n{\"name\": \"fs_list\", \"arguments\": {}}");
        assert_eq!(calls.len(), 1);
        assert_eq!(clean, "ok");
        let text = "```tool_call\nnot json\n```\n```tool_call\n{\"name\": \"a\"}\n```";
        let (clean, calls) = parse_tool_calls(text);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "a");
        assert_eq!(clean, "```tool_call\nnot json\n```");
    }
}
