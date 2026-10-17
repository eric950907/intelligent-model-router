//! HTTP API 的请求/响应 DTO(spec §8.1)与错误码(spec §8.2)。

use serde::{Deserialize, Serialize};

pub const MAX_TOP_K: u32 = 50;

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RecommendRequest {
    /// 原始网关请求体(OpenAI/Claude),原样透传;解析在 ProfileExtractor 内做。
    pub request: serde_json::Value,
    #[serde(default)]
    pub constraints: Constraints,
    #[serde(default)]
    pub options: Options,
}

impl RecommendRequest {
    pub fn validate(&self) -> Result<(), String> {
        if let Some(b) = &self.constraints.budget {
            if let Some(p) = b.max_input_price_per_mtok {
                if !p.is_finite() || p < 0.0 {
                    return Err(
                        "constraints.budget.max_input_price_per_mtok 必须为非负有限数".into(),
                    );
                }
            }
        }
        if let Some(k) = self.options.top_k {
            if k == 0 || k > MAX_TOP_K {
                return Err(format!("options.top_k 应在 1..={MAX_TOP_K}"));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Constraints {
    #[serde(default)]
    pub budget: Option<BudgetConstraint>,
    #[serde(default)]
    pub require_tags: Vec<String>,
    #[serde(default)]
    pub vendor_allow: Vec<String>,
    #[serde(default)]
    pub vendor_deny: Vec<String>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BudgetConstraint {
    #[serde(default)]
    pub max_input_price_per_mtok: Option<f64>,
    #[serde(default)]
    pub cost_sensitive: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Options {
    #[serde(default)]
    pub top_k: Option<u32>,
    #[serde(default)]
    pub locale: Locale,
    #[serde(default)]
    pub include_debug: bool,
}
impl Default for Options {
    fn default() -> Self {
        Self {
            top_k: None,
            locale: Locale::Zh,
            include_debug: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Locale {
    #[default]
    Zh,
    En,
}
impl Locale {
    pub fn as_str(&self) -> &'static str {
        match self {
            Locale::Zh => "zh",
            Locale::En => "en",
        }
    }
}

// ---- 响应(spec §8.1)----

#[derive(Debug, Clone, Serialize)]
pub struct RecommendResponse {
    pub recommendations: Vec<Recommendation>,
    pub profile: ProfileView,
    pub degraded: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub debug: Option<serde_json::Value>,
    pub meta: Meta,
}

#[derive(Debug, Clone, Serialize)]
pub struct Recommendation {
    pub model: String,
    pub rank: u32,
    pub score: f32,
    pub confidence: Option<f32>,
    pub low_confidence: bool,
    pub breakdown: Breakdown,
    pub reasons: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Breakdown {
    pub quality_fit: Option<f32>,
    pub cost_fit: Option<f32>,
    pub weights: Option<WeightsOut>,
    pub penalty: Option<f32>,
    pub rule_hits: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub coarse: Option<CoarseOut>,
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct WeightsOut {
    pub quality_fit: f32,
    pub cost_fit: f32,
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct CoarseOut {
    pub tag_overlap: f32,
    pub price_score: f32,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProfileView {
    pub task_type: Option<String>,
    pub difficulty: Option<f32>,
    pub frontier_needed: Option<f32>,
    pub est_tokens: u32,
    pub has_images: bool,
    pub tools_count: u32,
    pub json_mode: bool,
    pub locale_hint: Option<String>,
    pub task_hints: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Meta {
    pub backend: String,
    pub latency_ms: u64,
    pub cache_hit: bool,
    pub catalog_version: String,
    pub weights_version: String,
    pub usage: Option<UsageOut>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub filtered: Option<Vec<FilteredByRule>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub degraded_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub partial_answers: Option<Vec<String>>,
    pub request_id: String,
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct UsageOut {
    pub input_tokens: u64,
    pub output_tokens: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct FilteredByRule {
    pub rule: String,
    pub count: u32,
}

// ---- 错误码(spec §8.2)----

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApiErrorCode {
    InvalidJson,
    RequestTooLarge,
    InvalidRequestBody,
    InvalidConstraints,
    UpstreamError,
    RequestTimeout,
    InternalError,
}
impl ApiErrorCode {
    pub fn as_str(&self) -> &'static str {
        match self {
            ApiErrorCode::InvalidJson => "invalid_json",
            ApiErrorCode::RequestTooLarge => "request_too_large",
            ApiErrorCode::InvalidRequestBody => "invalid_request_body",
            ApiErrorCode::InvalidConstraints => "invalid_constraints",
            ApiErrorCode::UpstreamError => "upstream_error",
            ApiErrorCode::RequestTimeout => "request_timeout",
            ApiErrorCode::InternalError => "internal_error",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn envelope_defaults() {
        let r: RecommendRequest = serde_json::from_str(r#"{"request":{"messages":[]}}"#).unwrap();
        assert!(r.constraints.budget.is_none());
        assert_eq!(r.options.top_k, None);
        assert_eq!(r.options.locale, Locale::Zh);
        assert!(!r.options.include_debug);
        r.validate().unwrap();
    }

    #[test]
    fn unknown_field_rejected() {
        let e = serde_json::from_str::<RecommendRequest>(r#"{"request":{},"oops":1}"#);
        assert!(e.is_err());
    }

    #[test]
    fn validate_rejects_bad_values() {
        let mut r: RecommendRequest =
            serde_json::from_str(r#"{"request":{},"options":{"top_k":0}}"#).unwrap();
        assert!(r.validate().unwrap_err().contains("top_k"));
        r.options.top_k = None;
        r.constraints.budget = Some(BudgetConstraint {
            max_input_price_per_mtok: Some(-1.0),
            cost_sensitive: false,
        });
        assert!(r
            .validate()
            .unwrap_err()
            .contains("max_input_price_per_mtok"));
    }

    #[test]
    fn response_json_matches_spec_field_names() {
        // 与 spec §8.1 响应示例逐字段对齐(存在性检查)
        let resp = RecommendResponse {
            recommendations: vec![Recommendation {
                model: "gpt-5.4".into(),
                rank: 1,
                score: 0.773,
                confidence: Some(0.61),
                low_confidence: false,
                breakdown: Breakdown {
                    quality_fit: Some(0.92),
                    cost_fit: Some(0.50),
                    weights: Some(WeightsOut {
                        quality_fit: 0.65,
                        cost_fit: 0.35,
                    }),
                    penalty: Some(0.0),
                    rule_hits: vec!["capability:tools✓".into()],
                    coarse: None,
                },
                reasons: vec!["✅ …".into()],
            }],
            profile: ProfileView {
                task_type: Some("code_debug".into()),
                difficulty: Some(1.2),
                frontier_needed: Some(0.3),
                est_tokens: 8200,
                has_images: false,
                tools_count: 3,
                json_mode: false,
                locale_hint: Some("zh".into()),
                task_hints: vec![],
            },
            degraded: false,
            debug: None,
            meta: Meta {
                backend: "jev-1.13.0".into(),
                latency_ms: 412,
                cache_hit: false,
                catalog_version: "a3f9".into(),
                weights_version: "b21c".into(),
                usage: Some(UsageOut {
                    input_tokens: 5210,
                    output_tokens: 12,
                }),
                filtered: None,
                degraded_reason: None,
                partial_answers: None,
                request_id: "req-1".into(),
            },
        };
        let v = serde_json::to_value(&resp).unwrap();
        for key in ["recommendations", "profile", "degraded", "meta"] {
            assert!(v.get(key).is_some(), "missing {key}");
        }
        let rec = &v["recommendations"][0];
        for key in [
            "model",
            "rank",
            "score",
            "confidence",
            "low_confidence",
            "breakdown",
            "reasons",
        ] {
            assert!(rec.get(key).is_some(), "missing recommendations[0].{key}");
        }
        let meta = &v["meta"];
        for key in [
            "backend",
            "latency_ms",
            "cache_hit",
            "catalog_version",
            "weights_version",
            "usage",
        ] {
            assert!(meta.get(key).is_some(), "missing meta.{key}");
        }
        // include_debug=false 时 debug 字段不出现
        assert!(v.get("debug").is_none());
    }

    #[test]
    fn error_codes_frozen() {
        assert_eq!(ApiErrorCode::InvalidJson.as_str(), "invalid_json");
        assert_eq!(ApiErrorCode::RequestTooLarge.as_str(), "request_too_large");
        assert_eq!(
            ApiErrorCode::InvalidRequestBody.as_str(),
            "invalid_request_body"
        );
        assert_eq!(
            ApiErrorCode::InvalidConstraints.as_str(),
            "invalid_constraints"
        );
        assert_eq!(ApiErrorCode::UpstreamError.as_str(), "upstream_error");
        assert_eq!(ApiErrorCode::RequestTimeout.as_str(), "request_timeout");
        assert_eq!(ApiErrorCode::InternalError.as_str(), "internal_error");
    }
}
