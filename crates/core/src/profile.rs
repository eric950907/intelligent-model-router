//! 请求画像提取(spec §6.1)。容错: 非 chat JSON → 包一层;空输入 → 422 级错误。

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const EXCERPT_HEAD: usize = 2000;
pub const EXCERPT_TAIL: usize = 500;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Profile {
    pub est_tokens: u32,
    pub text_chars: usize,
    pub has_images: bool,
    pub tools_count: u32,
    pub json_mode: bool,
    pub stream: bool,
    pub max_tokens: Option<u32>,
    pub locale_hint: Option<String>, // "zh" | "other";空文本为 None
    pub task_hints: Vec<String>,
    pub request_excerpt: String,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProfileError {
    #[error("请求体为空,无法解析为 OpenAI/Claude 格式")]
    Empty,
}

impl Profile {
    /// 服务端入口:内层 request 字段(信封保证是合法 JSON Value)。
    pub fn from_value(body: &Value) -> Result<Profile, ProfileError> {
        let mut text = String::new();
        let mut message_count = 0usize;
        let mut has_images = false;

        let is_chat = body.get("messages").and_then(Value::as_array).is_some();
        if is_chat {
            if let Some(sys) = body.get("system") {
                collect_blocks(sys, &mut text, &mut has_images);
            }
            if let Some(msgs) = body["messages"].as_array() {
                message_count = msgs.len();
                for m in msgs {
                    if let Some(c) = m.get("content") {
                        collect_blocks(c, &mut text, &mut has_images);
                    }
                }
            }
            let tools_count = body
                .get("tools")
                .and_then(Value::as_array)
                .map(|a| a.len() as u32)
                .unwrap_or(0);
            let json_mode = matches!(
                body.get("response_format")
                    .and_then(|r| r.get("type"))
                    .and_then(Value::as_str),
                Some("json_object") | Some("json_schema")
            );
            let stream = body.get("stream").and_then(Value::as_bool).unwrap_or(false);
            let max_tokens = body
                .get("max_tokens")
                .or_else(|| body.get("max_completion_tokens"))
                .and_then(Value::as_u64)
                .map(|v| v.min(u32::MAX as u64) as u32);
            return Ok(Self::assemble(
                text,
                message_count,
                has_images,
                tools_count,
                json_mode,
                stream,
                max_tokens,
            ));
        }

        // 非 chat:字符串直接当正文;空/null/空容器 → Empty;其余 JSON → 序列化文本包一层。
        if let Some(s) = body.as_str() {
            if s.trim().is_empty() {
                return Err(ProfileError::Empty);
            }
            return Ok(Self::assemble_one(s.to_string()));
        }
        let empty_container = body.is_null()
            || body.as_object().map(|o| o.is_empty()).unwrap_or(false)
            || body.as_array().map(|a| a.is_empty()).unwrap_or(false);
        if empty_container {
            return Err(ProfileError::Empty);
        }
        Ok(Self::assemble_one(body.to_string()))
    }

    /// CLI/测试入口:任意文本(可能根本不是 JSON)。
    pub fn from_text(raw: &str) -> Result<Profile, ProfileError> {
        match serde_json::from_str::<Value>(raw) {
            Ok(v) => Profile::from_value(&v),
            Err(_) => {
                if raw.trim().is_empty() {
                    return Err(ProfileError::Empty);
                }
                Ok(Self::assemble_one(raw.to_string()))
            }
        }
    }

    fn assemble_one(text: String) -> Profile {
        Self::assemble(text, 1, false, 0, false, false, None)
    }

    fn assemble(
        text: String,
        message_count: usize,
        has_images: bool,
        tools_count: u32,
        json_mode: bool,
        stream: bool,
        max_tokens: Option<u32>,
    ) -> Profile {
        Profile {
            est_tokens: estimate_tokens(&text, message_count),
            text_chars: text.chars().count(),
            has_images,
            tools_count,
            json_mode,
            stream,
            max_tokens,
            locale_hint: locale_hint(&text),
            task_hints: task_hints(&text),
            request_excerpt: make_excerpt(&text),
        }
    }
}

fn collect_blocks(content: &Value, text: &mut String, images: &mut bool) {
    match content {
        Value::String(s) => {
            text.push_str(s);
            text.push('\n');
        }
        Value::Array(items) => {
            for it in items {
                match it.get("type").and_then(Value::as_str) {
                    Some("text") | Some("input_text") | Some("output_text") => {
                        if let Some(t) = it.get("text").and_then(Value::as_str) {
                            text.push_str(t);
                            text.push('\n');
                        }
                    }
                    Some("image_url") | Some("image") | Some("input_image") => *images = true,
                    _ => {}
                }
            }
        }
        _ => {}
    }
}

/// spec §5.2: ASCII 字符/3.5 + CJK 字符/1.5 + 每消息 4 token;向上取整(保守)。
pub fn estimate_tokens(text: &str, message_count: usize) -> u32 {
    let (mut ascii, mut cjk) = (0u64, 0u64);
    for c in text.chars() {
        if is_cjk(c) {
            cjk += 1;
        } else {
            ascii += 1;
        }
    }
    let t = ascii as f64 / 3.5 + cjk as f64 / 1.5 + 4.0 * message_count as f64;
    t.ceil() as u32
}

pub fn is_cjk(c: char) -> bool {
    matches!(c as u32,
        0x3000..=0x303F | 0x3040..=0x30FF | 0x3400..=0x4DBF
        | 0x4E00..=0x9FFF | 0xAC00..=0xD7A3 | 0xF900..=0xFAFF | 0xFF00..=0xFFEF)
}

/// 清洗(去控制字符,\n \t 保留)+ 头 2000/尾 500 截断(spec §6.1)。
pub fn make_excerpt(text: &str) -> String {
    let cleaned: String = text
        .chars()
        .filter(|c| !c.is_control() || matches!(c, '\n' | '\t'))
        .collect();
    let chars: Vec<char> = cleaned.chars().collect();
    if chars.len() <= EXCERPT_HEAD + EXCERPT_TAIL {
        return cleaned;
    }
    let head: String = chars[..EXCERPT_HEAD].iter().collect();
    let tail: String = chars[chars.len() - EXCERPT_TAIL..].iter().collect();
    format!("{head}…[截断]…{tail}")
}

fn locale_hint(text: &str) -> Option<String> {
    let total = text.chars().count();
    if total == 0 {
        return None;
    }
    let cjk = text.chars().filter(|c| is_cjk(*c)).count();
    Some(if cjk as f64 / total as f64 > 0.2 {
        "zh".into()
    } else {
        "other".into()
    })
}

/// spec §6.1 固定检测表 v1 三条;命中才加入。
fn task_hints(text: &str) -> Vec<String> {
    let mut v = vec![];
    if text.contains("```") {
        v.push("包含代码块".to_string());
    }
    let lower = text.to_lowercase();
    if ["error", "exception", "traceback", "异常", "堆栈"]
        .iter()
        .any(|k| lower.contains(k))
    {
        v.push("提到报错堆栈".to_string());
    }
    if text.contains("://") || lower.contains("www.") {
        v.push("包含链接".to_string());
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn openai_basic() {
        let p = Profile::from_value(&json!({
            "messages": [{"role":"user","content":"hello"}]
        }))
        .unwrap();
        assert_eq!(p.est_tokens, 6); // "hello\n" 6 ascii /3.5 + 4×1 消息 → ceil ≈ 6
        assert!(!p.has_images && !p.json_mode && !p.stream);
        assert_eq!(p.tools_count, 0);
        assert_eq!(p.locale_hint.as_deref(), Some("other"));
        assert_eq!(p.request_excerpt, "hello\n");
    }

    #[test]
    fn claude_system_and_content_blocks() {
        let p = Profile::from_value(&json!({
            "system": "你是助手",
            "messages": [{"role":"user","content":[
                {"type":"text","text":"看这张图"},
                {"type":"image","source":{"data":"x"}}
            ]}],
            "max_tokens": 1024
        }))
        .unwrap();
        assert!(p.has_images);
        assert_eq!(p.max_tokens, Some(1024));
        assert!(p.request_excerpt.contains("你是助手"));
        assert!(p.request_excerpt.contains("看这张图"));
        assert_eq!(p.locale_hint.as_deref(), Some("zh")); // CJK 占比 > 20%
    }

    #[test]
    fn openai_flags() {
        let p = Profile::from_value(&json!({
            "messages": [{"role":"user","content":"hi"}],
            "tools": [{"a":1},{"b":2}],
            "response_format": {"type":"json_schema"},
            "stream": true,
            "max_completion_tokens": 2048
        }))
        .unwrap();
        assert_eq!(p.tools_count, 2);
        assert!(p.json_mode && p.stream);
        assert_eq!(p.max_tokens, Some(2048));
    }

    #[test]
    fn wrap_non_chat_json_and_raw_text() {
        let p = Profile::from_value(&json!({"foo": 1})).unwrap();
        assert!(p.request_excerpt.contains("foo"));
        let p2 = Profile::from_text("这不是 JSON{").unwrap();
        assert_eq!(p2.request_excerpt, "这不是 JSON{");
        assert!(p2.est_tokens > 0);
    }

    #[test]
    fn empty_inputs_rejected() {
        assert_eq!(Profile::from_text("   ").unwrap_err(), ProfileError::Empty);
        assert!(Profile::from_value(&json!("")).is_err());
        assert!(Profile::from_value(&json!({})).is_err());
        assert!(Profile::from_value(&json!([])).is_err());
        assert!(Profile::from_value(&json!(null)).is_err());
    }

    #[test]
    fn excerpt_truncation_and_cleaning() {
        let long = "a".repeat(3000);
        let p = Profile::from_text(&long).unwrap();
        let n = p.request_excerpt.chars().count();
        assert_eq!(n, 2000 + 6 + 500); // head + "…[截断]…" + tail
        assert!(p.request_excerpt.contains("[截断]"));

        let p2 = Profile::from_value(&json!({
            "messages": [{"role":"user","content":"a\u{0000}b\nc\tx"}]
        }))
        .unwrap();
        assert_eq!(p2.request_excerpt, "ab\nc\tx\n"); // 控制字符去除,\n \t 保留
    }

    #[test]
    fn hints_are_chinese_strings() {
        let p = Profile::from_text("```rust\ncode\n```\nError: boom\nhttps://x.com").unwrap();
        assert!(p.task_hints.contains(&"包含代码块".to_string()));
        assert!(p.task_hints.contains(&"提到报错堆栈".to_string()));
        assert!(p.task_hints.contains(&"包含链接".to_string()));
    }

    #[test]
    fn cjk_ranges() {
        assert!(is_cjk('你') && is_cjk('あ') && is_cjk('한'));
        assert!(!is_cjk('a') && !is_cjk('é'));
    }
}
