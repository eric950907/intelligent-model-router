//! /v1/systemone 契约类型(spec §7)、JudgmentBackend trait(spec §4.3)、响应容错解码(spec §8.2)。

use crate::config::QuestionType;
use futures::future::BoxFuture;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JudgeRequest {
    /// 发送时由客户端覆盖为配置的钉死版本 ID(pipeline 构造时不填)。
    #[serde(default)]
    pub model: String,
    pub state: serde_json::Value,
    pub questions: BTreeMap<String, Question>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Question {
    #[serde(rename = "type")]
    pub qtype: QuestionType,
    pub instructions: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub criteria: Option<serde_json::Value>,
}

impl From<&crate::config::QuestionSpec> for Question {
    fn from(s: &crate::config::QuestionSpec) -> Self {
        Question {
            qtype: s.qtype,
            instructions: s.instructions.clone(),
            criteria: s.criteria.clone(),
        }
    }
}

// 响应:宽松解析(不 deny_unknown_fields,服务端可加字段);answers 先收 Value,再逐个容错解码。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JudgeResponse {
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub answers: BTreeMap<String, serde_json::Value>,
    #[serde(default)]
    pub usage: Option<WireUsage>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct WireUsage {
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
}

#[derive(Debug, Clone, thiserror::Error)]
pub enum JudgeError {
    #[error("后端 API key 未配置")]
    MissingKey,
    #[error("后端 HTTP {status}: {body}")]
    Http { status: u16, body: String },
    #[error("后端超时")]
    Timeout,
    #[error("后端连接失败: {0}")]
    Connect(String),
    #[error("后端响应解析失败: {0}")]
    Decode(String),
}

impl JudgeError {
    /// 降级原因(spec §8.2):4xx(非 429) → backend_4xx;429 → backend_429;其余 → backend_unavailable。
    pub fn degraded_reason(&self) -> &'static str {
        match self {
            JudgeError::Http { status, .. } if (400..500).contains(status) && *status != 429 => {
                "backend_4xx"
            }
            JudgeError::Http { status: 429, .. } => "backend_429",
            _ => "backend_unavailable",
        }
    }
}

pub trait JudgmentBackend: Send + Sync {
    fn id(&self) -> &str;
    fn judge<'a>(&'a self, req: JudgeRequest) -> BoxFuture<'a, Result<JudgeResponse, JudgeError>>;
}

// ---- 容错解码(spec §8.2“缺答案”口径)----

