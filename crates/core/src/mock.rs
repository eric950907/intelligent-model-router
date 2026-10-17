//! 确定性 Mock 后端: 供测试与离线开发(spec M0“mock 后端”)。
//! 行为: 按问题 ID 生成固定合法答案;不认识的问题不给答案。

use crate::judgment::*;
use futures::future::BoxFuture;
use serde_json::json;

pub struct MockBackend {
    pub backend_id: String,
}

impl MockBackend {
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            backend_id: id.into(),
        }
    }
}

impl JudgmentBackend for MockBackend {
    fn id(&self) -> &str {
        &self.backend_id
    }

    fn judge<'a>(&'a self, req: JudgeRequest) -> BoxFuture<'a, Result<JudgeResponse, JudgeError>> {
        Box::pin(async move {
            let mut answers = serde_json::Map::new();
            for id in req.questions.keys() {
                let ans = match id.as_str() {
                    "task_type" => json!({"type":"choice","choice":"chat",
                        "probabilities":{"chat":0.9},"confidence":0.9}),
                    "difficulty" => json!({"type":"score","score":1.0,
                        "legend":{"0":"简单","1":"中等","2":"困难"},
                        "probabilities":{"0":0.2,"1":0.6,"2":0.2},"confidence":0.8}),
                    "frontier_needed" => json!({"type":"noul","noul":0.3}),
                    _ if id.ends_with(".quality_fit") => json!({"type":"score","score":1.5,
                        "legend":{},"probabilities":{},"confidence":0.7}),
                    _ if id.ends_with(".cost_fit") => json!({"type":"score","score":1.0,
                        "legend":{},"probabilities":{},"confidence":0.7}),
                    _ => continue,
                };
                answers.insert(id.clone(), ans);
            }
            Ok(JudgeResponse {
                model: "mock-1".into(),
                answers: answers.into_iter().collect(),
                usage: Some(WireUsage {
                    input_tokens: 300,
                    output_tokens: 10,
                }),
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::QuestionType;
    use crate::judgment::decode_answers;
    use serde_json::json;
    use std::collections::BTreeMap;

    fn q(qtype: QuestionType) -> Question {
        Question {
            qtype,
            instructions: json!("test"),
            criteria: None,
        }
    }

    /// 3 个共享问题 + 1 候选 × 2 维 + 1 个未知 id。
    fn full_request() -> JudgeRequest {
        let questions = BTreeMap::from([
            ("task_type".to_string(), q(QuestionType::Choice)),
            ("difficulty".to_string(), q(QuestionType::Score)),
            ("frontier_needed".to_string(), q(QuestionType::Noul)),
            ("c0.quality_fit".to_string(), q(QuestionType::Score)),
            ("c0.cost_fit".to_string(), q(QuestionType::Score)),
            ("whatever".to_string(), q(QuestionType::Choice)),
        ]);
        JudgeRequest {
            model: String::new(),
            state: json!({"task": "hello"}),
            questions,
        }
    }

    #[tokio::test]
    async fn answers_known_ids_and_skips_unknown() {
        let backend = MockBackend::new("mock-a");
        assert_eq!(backend.id(), "mock-a");

        let resp = backend.judge(full_request()).await.expect("mock 不应报错");
        assert_eq!(resp.model, "mock-1");
        let usage = resp.usage.expect("usage");
        assert_eq!((usage.input_tokens, usage.output_tokens), (300, 10));

        // 5 类已知 id 全部有答案;未知 id 无答案。
        assert_eq!(resp.answers.len(), 5, "answers: {:?}", resp.answers);
        assert!(
            !resp.answers.contains_key("whatever"),
            "未知 id 不应有答案: {:?}",
            resp.answers
        );
        for id in [
            "task_type",
            "difficulty",
            "frontier_needed",
            "c0.quality_fit",
            "c0.cost_fit",
        ] {
            assert!(resp.answers.contains_key(id), "缺少答案: {id}");
        }

        // 各答案形状与固定值。
        assert_eq!(
            resp.answers["task_type"],
            json!({"type":"choice","choice":"chat",
                   "probabilities":{"chat":0.9},"confidence":0.9})
        );
        assert_eq!(
            resp.answers["difficulty"],
            json!({"type":"score","score":1.0,
                   "legend":{"0":"简单","1":"中等","2":"困难"},
                   "probabilities":{"0":0.2,"1":0.6,"2":0.2},"confidence":0.8})
        );
        assert_eq!(
            resp.answers["frontier_needed"],
            json!({"type":"noul","noul":0.3})
        );
        assert_eq!(
            resp.answers["c0.quality_fit"],
            json!({"type":"score","score":1.5,
                   "legend":{},"probabilities":{},"confidence":0.7})
        );
        assert_eq!(
            resp.answers["c0.cost_fit"],
            json!({"type":"score","score":1.0,
                   "legend":{},"probabilities":{},"confidence":0.7})
        );
    }

    #[tokio::test]
    async fn deterministic_across_calls() {
        let backend = MockBackend::new("mock-a");
        let a = backend.judge(full_request()).await.unwrap();
        let b = backend.judge(full_request()).await.unwrap();
        assert_eq!(
            serde_json::to_value(&a).unwrap(),
            serde_json::to_value(&b).unwrap()
        );
    }

    #[tokio::test]
    async fn decodes_fully() {
        let backend = MockBackend::new("mock-a");
        let resp = backend.judge(full_request()).await.unwrap();
        let d = decode_answers(&resp, 1);

        assert!(d.partial.is_empty(), "partial: {:?}", d.partial);
        assert_eq!(d.shared.task_type.as_deref(), Some("chat"));
        assert_eq!(d.shared.difficulty, Some(1.0));
        assert_eq!(d.shared.frontier_needed, Some(0.3));

        assert_eq!(d.per_candidate.len(), 1);
        let quality = d.per_candidate[0].quality.as_ref().expect("c0.quality_fit");
        assert_eq!(quality.raw_score, 1.5);
        assert_eq!(quality.confidence, 0.7);
        assert!(quality.legend.is_empty());
        assert!(quality.probabilities.is_empty());
        let cost = d.per_candidate[0].cost.as_ref().expect("c0.cost_fit");
        assert_eq!(cost.raw_score, 1.0);
        assert_eq!(cost.confidence, 0.7);
    }
}
