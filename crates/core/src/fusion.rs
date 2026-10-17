//! 融合(spec §6.4): 归一化 = score/(档数-1);权重 = base + adjustments(全条件 AND)→ 归一化 → clamp;
//! final = w_q·q + w_c·c − penalty;tie-break: 价格升序 → id 字典序;confidence = min(维度 confidence)。

use crate::api::Constraints;
use crate::config::{Conditions, QuestionsConfig};
use crate::judgment::{DimAnswer, SharedAnswers};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Weights {
    pub quality_fit: f32,
    pub cost_fit: f32,
}

/// adjustments 命中判定:同一调整内**所有**出现的条件都需满足;未出现的条件不约束(spec §6.4)。
fn cond_hit(c: &Conditions, s: &SharedAnswers, cons: &Constraints) -> bool {
    let d = c
        .difficulty_at_least
        .is_none_or(|t| s.difficulty.is_some_and(|v| v >= t));
    let f = c
        .frontier_needed_above
        .is_none_or(|t| s.frontier_needed.is_some_and(|v| v > t));
    let b = c.budget_present.is_none_or(|t| t == cons.budget.is_some());
    let cs = c
        .cost_sensitive
        .is_none_or(|t| t == cons.budget.as_ref().is_some_and(|b| b.cost_sensitive));
    d && f && b && cs
}

pub fn compute_weights(
    cfg: &QuestionsConfig,
    shared: &SharedAnswers,
    constraints: &Constraints,
) -> Weights {
    let mut wq = cfg.weights.base.quality_fit;
    let mut wc = cfg.weights.base.cost_fit;
    for adj in &cfg.weights.adjustments {
        if !cond_hit(&adj.when, shared, constraints) {
            continue;
        }
        for (k, dv) in &adj.set {
            let Ok(delta) = dv.delta() else {
                continue;
            }; // 校验已拒绝坏值;此处防御
            match k.as_str() {
                "quality_fit" => wq += delta,
                "cost_fit" => wc += delta,
                _ => {}
            }
        }
    }
    let sum = wq + wc;
    if sum > 0.0 {
        wq /= sum;
        wc /= sum;
    }
    Weights {
        quality_fit: wq.clamp(cfg.weights.clamp.min, cfg.weights.clamp.max),
        cost_fit: wc.clamp(cfg.weights.clamp.min, cfg.weights.clamp.max),
    }
}

pub fn norm_score(raw: f32, levels: usize) -> f32 {
    let denom = levels.saturating_sub(1).max(1) as f32;
    (raw / denom).clamp(0.0, 1.0)
}

