//! CoarseRank(spec §6.2): >60 截断与降级排序;确定性。

use crate::model::ModelEntry;
use crate::profile::Profile;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CoarseScore {
    pub tag_overlap: f32,
    pub price_score: f32,
}

impl CoarseScore {
    pub fn total(&self) -> f32 {
        0.5 * self.tag_overlap + 0.5 * self.price_score
    }
}

/// 画像推导的 needs 标签集(spec §6.2)。
pub fn needs(p: &Profile) -> Vec<&'static str> {
    let mut n = vec![];
    if p.has_images {
        n.push("vision");
    }
    if p.tools_count > 0 {
        n.push("tools");
    }
    if p.json_mode {
        n.push("json_mode");
    }
    if p.est_tokens > 32_000 {
        n.push("long_context");
    }
    n
}

pub fn tag_overlap(needs: &[&str], tags: &[String]) -> f32 {
    if needs.is_empty() {
        return 1.0;
    }
    let hit = needs
        .iter()
        .filter(|n| tags.iter().any(|t| t == *n))
        .count();
    hit as f32 / needs.len() as f32
}

/// 输入单价升序名次 → 分位:最便宜 1.0,最贵 0.0;N=1 → 1.0。等价格保持稳定序。
pub fn price_percentiles(models: &[ModelEntry]) -> Vec<f32> {
    let n = models.len();
    if n <= 1 {
        return vec![1.0; n];
    }
    let mut idx: Vec<usize> = (0..n).collect();
    idx.sort_by(|&a, &b| {
        models[a]
            .price
            .in_per_mtok
            .partial_cmp(&models[b].price.in_per_mtok)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let mut out = vec![0.0f32; n];
    for (rank, &i) in idx.iter().enumerate() {
        out[i] = 1.0 - rank as f32 / (n - 1) as f32;
    }
    out
}

/// 降序排好序的 (模型, 粗排分)。tie-break: 输入价格升序 → id 字典序
/// (采用 spec §6.4 的融合链;§6.2 行文仅写“id 字典序”,此处一致地细化到价格优先)。
pub fn coarse_rank<'a>(
    profile: &Profile,
    models: &'a [ModelEntry],
) -> Vec<(&'a ModelEntry, CoarseScore)> {
    let needs = needs(profile);
    let pct = price_percentiles(models);
    let mut scored: Vec<(&ModelEntry, CoarseScore)> = models
        .iter()
        .enumerate()
        .map(|(i, m)| {
            (
                m,
                CoarseScore {
                    tag_overlap: tag_overlap(&needs, &m.tags),
                    price_score: pct[i],
                },
            )
        })
        .collect();
    scored.sort_by(|a, b| {
        b.1.total()
            .partial_cmp(&a.1.total())
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(
                a.0.price
                    .in_per_mtok
                    .partial_cmp(&b.0.price.in_per_mtok)
                    .unwrap_or(std::cmp::Ordering::Equal),
            )
            .then(a.0.id.cmp(&b.0.id))
    });
    scored
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Capabilities, ModelStatus, Price};

    /// 测试要点 4 的复用构造:同价/同 tag 可自由组合。
    fn make_model(id: &str, price: f64, tags: &[&str]) -> ModelEntry {
        ModelEntry {
            id: id.into(),
            name: id.into(),
            vendor: "test".into(),
            description: String::new(),
            tags: tags.iter().map(|t| t.to_string()).collect(),
            capabilities: Capabilities::default(),
            context_window: 128_000,
            max_output: None,
            price: Price {
                in_per_mtok: price,
                out_per_mtok: 0.0,
            },
            groups: vec![],
            quality_notes: String::new(),
            status: ModelStatus::Enabled,
        }
    }

    fn profile_with(est_tokens: u32, images: bool, tools: u32, json: bool) -> Profile {
        let mut p = Profile::from_text("hi").unwrap();
        p.est_tokens = est_tokens;
        p.has_images = images;
        p.tools_count = tools;
        p.json_mode = json;
        p
    }

    // 测试要点 1:needs。
    #[test]
    fn needs_empty_when_profile_plain() {
        assert!(needs(&profile_with(5, false, 0, false)).is_empty());
    }

    #[test]
    fn needs_reflect_flags_and_32k_threshold() {
        let all = profile_with(32_001, true, 2, true);
        assert_eq!(
            needs(&all),
            vec!["vision", "tools", "json_mode", "long_context"]
        );

        // > 32k 才算 long_context:32001 含,32000 不含。
        assert!(needs(&profile_with(32_001, false, 0, false)).contains(&"long_context"));
        assert!(!needs(&profile_with(32_000, false, 0, false)).contains(&"long_context"));
    }

    // 测试要点 2:tag_overlap。
    #[test]
    fn tag_overlap_counts_hits() {
        let tags: Vec<String> = vec!["vision".into(), "tools".into()];
        assert_eq!(tag_overlap(&[], &tags), 1.0); // needs 空 → 1.0
        assert_eq!(tag_overlap(&["vision", "json_mode"], &tags), 0.5); // 命中一半
        assert_eq!(tag_overlap(&["audio", "json_mode"], &tags), 0.0); // 全不中
        assert_eq!(tag_overlap(&["vision"], &tags), 1.0); // 全中

        // 模型多余 tag 不影响命中率;needs 与空 tag 集 → 0.0。
        let extra: Vec<String> = vec!["vision".into(), "chat".into(), "reasoning".into()];
        assert_eq!(tag_overlap(&["vision"], &extra), 1.0);
        assert_eq!(tag_overlap(&["vision"], &[]), 0.0);
        assert_eq!(tag_overlap(&[], &[]), 1.0);
    }

    // 测试要点 3:price_percentiles。
    #[test]
    fn price_percentiles_spread_and_single() {
        let ms = [
            make_model("a", 1.0, &[]),
            make_model("b", 2.0, &[]),
            make_model("c", 3.0, &[]),
        ];
        assert_eq!(price_percentiles(&ms), vec![1.0, 0.5, 0.0]);

        let one = [make_model("only", 5.0, &[])];
        assert_eq!(price_percentiles(&one), vec![1.0]);
        assert_eq!(price_percentiles(&[]), Vec::<f32>::new());
    }

    #[test]
    fn price_percentiles_stable_for_equal_prices() {
        // 等价格保持输入序:先出现者名次更高(稳定排序)。
        let ms = [
            make_model("x", 2.0, &[]),
            make_model("y", 1.0, &[]),
            make_model("z", 1.0, &[]),
        ];
        assert_eq!(price_percentiles(&ms), vec![0.0, 1.0, 0.5]);
    }

    // 测试要点 4:coarse_rank。
    #[test]
    fn coarse_rank_orders_by_total() {
        // 同 tag(overlap 全 1),价格 1 < 2 < 3 → total 1.0 > 0.75 > 0.5。
        // tie-break 链路由下方 equal_total_* 两个测试钉住。
        let ms = [
            make_model("mid", 2.0, &["vision"]),
            make_model("top", 1.0, &["vision"]),
            make_model("low", 3.0, &["vision"]),
        ];
        let p = profile_with(5, true, 0, false);
        let ranked = coarse_rank(&p, &ms);
        let ids: Vec<&str> = ranked.iter().map(|(m, _)| m.id.as_str()).collect();
        assert_eq!(ids, vec!["top", "mid", "low"]);
        assert_eq!(
            ranked[0].1,
            CoarseScore {
                tag_overlap: 1.0,
                price_score: 1.0
            }
        );
        assert_eq!(ranked[1].1.total(), 0.75);
        assert_eq!(ranked[2].1.total(), 0.5);
    }

    /// total 完全相等、仅价格不同时,价格升序决定先后。
    /// id 特意与价格顺序相反(zcheap 字典序 > apricey),使断言只有在价格 key 生效时才成立。
    #[test]
    fn coarse_rank_equal_total_prefers_cheaper() {
        // zcheap 无 tag:0.5×0 + 0.5×1.0 = 0.5;apricey 有 tag:0.5×1.0 + 0.5×0 = 0.5。
        // 去掉 .then(price) 后 id 比较会把 "apricey" 排到前面 → 本测试必失败。
        let ms = [
            make_model("apricey", 3.0, &["vision"]),
            make_model("zcheap", 1.0, &[]),
        ];
        let p = profile_with(5, true, 0, false);
        let ranked = coarse_rank(&p, &ms);
        let ids: Vec<&str> = ranked.iter().map(|(m, _)| m.id.as_str()).collect();
        assert_eq!(ids, vec!["zcheap", "apricey"]);
        assert_eq!(ranked[0].1.total(), ranked[1].1.total());
    }

    /// 测试要点 4 后半的 id tie-break。注意:spec §6.2 分位公式下,等价格模型名次相邻、
    /// 分位必不相同,因此 total 与「价格」同为 tie 需要 overlap 恰好补偿分位差;
    /// 这里构造该唯一可触发链路的场景,断言最终按 id 字典序。
    #[test]
    fn coarse_rank_equal_total_and_price_breaks_by_id() {
        // 价格 [1,2,2] → 分位 [1.0,0.5,0.0];needs = vision+tools。
        // "b": overlap 0.5、分位 0.5 → total 0.5;"a": overlap 1.0、分位 0.0 → total 0.5。
        // "b" 与 "a" 价格同为 2.0 → 仅剩 id 判序 → "a" 在前。
        let ms = [
            make_model("cheap", 1.0, &["vision"]),
            make_model("b", 2.0, &["vision"]),
            make_model("a", 2.0, &["vision", "tools"]),
        ];
        let p = profile_with(5, true, 1, false);
        let ranked = coarse_rank(&p, &ms);
        let ids: Vec<&str> = ranked.iter().map(|(m, _)| m.id.as_str()).collect();
        assert_eq!(ids, vec!["cheap", "a", "b"]);
        assert_eq!(ranked[1].1.total(), ranked[2].1.total());
        assert_eq!(ranked[1].0.price, ranked[2].0.price);
    }

    /// 等价格 + 等重叠的「字面」场景:分位公式给出相邻名次(差 1/(N−1)),
    /// 故 total 不同,id tie-break 不触发——先出现者分位更高,与 id 无关。
    /// 此测试把该确定性行为钉死,防止未来改动其顺序。
    #[test]
    fn coarse_rank_equal_price_equal_overlap_keeps_stable_order() {
        let ms = [
            make_model("b", 1.0, &["vision"]),
            make_model("a", 1.0, &["vision"]),
        ];
        let p = profile_with(5, true, 0, false);
        let ranked = coarse_rank(&p, &ms);
        let ids: Vec<&str> = ranked.iter().map(|(m, _)| m.id.as_str()).collect();
        assert_eq!(ids, vec!["b", "a"]);
        assert_ne!(ranked[0].1.total(), ranked[1].1.total());
    }

    #[test]
    fn coarse_rank_is_deterministic_and_handles_empty() {
        let ms = [
            make_model("m2", 2.0, &["vision"]),
            make_model("m1", 2.0, &["vision"]),
            make_model("m3", 1.0, &[]),
        ];
        let p = profile_with(5, true, 0, false);
        let first: Vec<String> = coarse_rank(&p, &ms)
            .iter()
            .map(|(m, _)| m.id.clone())
            .collect();
        let second: Vec<String> = coarse_rank(&p, &ms)
            .iter()
            .map(|(m, _)| m.id.clone())
            .collect();
        assert_eq!(first, second);
        // m2 total 0.75 居首;m3 与 m1 total 同为 0.5,价格 1.0 < 2.0 → m3 在前。
        assert_eq!(first, vec!["m2", "m3", "m1"]);
        assert!(coarse_rank(&p, &[]).is_empty());
    }
}
