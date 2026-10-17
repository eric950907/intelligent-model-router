//! rules/questions/server 配置的 serde 模型与加载(spec §5);catalog 由 server.yaml 的
//! `catalog` 段路径装配(LiteLLM model_list + enrichment)。所有校验错误聚合返回,便于 validate-catalog 全量打印。

use crate::catalog::Catalog;
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, thiserror::Error)]
#[error("配置加载失败:\n{}", .0.join("\n"))]
pub struct ConfigErrors(pub Vec<String>);

#[derive(Debug)]
pub struct LoadedConfigs {
    pub catalog: Catalog,
    pub rules: RulesConfig,
    pub questions: QuestionsConfig,
    pub server: ServerConfig,
}

fn read_file(dir: &Path, name: &str, errs: &mut Vec<String>) -> Option<String> {
    match std::fs::read_to_string(dir.join(name)) {
        Ok(s) => Some(s),
        Err(e) => {
            errs.push(format!("{name}: 读取失败: {e}"));
            None
        }
    }
}

pub fn load_configs(dir: &Path) -> Result<LoadedConfigs, ConfigErrors> {
    let mut errs = Vec::new();

    // ① server.yaml 先读:其 catalog 段决定 model_list / enrichment 的路径
    let server =
        read_file(dir, "server.yaml", &mut errs).and_then(|s| match ServerConfig::from_yaml(&s) {
            Ok(v) => {
                errs.extend(
                    v.validate()
                        .into_iter()
                        .map(|e| format!("server.yaml: {e}")),
                );
                Some(v)
            }
            Err(e) => {
                errs.push(format!("server.yaml: {e}"));
                None
            }
        });

    // ② catalog:相对路径基于 configs 目录解析;绝对路径原样(可直达网关真实配置)
    let catalog = server.as_ref().and_then(|srv| {
        let ml = resolve_config_path(dir, &srv.catalog.model_list_path);
        let en = resolve_config_path(dir, &srv.catalog.enrichment_path);
        match Catalog::load(&ml, &en) {
            Ok(c) => Some(c),
            Err(e) => {
                errs.push(format!("catalog: {e}"));
                None
            }
        }
    });

    // ③ rules / questions(独立于 server.yaml)
    let rules =
        read_file(dir, "rules.yaml", &mut errs).and_then(|s| match RulesConfig::from_yaml(&s) {
            Ok(v) => {
                errs.extend(v.validate().into_iter().map(|e| format!("rules.yaml: {e}")));
                Some(v)
            }
            Err(e) => {
                errs.push(format!("rules.yaml: {e}"));
                None
            }
        });
    let questions =
        read_file(dir, "questions.yaml", &mut errs).and_then(|s| match QuestionsConfig::from_yaml(
            &s,
        ) {
            Ok(v) => {
                errs.extend(
                    v.validate()
                        .into_iter()
                        .map(|e| format!("questions.yaml: {e}")),
                );
                Some(v)
            }
            Err(e) => {
                errs.push(format!("questions.yaml: {e}"));
                None
            }
        });

    match (catalog, rules, questions, server) {
        (Some(catalog), Some(rules), Some(questions), Some(server)) if errs.is_empty() => {
            Ok(LoadedConfigs {
                catalog,
                rules,
                questions,
                server,
            })
        }
        _ => Err(ConfigErrors(errs)),
    }
}