#[derive(Debug, Clone, Default, PartialEq)]
pub struct SharedAnswers {
    pub task_type: Option<String>,
    pub difficulty: Option<f32>,
    pub frontier_needed: Option<f32>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DimAnswer {
    pub raw_score: f32,
    pub confidence: f32,
    pub legend: BTreeMap<String, String>,
    pub probabilities: BTreeMap<String, f32>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct CandidateAnswers {
    pub quality: Option<DimAnswer>,
    pub cost: Option<DimAnswer>,
}

#[derive(Debug, Clone, Default)]
pub struct DecodedAnswers {
    pub shared: SharedAnswers,
    pub per_candidate: Vec<CandidateAnswers>,
    /// 缺失或不可解析的问题 ID(→ meta.partial_answers)。
    /// 顺序为确定性插入序:共享问题在前(task_type、difficulty、frontier_needed),
    /// 随后逐候选按 c{i}.quality_fit、c{i}.cost_fit 排列;每次调用同一 ID 至多出现一次。
    /// 调用方(如 Task 19 dry-run / Task 20 golden)需要稳定比较时可自行排序/去重。
    pub partial: Vec<String>,
}

pub fn decode_answers(resp: &JudgeResponse, candidate_count: usize) -> DecodedAnswers {
    let mut out = DecodedAnswers::default();
    let get = |id: &str| resp.answers.get(id);

    match get("task_type") {
        Some(v) if v.get("type").and_then(|t| t.as_str()) == Some("choice") => {
            match v.get("choice").and_then(|c| c.as_str()) {
                Some(c) => out.shared.task_type = Some(c.to_string()),
                None => out.partial.push("task_type".into()),
            }
        }
        Some(_) => out.partial.push("task_type".into()),
        None => out.partial.push("task_type".into()),
    }

    // difficulty: score 形状 `{type, score, legend, probabilities, confidence}`;
    // 缺失/类型不符/值非数值 → partial + None(融合按 0.5 中性值)。
    match get("difficulty") {
        Some(v) if v.get("type").and_then(|t| t.as_str()) == Some("score") => {
            match v.get("score").and_then(|s| s.as_f64()) {
                Some(s) => out.shared.difficulty = Some(s as f32),
                None => out.partial.push("difficulty".into()),
            }
        }
        Some(_) => out.partial.push("difficulty".into()),
        None => out.partial.push("difficulty".into()),
    }

    // frontier_needed: noul 形状 `{type, noul: 0..1}`;同上。
    match get("frontier_needed") {
        Some(v) if v.get("type").and_then(|t| t.as_str()) == Some("noul") => {
            match v.get("noul").and_then(|n| n.as_f64()) {
                Some(n) => out.shared.frontier_needed = Some(n as f32),
                None => out.partial.push("frontier_needed".into()),
            }
        }
        Some(_) => out.partial.push("frontier_needed".into()),
        None => out.partial.push("frontier_needed".into()),
    }

    for i in 0..candidate_count {
        let quality_id = format!("c{i}.quality_fit");
        let cost_id = format!("c{i}.cost_fit");
        let quality = decode_dim(get(&quality_id), &mut out.partial, &quality_id);
        let cost = decode_dim(get(&cost_id), &mut out.partial, &cost_id);
        out.per_candidate.push(CandidateAnswers { quality, cost });
    }
    out
}

fn decode_dim(
    v: Option<&serde_json::Value>,
    partial: &mut Vec<String>,
    id: &str,
) -> Option<DimAnswer> {
    let Some(v) = v else {
        partial.push(id.to_string());
        return None;
    };
    if v.get("type").and_then(|t| t.as_str()) != Some("score") {
        partial.push(id.to_string());
        return None;
    }
    let Some(score) = v.get("score").and_then(|s| s.as_f64()) else {
        partial.push(id.to_string());
        return None;
    };
    let legend = v
        .get("legend")
        .and_then(|l| l.as_object())
        .map(|o| {
            o.iter()
                .filter_map(|(k, val)| val.as_str().map(|s| (k.clone(), s.to_string())))
                .collect()
        })
        .unwrap_or_default();
    let probabilities = v
        .get("probabilities")
        .and_then(|p| p.as_object())
        .map(|o| {
            o.iter()
                .filter_map(|(k, val)| val.as_f64().map(|f| (k.clone(), f as f32)))
                .collect()
        })
        .unwrap_or_default();
    // 缺省或非数值 confidence 故意按 0.0 处理(spec §8.2:未知置信度视为 0;配合
    // flag_below 默认 0.5 保守地标记低置信度),且此降级不记入 partial。
    let confidence = v.get("confidence").and_then(|c| c.as_f64()).unwrap_or(0.0) as f32;
    Some(DimAnswer {
        raw_score: score as f32,
        confidence,
        legend,
        probabilities,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::QuestionSpec;
    use serde_json::json;

    fn resp(answers: serde_json::Value) -> JudgeResponse {
        JudgeResponse {
            model: "test-1".into(),
            answers: serde_json::from_value(answers).expect("answers 必须是 JSON object"),
            usage: None,
        }
    }

    /// 3 个共享问题 + 2 候选 × 2 维的完整合法响应。
    fn full_answers() -> serde_json::Value {
        json!({
            "task_type": {"type":"choice","choice":"chat","probabilities":{"chat":0.9},"confidence":0.9},
            "difficulty": {"type":"score","score":1.2,"legend":{"0":"简单","1":"中等","2":"困难"},
                            "probabilities":{"0":0.1,"1":0.5,"2":0.4},"confidence":0.9},
            "frontier_needed": {"type":"noul","noul":0.3},
            "c0.quality_fit": {"type":"score","score":0.8,"confidence":0.7},
            "c0.cost_fit": {"type":"score","score":1.0,"confidence":0.6},
            "c1.quality_fit": {"type":"score","score":1.5,"legend":{"0":"便宜","1":"贵"},
                                "probabilities":{"0":0.2,"1":0.8},"confidence":0.8},
            "c1.cost_fit": {"type":"score","score":2.0,"confidence":0.5},
        })
    }

    #[test]
    fn decodes_full_response() {
        let d = decode_answers(&resp(full_answers()), 2);
        assert!(d.partial.is_empty(), "partial: {:?}", d.partial);
        assert_eq!(d.shared.task_type.as_deref(), Some("chat"));
        assert_eq!(d.shared.difficulty, Some(1.2));
        assert_eq!(d.shared.frontier_needed, Some(0.3));
        assert_eq!(d.per_candidate.len(), 2);

        let q0 = d.per_candidate[0].quality.as_ref().expect("c0.quality_fit");
        assert_eq!(q0.raw_score, 0.8);
        assert_eq!(q0.confidence, 0.7);
        assert!(q0.legend.is_empty());
        assert!(q0.probabilities.is_empty());
        assert_eq!(d.per_candidate[0].cost.as_ref().unwrap().raw_score, 1.0);

        let q1 = d.per_candidate[1].quality.as_ref().expect("c1.quality_fit");
        assert_eq!(q1.raw_score, 1.5);
        assert_eq!(q1.confidence, 0.8);
        assert_eq!(q1.legend.get("0").map(String::as_str), Some("便宜"));
        assert_eq!(q1.probabilities.get("1").copied(), Some(0.8));
        assert_eq!(d.per_candidate[1].cost.as_ref().unwrap().raw_score, 2.0);
    }

    #[test]
    fn missing_candidate_dim_is_partial_and_isolated() {
        let mut a = full_answers();
        a.as_object_mut().unwrap().remove("c1.quality_fit");
        let d = decode_answers(&resp(a), 2);

        assert!(d.per_candidate[1].quality.is_none());
        assert!(
            d.partial.contains(&"c1.quality_fit".to_string()),
            "partial: {:?}",
            d.partial
        );
        // 其余维度不受影响
        assert!(d.per_candidate[0].quality.is_some());
        assert!(d.per_candidate[0].cost.is_some());
        assert!(d.per_candidate[1].cost.is_some());
        assert_eq!(d.shared.task_type.as_deref(), Some("chat"));
        assert_eq!(d.shared.difficulty, Some(1.2));
        assert_eq!(d.shared.frontier_needed, Some(0.3));
    }

    #[test]
    fn malformed_type_or_value_goes_partial() {
        // difficulty: score 值是字符串 → partial + None
        let mut a = full_answers();
        a["difficulty"] = json!({"type":"score","score":"1.2"});
        let d = decode_answers(&resp(a), 2);
        assert!(d.shared.difficulty.is_none());
        assert!(
            d.partial.contains(&"difficulty".to_string()),
            "partial: {:?}",
            d.partial
        );

        // difficulty: type 写错 → partial + None
        let mut a = full_answers();
        a["difficulty"] = json!({"type":"noul","noul":0.3});
        let d = decode_answers(&resp(a), 2);
        assert!(d.shared.difficulty.is_none());
        assert!(
            d.partial.contains(&"difficulty".to_string()),
            "partial: {:?}",
            d.partial
        );

        // 候选维: type 写错 → partial + None
        let mut a = full_answers();
        a["c0.quality_fit"] = json!({"type":"choice","choice":"chat"});
        let d = decode_answers(&resp(a), 2);
        assert!(d.per_candidate[0].quality.is_none());
        assert!(
            d.partial.contains(&"c0.quality_fit".to_string()),
            "partial: {:?}",
            d.partial
        );

        // 候选维: score 值是字符串 → partial + None
        let mut a = full_answers();
        a["c0.cost_fit"] = json!({"type":"score","score":"1.0"});
        let d = decode_answers(&resp(a), 2);
        assert!(d.per_candidate[0].cost.is_none());
        assert!(
            d.partial.contains(&"c0.cost_fit".to_string()),
            "partial: {:?}",
            d.partial
        );

        // task_type: choice 值缺失(非字符串)→ partial + None
        let mut a = full_answers();
        a["task_type"] = json!({"type":"choice","probabilities":{"chat":0.9}});
        let d = decode_answers(&resp(a), 2);
        assert!(d.shared.task_type.is_none());
        assert!(
            d.partial.contains(&"task_type".to_string()),
            "partial: {:?}",
            d.partial
        );
    }

    #[test]
    fn noul_missing_or_wrong_value_type_goes_partial() {
        // 缺失 → partial + None
        let mut a = full_answers();
        a.as_object_mut().unwrap().remove("frontier_needed");
        let d = decode_answers(&resp(a), 2);
        assert!(d.shared.frontier_needed.is_none());
        assert!(
            d.partial.contains(&"frontier_needed".to_string()),
            "partial: {:?}",
            d.partial
        );

        // 值类型错误(字符串)→ partial + None
        let mut a = full_answers();
        a["frontier_needed"] = json!({"type":"noul","noul":"0.3"});
        let d = decode_answers(&resp(a), 2);
        assert!(d.shared.frontier_needed.is_none());
        assert!(
            d.partial.contains(&"frontier_needed".to_string()),
            "partial: {:?}",
            d.partial
        );

        // type 写错 → partial + None
        let mut a = full_answers();
        a["frontier_needed"] = json!({"type":"score","score":0.3});
        let d = decode_answers(&resp(a), 2);
        assert!(d.shared.frontier_needed.is_none());
        assert!(
            d.partial.contains(&"frontier_needed".to_string()),
            "partial: {:?}",
            d.partial
        );
    }

    #[test]
    fn degraded_reason_mapping() {
        let http = |status: u16| JudgeError::Http {
            status,
            body: "boom".into(),
        };
        assert_eq!(http(400).degraded_reason(), "backend_4xx");
        assert_eq!(http(401).degraded_reason(), "backend_4xx");
        assert_eq!(http(404).degraded_reason(), "backend_4xx");
        assert_eq!(http(429).degraded_reason(), "backend_429");
        assert_eq!(http(529).degraded_reason(), "backend_unavailable");
        assert_eq!(http(500).degraded_reason(), "backend_unavailable");
        assert_eq!(JudgeError::Timeout.degraded_reason(), "backend_unavailable");
        assert_eq!(
            JudgeError::MissingKey.degraded_reason(),
            "backend_unavailable"
        );
        assert_eq!(
            JudgeError::Connect("refused".into()).degraded_reason(),
            "backend_unavailable"
        );
        assert_eq!(
            JudgeError::Decode("bad json".into()).degraded_reason(),
            "backend_unavailable"
        );
    }

    #[test]
    fn judge_request_serializes_type_field() {
        let questions = BTreeMap::from([
            (
                "task_type".to_string(),
                Question {
                    qtype: QuestionType::Choice,
                    instructions: json!("判断任务类型"),
                    criteria: Some(json!({"chat": null})),
                },
            ),
            (
                "frontier_needed".to_string(),
                Question {
                    qtype: QuestionType::Noul,
                    instructions: json!("是否需要前沿模型"),
                    criteria: None,
                },
            ),
        ]);
        let req = JudgeRequest {
            model: String::new(),
            state: json!({"task": "hello"}),
            questions,
        };
        let v = serde_json::to_value(&req).unwrap();
        assert_eq!(v["questions"]["task_type"]["type"], "choice");
        assert_eq!(v["questions"]["frontier_needed"]["type"], "noul");
        assert_eq!(
            v["questions"]["task_type"]["criteria"],
            json!({"chat": null})
        );
        assert!(v["questions"]["frontier_needed"].get("criteria").is_none());
        assert_eq!(v["state"], json!({"task": "hello"}));
        assert_eq!(v["model"], "");

        // 反序列化:type 字段映射回 qtype
        let back: JudgeRequest = serde_json::from_value(v).unwrap();
        assert_eq!(back.questions["task_type"].qtype, QuestionType::Choice);
    }

    #[test]
    fn question_from_spec_copies_fields() {
        let spec = QuestionSpec {
            qtype: QuestionType::Score,
            instructions: json!("0-2 打分"),
            criteria: Some(json!(["低", "中", "高"])),
        };
        let q = Question::from(&spec);
        assert_eq!(
            q,
            Question {
                qtype: QuestionType::Score,
                instructions: json!("0-2 打分"),
                criteria: Some(json!(["低", "中", "高"])),
            }
        );
    }

    #[test]
    fn judge_response_parses_leniently() {
        let r: JudgeResponse = serde_json::from_value(json!({
            "model": "jev-1.13.0",
            "answers": {"task_type": {"type": "choice", "choice": "chat"}},
            "usage": {"input_tokens": 10, "output_tokens": 2, "cache_hits": 3},
            "future_field": {"a": 1},
        }))
        .unwrap();
        assert_eq!(r.model, "jev-1.13.0");
        let u = r.usage.expect("usage");
        assert_eq!((u.input_tokens, u.output_tokens), (10, 2));

        // usage 字段缺省 → 全 0;整体缺省 → 默认值
        let r: JudgeResponse = serde_json::from_value(json!({"usage": {}})).unwrap();
        let u = r.usage.expect("usage");
        assert_eq!((u.input_tokens, u.output_tokens), (0, 0));
        let r: JudgeResponse = serde_json::from_value(json!({})).unwrap();
        assert!(r.model.is_empty() && r.answers.is_empty() && r.usage.is_none());
    }

    #[test]
    fn confidence_absent_or_non_numeric_defaults_to_zero() {
        // 缺 confidence → 0.0(不进 partial),score 仍正常解码
        let mut a = full_answers();
        a["c0.quality_fit"] = json!({"type":"score","score":0.8});
        let d = decode_answers(&resp(a), 2);
        let q = d.per_candidate[0].quality.as_ref().expect("c0.quality_fit");
        assert_eq!(q.raw_score, 0.8);
        assert_eq!(q.confidence, 0.0);
        assert!(d.partial.is_empty(), "partial: {:?}", d.partial);

        // confidence 非数值(字符串)→ 0.0(不进 partial)
        let mut a = full_answers();
        a["c1.cost_fit"] = json!({"type":"score","score":2.0,"confidence":"0.9"});
        let d = decode_answers(&resp(a), 2);
        let c = d.per_candidate[1].cost.as_ref().expect("c1.cost_fit");
        assert_eq!(c.raw_score, 2.0);
        assert_eq!(c.confidence, 0.0);
        assert!(d.partial.is_empty(), "partial: {:?}", d.partial);
    }

    #[test]
    fn decode_count_edges() {
        // (a) 答案只有 2 个候选,却按 4 个候选解码 → c2/c3 逐 ID 记录且顺序确定
        let d = decode_answers(&resp(full_answers()), 4);
        assert_eq!(d.per_candidate.len(), 4);
        assert!(d.per_candidate[2].quality.is_none());
        assert!(d.per_candidate[2].cost.is_none());
        assert!(d.per_candidate[3].quality.is_none());
        assert!(d.per_candidate[3].cost.is_none());
        assert_eq!(
            d.partial,
            vec![
                "c2.quality_fit".to_string(),
                "c2.cost_fit".to_string(),
                "c3.quality_fit".to_string(),
                "c3.cost_fit".to_string(),
            ]
        );

        // (b) candidate_count = 0 → 不产出候选维度,也不记录缺失
        let d = decode_answers(&resp(full_answers()), 0);
        assert!(d.per_candidate.is_empty());
        assert!(d.partial.is_empty(), "partial: {:?}", d.partial);

        // (c) 未被请求的额外答案(c1.*、c9.*)被忽略,不产生 partial
        let d = decode_answers(&resp(full_answers()), 1);
        assert_eq!(d.per_candidate.len(), 1);
        assert!(d.partial.is_empty(), "partial: {:?}", d.partial);

        let mut a = full_answers();
        a["c9.quality_fit"] = json!({"type":"score","score":0.5,"confidence":0.5});
        let d = decode_answers(&resp(a), 1);
        assert_eq!(d.per_candidate.len(), 1);
        assert!(d.partial.is_empty(), "partial: {:?}", d.partial);
    }
}
