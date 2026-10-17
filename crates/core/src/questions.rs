//! 提问构造(spec §6.3): state、共享/每候选问题、价格档位、`<i>` 占位替换。

use crate::api::Constraints;
use crate::config::{PriceBands, QuestionSpec, QuestionsConfig};
use crate::judgment::{JudgeRequest, Question};
use crate::model::ModelEntry;
use crate::profile::Profile;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PriceBand {
    Cheap,
    Mid,
    High,
}

impl PriceBand {
    pub fn as_str(&self) -> &'static str {
        match self {
            PriceBand::Cheap => "cheap",
            PriceBand::Mid => "mid",
            PriceBand::High => "high",
        }
    }
}

/// 边界含端值:≤ cheap_max → cheap;≤ mid_max → mid;否则 high(spec §5.3)。
pub fn price_band(in_price: f64, bands: &PriceBands) -> PriceBand {
    if in_price <= bands.cheap_max {
        PriceBand::Cheap
    } else if in_price <= bands.mid_max {
        PriceBand::Mid
    } else {
        PriceBand::High
    }
}

/// 候选在 **本分片** 的视图;i 为分片内局部序号(= 问题 ID 的 idx,spec §6.2/§6.3)。
#[derive(Debug, Clone, PartialEq)]
pub struct CandidateView {
    pub i: usize,
    pub id: String,
    pub desc: String,
    pub tags: Vec<String>,
    pub ctx: u64,
    pub price_band: PriceBand,
    pub in_price: f64,
    pub out_price: f64,
}

pub fn candidate_view(i: usize, m: &ModelEntry, bands: &PriceBands) -> CandidateView {
    let desc = m.description.chars().take(200).collect::<String>();
    CandidateView {
        i,
        id: m.id.clone(),
        desc,
        tags: m.tags.clone(),
        ctx: m.context_window,
        price_band: price_band(m.price.in_per_mtok, bands),
        in_price: m.price.in_per_mtok,
        out_price: m.price.out_per_mtok,
    }
}

/// spec §6.3 的 state: profile 摘要 + request_excerpt(专用字段,不与指令混排) +
/// task_hints(非空才出现) + constraints(仅含显式项,可为空对象) + candidates。
pub fn build_state(
    profile: &Profile,
    constraints: &Constraints,
    choices: &[CandidateView],
) -> Value {
    let mut cons = serde_json::Map::new();
    if let Some(b) = &constraints.budget {
        if let Some(p) = b.max_input_price_per_mtok {
            cons.insert("budget_max_in_price".into(), json!(p));
        }
        cons.insert("cost_sensitive".into(), json!(b.cost_sensitive));
    }

    let candidates: Vec<Value> = choices
        .iter()
        .map(|c| {
            json!({
                "i": c.i, "id": c.id, "desc": c.desc, "tags": c.tags, "ctx": c.ctx,
                "price_band": c.price_band.as_str(), "in_price": c.in_price, "out_price": c.out_price,
            })
        })
        .collect();

    let mut state = serde_json::Map::new();
    state.insert(
        "profile".into(),
        json!({
            "est_tokens": profile.est_tokens,
            "has_images": profile.has_images,
            "tools_count": profile.tools_count,
            "json_mode": profile.json_mode,
            "max_tokens": profile.max_tokens,
            "locale_hint": profile.locale_hint,
        }),
    );
    state.insert("request_excerpt".into(), json!(profile.request_excerpt));
    if !profile.task_hints.is_empty() {
        state.insert("task_hints".into(), json!(profile.task_hints));
    }
    state.insert("constraints".into(), Value::Object(cons));
    state.insert("candidates".into(), Value::Array(candidates));
    Value::Object(state)
}