/// 相对路径基于 configs 目录;绝对路径原样(spec §5.4)。
fn resolve_config_path(dir: &Path, p: &str) -> std::path::PathBuf {
    let path = Path::new(p);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        dir.join(path)
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RulesConfig {
    pub rules: Vec<RuleSpec>,
    #[serde(default)]
    pub soft_penalties: SoftPenalties,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuleSpec {
    pub kind: RuleKind,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub safety_factor: Option<f64>, // 仅 context_length 用;缺省 1.15
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleKind {
    Capability,
    ContextLength,
    Budget,
    Vendor,
    TagRequire,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SoftPenalties {
    #[serde(default)]
    pub vendor_preference: VendorPreference, // v1.1 保留字段:校验格式、不消费
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VendorPreference {
    #[serde(default)]
    pub white: Vec<String>,
    #[serde(default = "default_value_each")]
    pub value_each: f64,
}

fn default_value_each() -> f64 {
    0.05
}
impl Default for VendorPreference {
    fn default() -> Self {
        Self {
            white: vec![],
            value_each: 0.05,
        }
    }
}

impl RulesConfig {
    pub fn from_yaml(raw: &str) -> Result<Self, serde_yaml::Error> {
        serde_yaml::from_str(raw)
    }
    pub fn validate(&self) -> Vec<String> {
        let mut errs = vec![];
        for (i, r) in self.rules.iter().enumerate() {
            if r.enabled && r.kind == RuleKind::ContextLength {
                if let Some(sf) = r.safety_factor {
                    if !(1.0..=3.0).contains(&sf) {
                        errs.push(format!("rules[{i}]: safety_factor 应在 [1.0, 3.0]"));
                    }
                }
            }
        }
        let vp = &self.soft_penalties.vendor_preference;
        if !(0.0..=1.0).contains(&vp.value_each) {
            errs.push("soft_penalties.vendor_preference.value_each 应在 [0,1]".into());
        }
        errs
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QuestionsConfig {
    pub shared: std::collections::BTreeMap<String, QuestionSpec>,
    pub per_candidate: PerCandidateQuestions,
    pub price_bands: PriceBands,
    pub weights: WeightsConfig,
    pub confidence: ConfidenceConfig,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QuestionSpec {
    #[serde(rename = "type")]
    pub qtype: QuestionType,
    pub instructions: serde_json::Value, // string|object|array 原样透传
    #[serde(default)]
    pub criteria: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QuestionType {
    Choice,
    Score,
    Noul,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PerCandidateQuestions {
    pub quality_fit: QuestionSpec,
    pub cost_fit: QuestionSpec,
}

// weights / confidence / price_bands 相关类型额外 derive Serialize —— Task 14 用
// serde_json::to_value 生成 weights_version 的 canonical JSON(其余字段仍只读)。
#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PriceBands {
    pub cheap_max: f64,
    pub mid_max: f64,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WeightsConfig {
    pub base: BaseWeights,
    #[serde(default)]
    pub adjustments: Vec<Adjustment>,
    pub clamp: Clamp,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BaseWeights {
    pub quality_fit: f32,
    pub cost_fit: f32,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Clamp {
    pub min: f32,
    pub max: f32,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Adjustment {
    pub when: Conditions,
    pub set: std::collections::BTreeMap<String, DeltaValue>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Conditions {
    #[serde(default)]
    pub difficulty_at_least: Option<f32>,
    #[serde(default)]
    pub frontier_needed_above: Option<f32>,
    #[serde(default)]
    pub budget_present: Option<bool>,
    #[serde(default)]
    pub cost_sensitive: Option<bool>,
}

/// `"+0.10"` / `"-0.10"` 字符串或数字。
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(untagged)]
pub enum DeltaValue {
    Num(f32),
    Str(String),
}

impl DeltaValue {
    pub fn delta(&self) -> Result<f32, String> {
        match self {
            DeltaValue::Num(n) => Ok(*n),
            DeltaValue::Str(s) => {
                let t = s.trim();
                if let Some(rest) = t.strip_prefix('+') {
                    rest.trim().parse::<f32>().map_err(|e| format!("{s}: {e}"))
                } else {
                    t.parse::<f32>().map_err(|e| format!("{s}: {e}"))
                }
            }
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ConfidenceConfig {
    pub flag_below: f32,
}

impl QuestionsConfig {
    pub fn from_yaml(raw: &str) -> Result<Self, serde_yaml::Error> {
        serde_yaml::from_str(raw)
    }
    pub fn validate(&self) -> Vec<String> {
        let mut errs = vec![];
        let check =
            |name: &str, q: &QuestionSpec, errs: &mut Vec<String>| match (q.qtype, &q.criteria) {
                (QuestionType::Choice, Some(serde_json::Value::Object(_))) => {}
                (QuestionType::Choice, _) => {
                    errs.push(format!("{name}: choice 必须提供 map 形式的 criteria"))
                }
                (QuestionType::Score, Some(serde_json::Value::Array(a)))
                    if (2..=10).contains(&a.len()) => {}
                (QuestionType::Score, Some(serde_json::Value::Array(a))) => errs.push(format!(
                    "{name}: score criteria 档位数 {} 应在 2..=10",
                    a.len()
                )),
                (QuestionType::Score, _) => {
                    errs.push(format!("{name}: score 必须提供数组 criteria"))
                }
                (QuestionType::Noul, _) => {}
            };
        for (id, q) in &self.shared {
            check(&format!("shared.{id}"), q, &mut errs);
        }
        // Task 11 的 decode 硬编码这三个共享问题 ID 与类型;缺失/类型不符会在运行时静默退化为 partial_answers。
        for (id, ty) in [
            ("task_type", QuestionType::Choice),
            ("difficulty", QuestionType::Score),
            ("frontier_needed", QuestionType::Noul),
        ] {
            match self.shared.get(id) {
                Some(q) if q.qtype == ty => {}
                Some(q) => errs.push(format!(
                    "shared.{id}: 类型必须为 {ty:?}(当前 {:?})",
                    q.qtype
                )),
                None => errs.push(format!("shared.{id}: 缺失(融合依赖该问题)")),
            }
        }
        check(
            "per_candidate.quality_fit",
            &self.per_candidate.quality_fit,
            &mut errs,
        );
        check(
            "per_candidate.cost_fit",
            &self.per_candidate.cost_fit,
            &mut errs,
        );

        // Task 11 的 decode 硬编码 per_candidate 为 score(按档位归一化);非 score 会静默退化为 0.5。
        for (name, q) in [
            ("per_candidate.quality_fit", &self.per_candidate.quality_fit),
            ("per_candidate.cost_fit", &self.per_candidate.cost_fit),
        ] {
            if q.qtype != QuestionType::Score {
                errs.push(format!("{name}: 必须为 score(融合按档位归一化)"));
            }
        }

        let pb = &self.price_bands;
        if !(pb.cheap_max.is_finite() && pb.mid_max.is_finite() && pb.cheap_max <= pb.mid_max) {
            errs.push("price_bands: 需满足 cheap_max ≤ mid_max 且为有限数".into());
        }
        let w = &self.weights;
        if !w.base.quality_fit.is_finite() || !w.base.cost_fit.is_finite() {
            errs.push("weights.base: 必须为有限数".into());
        }
        if w.base.quality_fit < 0.0
            || w.base.cost_fit < 0.0
            || w.base.quality_fit + w.base.cost_fit <= 0.0
        {
            errs.push("weights.base: 需为非负且和 > 0".into());
        }
        if !(0.0..=1.0).contains(&w.clamp.min)
            || !(0.0..=1.0).contains(&w.clamp.max)
            || w.clamp.min > w.clamp.max
        {
            errs.push("weights.clamp: 需满足 0 ≤ min ≤ max ≤ 1".into());
        }
        if !(0.0..=1.0).contains(&self.confidence.flag_below) {
            errs.push("confidence.flag_below 应在 [0,1]".into());
        }
        for (i, adj) in w.adjustments.iter().enumerate() {
            let conds = [
                adj.when.difficulty_at_least.is_some(),
                adj.when.frontier_needed_above.is_some(),
                adj.when.budget_present.is_some(),
                adj.when.cost_sensitive.is_some(),
            ];
            if !conds.iter().any(|x| *x) {
                errs.push(format!("weights.adjustments[{i}]: when 至少需要一个条件"));
            }
            for (k, v) in &adj.set {
                if !matches!(k.as_str(), "quality_fit" | "cost_fit") {
                    errs.push(format!(
                        "weights.adjustments[{i}]: set 键 `{k}` 非法(仅 quality_fit/cost_fit)"
                    ));
                }
                match v.delta() {
                    Err(e) => errs.push(format!("weights.adjustments[{i}]: delta 解析失败 {e}")),
                    Ok(d) if !d.is_finite() => errs.push(format!(
                        "weights.adjustments[{i}]: delta 必须为有限数(键 {k})"
                    )),
                    Ok(_) => {}
                }
            }
        }
        errs
    }

    /// 归一化分母:档位数-1(score 问题)。spec §6.4。
    pub fn quality_levels(&self) -> usize {
        match &self.per_candidate.quality_fit.criteria {
            Some(serde_json::Value::Array(a)) => a.len(),
            _ => 1,
        }
    }
    pub fn cost_levels(&self) -> usize {
        match &self.per_candidate.cost_fit.criteria {
            Some(serde_json::Value::Array(a)) => a.len(),
            _ => 1,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerConfig {
    pub server: ServerOpts,
    #[serde(default)]
    pub catalog: CatalogConfig,
    pub backends: Vec<BackendSpec>,
    #[serde(default = "default_templates_dir")]
    pub templates_dir: String,
    #[serde(default)]
    pub cache: CacheConfig,
    #[serde(default)]
    pub limits: LimitsConfig,
}

fn default_templates_dir() -> String {
    "templates".into()
}

/// catalog 文件路径(spec §5.4)。相对路径基于 configs 目录;绝对路径直达网关真实配置。
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CatalogConfig {
    #[serde(default = "default_model_list_path")]
    pub model_list_path: String,
    #[serde(default = "default_enrichment_path")]
    pub enrichment_path: String,
}

fn default_model_list_path() -> String {
    "model_list.yaml".into()
}
fn default_enrichment_path() -> String {
    "enrichment.yaml".into()
}

impl Default for CatalogConfig {
    fn default() -> Self {
        Self {
            model_list_path: default_model_list_path(),
            enrichment_path: default_enrichment_path(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerOpts {
    pub listen: String,
    #[serde(default)]
    pub client_auth_token_env: Option<String>,
    #[serde(default)]
    pub admin_token_env: Option<String>,
    #[serde(default)]
    pub metrics_enabled: bool,
    #[serde(default = "default_true")]
    pub debug_page_enabled: bool,
    #[serde(default = "default_max_body")]
    pub max_request_bytes: usize,
    #[serde(default = "default_request_timeout")]
    pub request_timeout_ms: u64,
}

fn default_max_body() -> usize {
    4 * 1024 * 1024
}
fn default_request_timeout() -> u64 {
    15_000
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BackendSpec {
    pub id: String,
    pub kind: BackendKind,
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub api_key_env: Option<String>,
    #[serde(default)]
    pub proxy: Option<String>,
    #[serde(default = "default_timeout_ms")]
    pub timeout_ms: u64,
    #[serde(default = "default_total_budget_ms")]
    pub total_budget_ms: u64,
    #[serde(default = "default_max_inflight")]
    pub max_inflight: usize,
    #[serde(default)]
    pub retry: RetrySpec,
    #[serde(default)]
    pub long_context_header: bool,
}

fn default_timeout_ms() -> u64 {
    5000
}
fn default_total_budget_ms() -> u64 {
    8000
}
fn default_max_inflight() -> usize {
    16
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackendKind {
    Systemone,
    Mock,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetrySpec {
    #[serde(default = "default_max_attempts")]
    pub max_attempts: u32,
    #[serde(default = "default_base_delay")]
    pub base_delay_ms: u64,
    #[serde(default = "default_true")]
    pub honor_retry_after: bool,
}
fn default_max_attempts() -> u32 {
    2
}
fn default_base_delay() -> u64 {
    250
}
impl Default for RetrySpec {
    fn default() -> Self {
        Self {
            max_attempts: 2,
            base_delay_ms: 250,
            honor_retry_after: true,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CacheConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_ttl")]
    pub ttl_secs: u64,
    #[serde(default = "default_cache_entries")]
    pub max_entries: u64,
}
fn default_ttl() -> u64 {
    600
}
fn default_cache_entries() -> u64 {
    2048
}
impl Default for CacheConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            ttl_secs: 600,
            max_entries: 2048,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LimitsConfig {
    #[serde(default = "default_max_in_call")]
    pub max_candidates_in_call: usize,
    #[serde(default = "default_max_judged")]
    pub max_candidates_judged: usize,
    #[serde(default = "default_top_k")]
    pub default_top_k: u32,
}
fn default_max_in_call() -> usize {
    30
}
fn default_max_judged() -> usize {
    60
}
fn default_top_k() -> u32 {
    5
}
impl Default for LimitsConfig {
    fn default() -> Self {
        Self {
            max_candidates_in_call: 30,
            max_candidates_judged: 60,
            default_top_k: 5,
        }
    }
}

impl ServerConfig {
    pub fn from_yaml(raw: &str) -> Result<Self, serde_yaml::Error> {
        serde_yaml::from_str(raw)
    }
    pub fn validate(&self) -> Vec<String> {
        let mut errs = vec![];
        for (i, b) in self.backends.iter().enumerate() {
            if b.kind == BackendKind::Systemone {
                if b.base_url.as_deref().unwrap_or("").trim().is_empty() {
                    errs.push(format!(
                        "backends[{i}] ({}): kind=systemone 需要 base_url",
                        b.id
                    ));
                }
                if b.model.as_deref().unwrap_or("").trim().is_empty() {
                    errs.push(format!(
                        "backends[{i}] ({}): kind=systemone 需要 model",
                        b.id
                    ));
                }
            }
        }
        let mut seen = std::collections::HashSet::new();
        for b in &self.backends {
            if !seen.insert(b.id.clone()) {
                errs.push(format!("backends: duplicate id {}", b.id));
            }
        }
        let l = &self.limits;
        if l.max_candidates_in_call == 0 || l.max_candidates_judged < l.max_candidates_in_call {
            errs.push("limits: 需满足 0 < max_candidates_in_call ≤ max_candidates_judged".into());
        }
        if l.default_top_k == 0 {
            errs.push("limits.default_top_k 必须 > 0".into());
        }
        errs
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_repo_configs() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../configs");
        let cfg = load_configs(&dir).expect("仓库 configs 必须可加载");
        assert!(cfg.catalog.models.iter().any(|m| m.id == "gpt-5.4"));
        assert!(
            !cfg.catalog.skipped.is_empty(),
            "示例 model_list 应含被排除的非 chat 条目"
        );
        assert_eq!(cfg.server.catalog.model_list_path, "model_list.yaml");
        assert_eq!(cfg.rules.rules.len(), 5);
        assert!(cfg.questions.shared.len() >= 3);
        assert!(!cfg.server.backends.is_empty());
        assert_eq!(cfg.server.limits.default_top_k, 5);
    }

    #[test]
    fn missing_file_reported_with_name() {
        let dir = std::env::temp_dir().join("mrc-test-empty-configs-1");
        let _ = std::fs::create_dir_all(&dir);
        let err = load_configs(&dir).unwrap_err().to_string();
        assert!(err.contains("server.yaml"), "got: {err}");
        assert!(err.contains("rules.yaml"), "got: {err}");
    }

    #[test]
    fn bad_rule_kind_rejected() {
        let y = "rules:\n  - kind: nope\n";
        let err = serde_yaml::from_str::<RulesConfig>(y)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("nope") || err.contains("unknown variant"),
            "got: {err}"
        );
    }

    #[test]
    fn score_criteria_bounds_validated() {
        let mut q =
            QuestionsConfig::from_yaml(include_str!("../../../configs/questions.yaml")).unwrap();
        q.per_candidate.quality_fit.criteria = Some(serde_json::json!(["only-one"]));
        let errs = q.validate();
        assert!(
            errs.iter().any(|e| e.contains("quality_fit")),
            "got: {errs:?}"
        );
    }

    #[test]
    fn adjustment_delta_parsing() {
        let q =
            QuestionsConfig::from_yaml(include_str!("../../../configs/questions.yaml")).unwrap();
        let w = &q.weights;
        assert_eq!(w.base.quality_fit, 0.75);
        let deltas: Vec<f32> = w.adjustments[0]
            .set
            .values()
            .map(|d| d.delta().unwrap())
            .collect();
        assert!(deltas.contains(&0.10) && deltas.contains(&-0.10));
    }

    #[test]
    fn per_candidate_must_be_score() {
        let mut q =
            QuestionsConfig::from_yaml(include_str!("../../../configs/questions.yaml")).unwrap();
        q.per_candidate.quality_fit.qtype = QuestionType::Choice;
        q.per_candidate.quality_fit.criteria = Some(serde_json::json!({ "a": "x" }));
        let errs = q.validate();
        assert!(
            errs.iter().any(|e| e.contains("per_candidate.quality_fit")),
            "got: {errs:?}"
        );
    }

    #[test]
    fn non_finite_weights_and_deltas_rejected() {
        let mut q =
            QuestionsConfig::from_yaml(include_str!("../../../configs/questions.yaml")).unwrap();
        q.weights.base.quality_fit = f32::NAN;
        q.weights.adjustments[0]
            .set
            .insert("cost_fit".into(), DeltaValue::Num(f32::INFINITY));
        let errs = q.validate();
        assert!(
            errs.iter().any(|e| e.contains("weights.base")),
            "got: {errs:?}"
        );
        assert!(
            errs.iter()
                .any(|e| e.contains("weights.adjustments[0]") && e.contains("有限数")),
            "got: {errs:?}"
        );
    }

    #[test]
    fn missing_shared_question_rejected() {
        let mut q =
            QuestionsConfig::from_yaml(include_str!("../../../configs/questions.yaml")).unwrap();
        q.shared.remove("difficulty");
        let errs = q.validate();
        assert!(
            errs.iter().any(|e| e.contains("shared.difficulty")),
            "got: {errs:?}"
        );
    }

    #[test]
    fn shared_question_type_checked() {
        let mut q =
            QuestionsConfig::from_yaml(include_str!("../../../configs/questions.yaml")).unwrap();
        q.shared.get_mut("task_type").unwrap().qtype = QuestionType::Score;
        let errs = q.validate();
        assert!(
            errs.iter().any(|e| e.contains("shared.task_type")),
            "got: {errs:?}"
        );
    }
}