pub fn final_score(w: Weights, q_norm: f32, c_norm: f32, penalty: f32) -> f32 {
    w.quality_fit * q_norm + w.cost_fit * c_norm - penalty
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FusedScore {
    pub quality_norm: f32,
    pub cost_norm: f32,
    pub score: f32,
    pub confidence: f32,
}

/// 缺答案(spec §8.2):该维按归一化 0.5 参与、confidence=0.0。
pub fn fuse_candidate(
    w: &Weights,
    q: Option<&DimAnswer>,
    c: Option<&DimAnswer>,
    q_levels: usize,
    c_levels: usize,
    penalty: f32,
) -> FusedScore {
    let q_norm = q.map(|d| norm_score(d.raw_score, q_levels)).unwrap_or(0.5);
    let c_norm = c.map(|d| norm_score(d.raw_score, c_levels)).unwrap_or(0.5);
    let confidence = q
        .map(|d| d.confidence)
        .unwrap_or(0.0)
        .min(c.map(|d| d.confidence).unwrap_or(0.0));
    FusedScore {
        quality_norm: q_norm,
        cost_norm: c_norm,
        score: final_score(*w, q_norm, c_norm, penalty),
        confidence,
    }
}

/// 排序比较器:score 降序 → 输入价升序 → id 字典序(完全确定性)。
pub fn cmp_rank(
    score_a: f32,
    price_a: f64,
    id_a: &str,
    score_b: f32,
    price_b: f64,
    id_b: &str,
) -> std::cmp::Ordering {
    score_b
        .partial_cmp(&score_a)
        .unwrap_or(std::cmp::Ordering::Equal)
        .then(
            price_a
                .partial_cmp(&price_b)
                .unwrap_or(std::cmp::Ordering::Equal),
        )
        .then(id_a.cmp(id_b))
}

/// 输出前统一舍入到 3 位小数(与 spec §8.1 示例一致)。
pub fn round3(x: f32) -> f32 {
    (x * 1000.0).round() / 1000.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::BudgetConstraint;
    use std::cmp::Ordering;
    use std::collections::BTreeMap;

    /// 拼一份完整小 YAML:shared 三问 + per_candidate 两问(4/3 档)+ price_bands +
    /// confidence;weights 段与仓库 configs/questions.yaml 相同(base 0.75/0.25、四条
    /// adjustments、clamp 0.05/0.95),`extra_adjustments` 追加进 adjustments 列表
    /// (每条自带缩进与结尾换行),用于构造极端权重场景。
    fn test_cfg(extra_adjustments: &str) -> QuestionsConfig {
        let yaml = format!(
            r#"shared:
  task_type:
    type: choice
    instructions: "任务类型"
    criteria: {{ chat: 一般对话 }}
  difficulty:
    type: score
    instructions: "难度"
    criteria: ["简单", "中等", "困难"]
  frontier_needed:
    type: noul
    instructions: "是否需要前沿"
    criteria: {{ "true": 是, "false": 否 }}
per_candidate:
  quality_fit:
    type: score
    instructions: "质量匹配"
    criteria: ["差", "中", "良", "优"]
  cost_fit:
    type: score
    instructions: "成本匹配"
    criteria: ["贵", "可接受", "实惠"]
price_bands: {{ cheap_max: 1.0, mid_max: 5.0 }}
weights:
  base: {{ quality_fit: 0.75, cost_fit: 0.25 }}
  adjustments:
    - when: {{ difficulty_at_least: 1.5 }}
      set: {{ quality_fit: "+0.10", cost_fit: "-0.10" }}
    - when: {{ frontier_needed_above: 0.6 }}
      set: {{ quality_fit: "+0.05", cost_fit: "-0.05" }}
    - when: {{ budget_present: true }}
      set: {{ cost_fit: "+0.10", quality_fit: "-0.10" }}
    - when: {{ cost_sensitive: true }}
      set: {{ cost_fit: "+0.15", quality_fit: "-0.15" }}
{extra_adjustments}  clamp: {{ min: 0.05, max: 0.95 }}
confidence: {{ flag_below: 0.5 }}"#
        );
        QuestionsConfig::from_yaml(&yaml).expect("test_cfg YAML 必须可解析")
    }

    fn shared_df(difficulty: f32, frontier: Option<f32>) -> SharedAnswers {
        SharedAnswers {
            task_type: Some("chat".into()),
            difficulty: Some(difficulty),
            frontier_needed: frontier,
        }
    }

    fn budget(cost_sensitive: bool) -> Constraints {
        Constraints {
            budget: Some(BudgetConstraint {
                max_input_price_per_mtok: Some(3.0),
                cost_sensitive,
            }),
            ..Default::default()
        }
    }

    fn dim(raw: f32, confidence: f32) -> DimAnswer {
        DimAnswer {
            raw_score: raw,
            confidence,
            legend: BTreeMap::new(),
            probabilities: BTreeMap::new(),
        }
    }

    #[test]
    fn spec_8_1_numeric_loop() {
        // 要点 1:base 0.75/0.25;仅 budget_present 命中(difficulty=1.2、无 frontier)
        // → weights == 0.65/0.35
        let cfg = test_cfg("");
        assert_eq!(cfg.quality_levels(), 4);
        assert_eq!(cfg.cost_levels(), 3);
        let w = compute_weights(&cfg, &shared_df(1.2, None), &budget(false));
        assert!(
            (w.quality_fit - 0.65).abs() < 1e-6,
            "wq = {}",
            w.quality_fit
        );
        assert!((w.cost_fit - 0.35).abs() < 1e-6, "wc = {}", w.cost_fit);

        // q raw=2.76(4 档)→ norm=0.92;c raw=1.0(3 档)→ norm=0.50;
        // final == 0.773(误差 < 1e-4);round3 后 == 0.773
        let f = fuse_candidate(&w, Some(&dim(2.76, 0.9)), Some(&dim(1.0, 0.6)), 4, 3, 0.0);
        assert!(
            (f.quality_norm - 0.92).abs() < 1e-6,
            "q_norm = {}",
            f.quality_norm
        );
        assert!(
            (f.cost_norm - 0.50).abs() < 1e-6,
            "c_norm = {}",
            f.cost_norm
        );
        assert!((f.score - 0.773).abs() < 1e-4, "score = {}", f.score);
        assert_eq!(round3(f.score), 0.773);
        // confidence = min(0.9, 0.6) = 0.6
        assert!((f.confidence - 0.6).abs() < 1e-6);
    }

    #[test]
    fn all_four_adjustments_hit_net_to_same_weights() {
        // 要点 2:四个调整全命中(difficulty=1.6、frontier=0.7、有预算、cost_sensitive=true)
        // → 净效果 -0.10q/+0.10c → 仍为 0.65/0.35
        let cfg = test_cfg("");
        let w = compute_weights(&cfg, &shared_df(1.6, Some(0.7)), &budget(true));
        assert!(
            (w.quality_fit - 0.65).abs() < 1e-6,
            "wq = {}",
            w.quality_fit
        );
        assert!((w.cost_fit - 0.35).abs() < 1e-6, "wc = {}", w.cost_fit);
    }

    #[test]
    fn condition_thresholds_inclusive_and_exclusive() {
        // 要点 3:difficulty 恰好 = 1.5 → 命中(>=)
        let cfg = test_cfg("");
        let w = compute_weights(&cfg, &shared_df(1.5, None), &Constraints::default());
        assert!(
            (w.quality_fit - 0.85).abs() < 1e-6,
            "wq = {}",
            w.quality_fit
        );
        assert!((w.cost_fit - 0.15).abs() < 1e-6, "wc = {}", w.cost_fit);

        // frontier 恰好 = 0.6 → 不命中(严格 >);difficulty 1.4 < 1.5 也不命中 → base
        let w = compute_weights(&cfg, &shared_df(1.4, Some(0.6)), &Constraints::default());
        assert!(
            (w.quality_fit - 0.75).abs() < 1e-6,
            "wq = {}",
            w.quality_fit
        );
        assert!((w.cost_fit - 0.25).abs() < 1e-6, "wc = {}", w.cost_fit);
    }

    #[test]
    fn clamp_bounds_normalized_weights() {
        // 要点 4:极端 adjustment(difficulty_at_least: 1.0,与仓库四条不重叠)
        // → wq=0.05、wc=1.15,sum=1.20 → wc≈0.958 → 被 clamp 到 0.95(wq≈0.042 → 0.05)
        let cfg = test_cfg(
            "    - when: { difficulty_at_least: 1.0 }\n      set: { quality_fit: \"-0.70\", cost_fit: \"+0.90\" }\n",
        );
        let w = compute_weights(&cfg, &shared_df(1.2, None), &Constraints::default());
        assert!(
            (w.quality_fit - 0.05).abs() < 1e-6,
            "wq = {}",
            w.quality_fit
        );
        assert!((w.cost_fit - 0.95).abs() < 1e-6, "wc = {}", w.cost_fit);

        // 只写 cost_fit "+0.90" 时 sum=1.90 → wc≈0.605,触发不了钳制
        let cfg = test_cfg(
            "    - when: { difficulty_at_least: 1.0 }\n      set: { cost_fit: \"+0.90\" }\n",
        );
        let w = compute_weights(&cfg, &shared_df(1.2, None), &Constraints::default());
        assert!((w.cost_fit - 0.605).abs() < 1e-3, "wc = {}", w.cost_fit);
        assert!(w.cost_fit < 0.95);
    }

    #[test]
    fn norm_score_scales_by_level_count() {
        // 要点 5:raw 3.0/4 档 → 1.0;raw -1 → 0.0;levels=1 → raw/1
        assert!((norm_score(3.0, 4) - 1.0).abs() < 1e-6);
        assert_eq!(norm_score(-1.0, 4), 0.0);
        assert!((norm_score(0.7, 1) - 0.7).abs() < 1e-6);
        // levels=0 → 分母 saturating_sub 后为 0 → max(1) → raw/1
        assert!((norm_score(0.3, 0) - 0.3).abs() < 1e-6);
        // 超界 raw 截到 1.0
        assert_eq!(norm_score(5.0, 3), 1.0);
    }

    #[test]
    fn missing_answers_use_neutral_norm_and_zero_confidence() {
        // 要点 6:q=None → q_norm 0.5 且 confidence 0.0
        let cfg = test_cfg("");
        let w = compute_weights(&cfg, &shared_df(1.2, None), &budget(false));
        let f = fuse_candidate(&w, None, Some(&dim(1.0, 0.6)), 4, 3, 0.0);
        assert!((f.quality_norm - 0.5).abs() < 1e-6);
        assert_eq!(f.confidence, 0.0);
        // score = wq*0.5 + wc*0.5 = 0.65*0.5 + 0.35*0.5 = 0.5
        assert!((f.score - 0.5).abs() < 1e-6, "score = {}", f.score);

        // 两者都 None → score = w_q*0.5 + w_c*0.5 − penalty
        let f = fuse_candidate(&w, None, None, 4, 3, 0.1);
        assert!((f.quality_norm - 0.5).abs() < 1e-6);
        assert!((f.cost_norm - 0.5).abs() < 1e-6);
        assert_eq!(f.confidence, 0.0);
        assert!((f.score - 0.4).abs() < 1e-6, "score = {}", f.score);
    }

    #[test]
    fn rank_tie_breaks_are_deterministic() {
        // 要点 7:同分低价优先
        assert_eq!(cmp_rank(0.9, 1.0, "a", 0.9, 2.0, "b"), Ordering::Less);
        assert_eq!(cmp_rank(0.9, 2.0, "a", 0.9, 1.0, "b"), Ordering::Greater);
        // 同分同价 id 升序
        assert_eq!(cmp_rank(0.9, 1.0, "b", 0.9, 1.0, "a"), Ordering::Greater);
        assert_eq!(cmp_rank(0.9, 1.0, "a", 0.9, 1.0, "b"), Ordering::Less);
        // 分数不同:高分在前(价格不影响)
        assert_eq!(cmp_rank(0.95, 9.9, "a", 0.9, 0.1, "b"), Ordering::Less);
        // NaN 分数 → 视为相等,继续比价格
        assert_eq!(
            cmp_rank(f32::NAN, 1.0, "a", f32::NAN, 2.0, "b"),
            Ordering::Less
        );
    }
}