/// 共享问题(固定 ID)+ 每候选 Score 问题(`c{idx}.quality_fit` / `c{idx}.cost_fit`)。
pub fn build_judge_request(
    model: &str,
    profile: &Profile,
    constraints: &Constraints,
    cfg: &QuestionsConfig,
    choices: &[CandidateView],
) -> JudgeRequest {
    let mut questions: BTreeMap<String, Question> = BTreeMap::new();

    for (id, spec) in &cfg.shared {
        // 共享问题:固定 ID(task_type / difficulty / frontier_needed)
        questions.insert(id.clone(), question_from(spec, None));
    }
    for c in choices {
        for (suffix, spec) in [
            ("quality_fit", &cfg.per_candidate.quality_fit),
            ("cost_fit", &cfg.per_candidate.cost_fit),
        ] {
            questions.insert(
                format!("c{}.{}", c.i, suffix),
                question_from(spec, Some(c.i)),
            );
        }
    }

    JudgeRequest {
        model: model.to_string(),
        state: build_state(profile, constraints, choices),
        questions,
    }
}

fn question_from(spec: &QuestionSpec, idx: Option<usize>) -> Question {
    Question {
        qtype: spec.qtype,
        instructions: substitute_i(&spec.instructions, idx),
        criteria: spec.criteria.clone(),
    }
}

/// 把 instructions 里的 `<i>` 占位替换为候选序号(递归处理字符串/数组/对象)。
pub fn substitute_i(v: &Value, idx: Option<usize>) -> Value {
    match v {
        Value::String(s) => Value::String(match idx {
            Some(i) => s.replace("<i>", &i.to_string()),
            None => s.clone(),
        }),
        Value::Array(a) => Value::Array(a.iter().map(|x| substitute_i(x, idx)).collect()),
        Value::Object(o) => Value::Object(
            o.iter()
                .map(|(k, x)| (k.clone(), substitute_i(x, idx)))
                .collect(),
        ),
        other => other.clone(),
    }
}

/// 候选规模策略(spec §6.2):
/// N ≤ in_call → 单次;in_call < N ≤ judged → 分片(每片 ≤ in_call);
/// N > judged → 先截断到 judged(截断由调用方按粗排结果执行),再分片。
///
/// 调用方保证 `in_call ≥ 1`(配置校验限定:`ServerConfig::validate` 要求
/// `0 < max_candidates_in_call ≤ max_candidates_judged`);
/// `in_call = 0` 且 `effective > 0` 时本函数不会终止。
#[derive(Debug, Clone, PartialEq)]
pub struct ShardPlan {
    pub truncate_to: Option<usize>,
    pub shards: Vec<std::ops::Range<usize>>,
}

pub fn plan_shards(n: usize, in_call: usize, judged: usize) -> ShardPlan {
    debug_assert!(
        in_call >= 1,
        "in_call 必须 ≥ 1(ServerConfig::validate 限定)"
    );
    let truncate_to = if n > judged { Some(judged) } else { None };
    let effective = truncate_to.unwrap_or(n);
    let mut shards = Vec::new();
    let mut start = 0;
    while start < effective {
        let end = (start + in_call).min(effective);
        shards.push(start..end);
        start = end;
    }
    ShardPlan {
        truncate_to,
        shards,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::BudgetConstraint;
    use crate::config::QuestionType;
    use crate::model::{Capabilities, ModelStatus, Price};
    use serde_json::json;

    fn shipped_cfg() -> QuestionsConfig {
        QuestionsConfig::from_yaml(include_str!("../../../configs/questions.yaml"))
            .expect("仓库 questions.yaml 必须可解析")
    }

    fn fixed_profile() -> Profile {
        Profile {
            est_tokens: 8200,
            text_chars: 1200,
            has_images: false,
            tools_count: 3,
            json_mode: true,
            stream: false,
            max_tokens: Some(4096),
            locale_hint: Some("zh".into()),
            task_hints: vec![],
            request_excerpt: "请帮我修一个 bug".to_string(),
        }
    }

    fn view(i: usize, id: &str, band: PriceBand, in_price: f64) -> CandidateView {
        CandidateView {
            i,
            id: id.to_string(),
            desc: format!("{id} 的描述"),
            tags: vec!["chat".into()],
            ctx: 128_000,
            price_band: band,
            in_price,
            out_price: in_price * 2.0,
        }
    }

    fn model_entry(id: &str, description: String, in_price: f64) -> ModelEntry {
        ModelEntry {
            id: id.into(),
            name: id.into(),
            vendor: "vendor-x".into(),
            description,
            tags: vec!["chat".into(), "code".into()],
            capabilities: Capabilities::default(),
            context_window: 200_000,
            max_output: Some(8192),
            price: Price {
                in_per_mtok: in_price,
                out_per_mtok: in_price * 3.0,
            },
            groups: vec![],
            quality_notes: String::new(),
            status: ModelStatus::default(),
        }
    }

    #[test]
    fn price_band_boundaries_inclusive() {
        let bands = shipped_cfg().price_bands;
        // 仓库配置即 spec 默认值(边界含端值)
        assert_eq!(bands.cheap_max, 1.0);
        assert_eq!(bands.mid_max, 5.0);
        assert_eq!(price_band(1.0, &bands), PriceBand::Cheap);
        assert_eq!(price_band(1.0001, &bands), PriceBand::Mid);
        assert_eq!(price_band(5.0, &bands), PriceBand::Mid);
        assert_eq!(price_band(5.0001, &bands), PriceBand::High);
        assert_eq!(price_band(0.0, &bands), PriceBand::Cheap);
    }

    #[test]
    fn candidate_view_truncates_desc_and_copies_fields() {
        let cfg = shipped_cfg();
        let m = model_entry("m1", "描".repeat(250), 3.5);
        let v = candidate_view(2, &m, &cfg.price_bands);
        assert_eq!(v.i, 2);
        assert_eq!(v.id, "m1");
        assert_eq!(v.desc.chars().count(), 200);
        assert_eq!(v.desc, "描".repeat(200));
        assert_eq!(v.tags, vec!["chat".to_string(), "code".to_string()]);
        assert_eq!(v.ctx, 200_000);
        assert_eq!(v.price_band, PriceBand::Mid); // 3.5 ∈ (1.0, 5.0]
        assert_eq!(v.in_price, 3.5);
        assert_eq!(v.out_price, 10.5);
    }

    #[test]
    fn build_state_no_budget_no_hints() {
        let profile = fixed_profile();
        let choices = vec![
            view(0, "m-cheap", PriceBand::Cheap, 0.5),
            view(1, "m-high", PriceBand::High, 12.0),
        ];
        let state = build_state(&profile, &Constraints::default(), &choices);

        // profile 摘要逐字段
        assert_eq!(state["profile"]["est_tokens"], json!(8200));
        assert_eq!(state["profile"]["has_images"], json!(false));
        assert_eq!(state["profile"]["tools_count"], json!(3));
        assert_eq!(state["profile"]["json_mode"], json!(true));
        assert_eq!(state["profile"]["max_tokens"], json!(4096));
        assert_eq!(state["profile"]["locale_hint"], json!("zh"));
        assert_eq!(state["request_excerpt"], json!("请帮我修一个 bug"));
        // hints 为空 → 无键
        assert!(state.get("task_hints").is_none());
        // 无预算 → 空对象
        assert_eq!(state["constraints"], json!({}));
        assert!(state["constraints"].as_object().unwrap().is_empty());

        // candidates 元素键恰好 8 个
        let cands = state["candidates"].as_array().expect("candidates 数组");
        assert_eq!(cands.len(), 2);
        let obj0 = cands[0].as_object().unwrap();
        assert_eq!(obj0.len(), 8);
        let mut keys: Vec<&str> = obj0.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            [
                "ctx",
                "desc",
                "i",
                "id",
                "in_price",
                "out_price",
                "price_band",
                "tags"
            ]
        );
        // 顺序/序号 = choices 顺序
        assert_eq!(cands[0]["i"], json!(0));
        assert_eq!(cands[0]["id"], json!("m-cheap"));
        assert_eq!(cands[0]["desc"], json!("m-cheap 的描述"));
        assert_eq!(cands[0]["price_band"], json!("cheap"));
        assert_eq!(cands[0]["tags"], json!(["chat"]));
        assert_eq!(cands[0]["ctx"], json!(128_000));
        assert_eq!(cands[1]["i"], json!(1));
        assert_eq!(cands[1]["id"], json!("m-high"));
        assert_eq!(cands[1]["price_band"], json!("high"));
        assert_eq!(cands[1]["in_price"], json!(12.0));
        assert_eq!(cands[1]["out_price"], json!(24.0));
    }

    #[test]
    fn build_state_budget_and_hints_variants() {
        let mut profile = fixed_profile();
        profile.task_hints = vec!["包含代码块".into(), "提到报错堆栈".into()];
        let choices = vec![view(0, "m1", PriceBand::Mid, 3.0)];

        // budget 含价格上限 → 两个键都出现
        let cons = Constraints {
            budget: Some(BudgetConstraint {
                max_input_price_per_mtok: Some(3.0),
                cost_sensitive: true,
            }),
            ..Default::default()
        };
        let state = build_state(&profile, &cons, &choices);
        assert_eq!(state["constraints"]["budget_max_in_price"], json!(3.0));
        assert_eq!(state["constraints"]["cost_sensitive"], json!(true));
        assert_eq!(state["constraints"].as_object().unwrap().len(), 2);
        assert_eq!(state["task_hints"], json!(["包含代码块", "提到报错堆栈"]));

        // budget 无价格上限 → 仅 cost_sensitive 出现
        let cons = Constraints {
            budget: Some(BudgetConstraint::default()),
            ..Default::default()
        };
        let state = build_state(&profile, &cons, &choices);
        assert!(state["constraints"].get("budget_max_in_price").is_none());
        assert_eq!(state["constraints"]["cost_sensitive"], json!(false));
    }

    #[test]
    fn build_judge_request_ids_substitution_and_passthrough() {
        let cfg = shipped_cfg();
        let profile = fixed_profile();
        let cons = Constraints::default();
        let choices = vec![
            view(0, "m-a", PriceBand::Cheap, 0.5),
            view(1, "m-b", PriceBand::Mid, 3.0),
        ];
        let req = build_judge_request("jev-test", &profile, &cons, &cfg, &choices);

        // 问题数 = shared.len() + 2×候选数
        assert_eq!(req.questions.len(), cfg.shared.len() + 2 * choices.len());
        // ID 集合(shared 固定 ID + c{i}.{dim})
        let mut expected: Vec<String> = cfg.shared.keys().cloned().collect();
        expected.push("c0.cost_fit".into());
        expected.push("c0.quality_fit".into());
        expected.push("c1.cost_fit".into());
        expected.push("c1.quality_fit".into());
        expected.sort();
        let actual: Vec<String> = req.questions.keys().cloned().collect(); // BTreeMap 即有序
        assert_eq!(actual, expected);

        // <i> 替换为候选局部序号;instructions 是字符串
        let q0 = &req.questions["c0.quality_fit"];
        let instr0 = q0
            .instructions
            .as_str()
            .expect("quality_fit.instructions 应为字符串");
        assert!(instr0.contains("candidates[0]"), "got: {instr0}");
        assert!(!instr0.contains("<i>"), "got: {instr0}");
        let q1 = &req.questions["c1.cost_fit"];
        let instr1 = q1
            .instructions
            .as_str()
            .expect("cost_fit.instructions 应为字符串");
        assert!(instr1.contains("candidates[1]"), "got: {instr1}");
        assert!(!instr1.contains("<i>"), "got: {instr1}");

        // criteria 原样透传(score → 数组)
        assert_eq!(q0.criteria, cfg.per_candidate.quality_fit.criteria);
        assert!(q0.criteria.as_ref().unwrap().is_array());
        assert_eq!(q0.qtype, QuestionType::Score);
        assert_eq!(q1.criteria, cfg.per_candidate.cost_fit.criteria);
        assert_eq!(q1.qtype, QuestionType::Score);

        // 共享问题无候选占位,字段原样
        let shared = &req.questions["difficulty"];
        assert_eq!(shared.instructions, cfg.shared["difficulty"].instructions);
        assert_eq!(shared.criteria, cfg.shared["difficulty"].criteria);
        assert_eq!(shared.qtype, QuestionType::Score);

        // model / state 基本接线
        assert_eq!(req.model, "jev-test");
        assert_eq!(req.state["candidates"][1]["id"], json!("m-b"));
        assert_eq!(req.state["constraints"], json!({}));
    }

    #[test]
    fn substitute_i_recurses_and_none_is_noop() {
        let v = json!({
            "a": "x <i> y",
            "b": ["<i>", {"c": "z<i>", "d": 7}],
            "e": 3,
            "f": null,
        });
        assert_eq!(
            substitute_i(&v, Some(2)),
            json!({
                "a": "x 2 y",
                "b": ["2", {"c": "z2", "d": 7}],
                "e": 3,
                "f": null,
            })
        );
        // 同一字符串多次出现全部替换
        assert_eq!(substitute_i(&json!("<i>+<i>"), Some(0)), json!("0+0"));
        // idx = None → 原样返回
        assert_eq!(substitute_i(&v, None), v);
    }

    #[test]
    fn plan_shards_boundaries_and_truncation() {
        // N = 0 → 空计划,不截断
        assert_eq!(
            plan_shards(0, 30, 60),
            ShardPlan {
                truncate_to: None,
                shards: vec![],
            }
        );
        // N ≤ in_call → 单次调用
        let p = plan_shards(1, 30, 60);
        assert_eq!(p.truncate_to, None);
        assert_eq!(p.shards.len(), 1);
        assert_eq!(p.shards[0], 0..1);
        let p = plan_shards(30, 30, 60);
        assert_eq!(p.truncate_to, None);
        assert_eq!(p.shards.len(), 1);
        assert_eq!(p.shards[0], 0..30);
        // in_call < N ≤ judged → 分片,无截断
        let p = plan_shards(31, 30, 60);
        assert_eq!(p.truncate_to, None);
        assert_eq!(p.shards, vec![0..30, 30..31]);
        let p = plan_shards(60, 30, 60);
        assert_eq!(p.truncate_to, None);
        assert_eq!(p.shards, vec![0..30, 30..60]);
        // N > judged → 先截断到 judged,再分片
        let p61 = plan_shards(61, 30, 60);
        assert_eq!(p61.truncate_to, Some(60));
        assert_eq!(p61.shards, vec![0..30, 30..60]);
        let p90 = plan_shards(90, 30, 60);
        assert_eq!(p90.truncate_to, Some(60));
        assert_eq!(p90.shards, vec![0..30, 30..60]);
        // 边界参数:in_call = 1, judged = 2 → 截断到 2,每片 1 个
        let p = plan_shards(5, 1, 2);
        assert_eq!(p.truncate_to, Some(2));
        assert_eq!(p.shards, vec![0..1, 1..2]);
    }

    #[test]
    fn snapshot_full_judge_request() {
        let cfg = shipped_cfg();
        let mut profile = fixed_profile();
        profile.task_hints = vec!["包含代码块".into()];
        let cons = Constraints {
            budget: Some(BudgetConstraint {
                max_input_price_per_mtok: Some(3.0),
                cost_sensitive: true,
            }),
            ..Default::default()
        };
        let choices = vec![
            view(0, "deepseek-v3-0324", PriceBand::Cheap, 0.27),
            view(1, "claude-opus-4-1", PriceBand::High, 15.0),
        ];
        let req = build_judge_request("jev-1.13.0", &profile, &cons, &cfg, &choices);
        insta::assert_snapshot!(serde_json::to_string_pretty(&req).unwrap());
    }
}
