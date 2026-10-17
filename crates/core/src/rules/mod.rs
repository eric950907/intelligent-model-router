//! 硬规则引擎(spec §4.3/§5.2): 有序插件链,配置驱动;产出候选集 + 命中/过滤记录。

pub mod builtin;

use crate::api::{Constraints, Options};
use crate::config::{RuleKind, RulesConfig};
use crate::model::ModelEntry;
use crate::profile::Profile;
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq)]
pub enum RuleOutcome {
    Pass,
    Filter {
        reason_key: String,
        params: BTreeMap<String, String>,
    },
    Penalty {
        value: f32,
        reason_key: String,
        params: BTreeMap<String, String>,
    },
}

pub struct RuleCtx<'a> {
    pub profile: &'a Profile,
    pub constraints: &'a Constraints,
    pub options: &'a Options,
}

pub trait Rule: Send + Sync {
    fn name(&self) -> &str;
    fn eval(&self, ctx: &RuleCtx, model: &ModelEntry) -> RuleOutcome;
}

pub fn build_rules(cfg: &RulesConfig) -> Vec<Box<dyn Rule>> {
    let mut rules: Vec<Box<dyn Rule>> = vec![];
    for spec in &cfg.rules {
        if !spec.enabled {
            continue;
        }
        rules.push(match spec.kind {
            RuleKind::Capability => Box::new(builtin::CapabilityRule),
            RuleKind::ContextLength => Box::new(builtin::ContextLengthRule {
                safety_factor: spec.safety_factor.unwrap_or(1.15),
            }),
            RuleKind::Budget => Box::new(builtin::BudgetRule),
            RuleKind::Vendor => Box::new(builtin::VendorRule),
            RuleKind::TagRequire => Box::new(builtin::TagRequireRule),
        });
    }
    rules
}

#[derive(Debug, Clone)]
pub struct PenaltyRecord {
    pub rule: String,
    pub value: f32,
    pub reason_key: String,
    pub params: BTreeMap<String, String>,
}

#[derive(Debug, Clone)]
pub struct Candidate {
    pub model: ModelEntry,
    pub penalties: f32,
    pub penalty_records: Vec<PenaltyRecord>,
    pub rule_hits: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct FilteredRecord {
    pub rule: String,
    pub reason_key: String,
    pub model_id: String,
}

#[derive(Debug, Default)]
pub struct FilterReport {
    pub candidates: Vec<Candidate>,
    pub filtered_by_rule: Vec<FilteredRecord>,
}

pub fn apply_rules(rules: &[Box<dyn Rule>], ctx: &RuleCtx, models: &[ModelEntry]) -> FilterReport {
    let mut report = FilterReport::default();
    for m in models {
        let mut filtered: Option<(String, String)> = None;
        let mut candidate = Candidate {
            model: m.clone(),
            penalties: 0.0,
            penalty_records: vec![],
            rule_hits: vec![],
        };
        for r in rules {
            match r.eval(ctx, m) {
                RuleOutcome::Pass => candidate.rule_hits.push(format!("{}✓", r.name())),
                RuleOutcome::Filter { reason_key, .. } => {
                    filtered = Some((r.name().to_string(), reason_key));
                    break;
                }
                RuleOutcome::Penalty {
                    value,
                    reason_key,
                    params,
                } => {
                    candidate.penalties += value;
                    candidate.penalty_records.push(PenaltyRecord {
                        rule: r.name().to_string(),
                        value,
                        reason_key,
                        params,
                    });
                    candidate.rule_hits.push(format!("{}:penalty", r.name()));
                }
            }
        }
        match filtered {
            Some((rule, reason_key)) => report.filtered_by_rule.push(FilteredRecord {
                rule,
                reason_key,
                model_id: m.id.clone(),
            }),
            None => report.candidates.push(candidate),
        }
    }
    report
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::BudgetConstraint;
    use crate::model::{Capabilities, ModelStatus, Price};

    fn base_model() -> ModelEntry {
        ModelEntry {
            id: "m1".into(),
            name: "M1".into(),
            vendor: "v1".into(),
            description: String::new(),
            tags: vec!["chat".into()],
            capabilities: Capabilities {
                tools: true,
                vision: true,
                json_mode: true,
                audio_in: false,
            },
            context_window: 100_000,
            max_output: Some(100_000),
            price: Price {
                in_per_mtok: 1.0,
                out_per_mtok: 2.0,
            },
            groups: vec!["default".into()],
            quality_notes: String::new(),
            status: ModelStatus::Enabled,
        }
    }

    /// `Profile::assemble_one` 私有;测试用 `from_text` 再改 pub 字段。
    fn profile() -> Profile {
        let mut p = Profile::from_text("hi").unwrap();
        p.est_tokens = 1000;
        p.max_tokens = Some(100);
        p
    }

    fn eval(rule: &dyn Rule, p: &Profile, c: &Constraints, m: &ModelEntry) -> RuleOutcome {
        let o = Options::default();
        rule.eval(
            &RuleCtx {
                profile: p,
                constraints: c,
                options: &o,
            },
            m,
        )
    }

    fn expect_filter(out: RuleOutcome) -> (String, BTreeMap<String, String>) {
        match out {
            RuleOutcome::Filter { reason_key, params } => (reason_key, params),
            other => panic!("expected Filter, got {other:?}"),
        }
    }

    #[test]
    fn capability_rule_truth_table() {
        let rule = builtin::CapabilityRule;
        let c = Constraints::default();

        // tools 需求 + 模型无 tools → 过滤
        let mut m = base_model();
        m.capabilities.tools = false;
        let mut p = profile();
        p.tools_count = 1;
        let (k, params) = expect_filter(eval(&rule, &p, &c, &m));
        assert_eq!(k, "capability_tools");
        assert_eq!(params["need"], "tools");

        // vision 需求 + 模型无 vision → 过滤
        let mut m = base_model();
        m.capabilities.vision = false;
        let mut p = profile();
        p.has_images = true;
        let (k, params) = expect_filter(eval(&rule, &p, &c, &m));
        assert_eq!(k, "capability_vision");
        assert_eq!(params["need"], "vision");

        // json_mode 需求 + 模型无 json_mode → 过滤
        let mut m = base_model();
        m.capabilities.json_mode = false;
        let mut p = profile();
        p.json_mode = true;
        let (k, params) = expect_filter(eval(&rule, &p, &c, &m));
        assert_eq!(k, "capability_json_mode");
        assert_eq!(params["need"], "json_mode");

        // 三种需求全满足 → 通过;stream=true 不产生额外约束
        let mut p = profile();
        p.tools_count = 3;
        p.has_images = true;
        p.json_mode = true;
        p.stream = true;
        assert_eq!(eval(&rule, &p, &c, &base_model()), RuleOutcome::Pass);

        // 无任何需求 → 通过(即便模型无能力)
        let mut m = base_model();
        m.capabilities = Capabilities::default();
        assert_eq!(eval(&rule, &profile(), &c, &m), RuleOutcome::Pass);
    }

    #[test]
    fn context_length_rule_boundary() {
        let rule = builtin::ContextLengthRule {
            safety_factor: 1.15,
        };
        let c = Constraints::default();
        let p = profile(); // est=1000 且 max_tokens=100 → needed = ceil(1000*1.15) + 100 = 1250

        // needed == window → 通过(过滤条件是严格 >,spec: needed ≤ window 即可)
        let mut m = base_model();
        m.context_window = 1250;
        assert_eq!(eval(&rule, &p, &c, &m), RuleOutcome::Pass);

        // 差 1 token → 过滤,params 带 needed/window
        m.context_window = 1249;
        let (k, params) = expect_filter(eval(&rule, &p, &c, &m));
        assert_eq!(k, "context_overflow");
        assert_eq!(params["needed"], "1250");
        assert_eq!(params["window"], "1249");

        // 窗口更大 → 通过
        m.context_window = 1251;
        assert_eq!(eval(&rule, &p, &c, &m), RuleOutcome::Pass);

        // max_tokens=None 视为 0: needed = 1150
        let mut p2 = profile();
        p2.max_tokens = None;
        m.context_window = 1150;
        assert_eq!(eval(&rule, &p2, &c, &m), RuleOutcome::Pass);
        m.context_window = 1149;
        let (k, _) = expect_filter(eval(&rule, &p2, &c, &m));
        assert_eq!(k, "context_overflow");
    }

    #[test]
    fn budget_rule_truth_table() {
        let rule = builtin::BudgetRule;
        let p = profile();
        let mut m = base_model();
        m.price.in_per_mtok = 3.0;

        // 无预算约束 → 通过
        assert_eq!(
            eval(&rule, &p, &Constraints::default(), &m),
            RuleOutcome::Pass
        );

        // 恰好等于上限 → 通过(仅严格 > 才过滤)
        let at_cap = Constraints {
            budget: Some(BudgetConstraint {
                max_input_price_per_mtok: Some(3.0),
                cost_sensitive: false,
            }),
            ..Constraints::default()
        };
        assert_eq!(eval(&rule, &p, &at_cap, &m), RuleOutcome::Pass);

        // 超限 → 过滤,params 带 price/max
        let over = Constraints {
            budget: Some(BudgetConstraint {
                max_input_price_per_mtok: Some(2.5),
                cost_sensitive: false,
            }),
            ..Constraints::default()
        };
        let (k, params) = expect_filter(eval(&rule, &p, &over, &m));
        assert_eq!(k, "budget_exceeded");
        assert_eq!(params["price"], "3");
        assert_eq!(params["max"], "2.5");

        // budget 存在但无价格上限 → 通过(不误用 cost_sensitive 过滤)
        let none_cap = Constraints {
            budget: Some(BudgetConstraint::default()),
            ..Constraints::default()
        };
        assert_eq!(eval(&rule, &p, &none_cap, &m), RuleOutcome::Pass);
    }

    #[test]
    fn vendor_rule_truth_table() {
        let rule = builtin::VendorRule;
        let p = profile();
        let m = base_model(); // vendor = "v1"

        // deny 命中 → 过滤(deny 优先于 allow)
        let c = Constraints {
            vendor_deny: vec!["v1".into()],
            vendor_allow: vec!["v1".into()],
            ..Constraints::default()
        };
        let (k, params) = expect_filter(eval(&rule, &p, &c, &m));
        assert_eq!(k, "vendor_denied");
        assert_eq!(params["vendor"], "v1");

        // allow 非空且不含该 vendor → 拦截
        let c = Constraints {
            vendor_allow: vec!["v2".into()],
            ..Constraints::default()
        };
        let (k, params) = expect_filter(eval(&rule, &p, &c, &m));
        assert_eq!(k, "vendor_not_allowed");
        assert_eq!(params["vendor"], "v1");

        // allow 含该 vendor → 通过
        let c = Constraints {
            vendor_allow: vec!["v1".into(), "v9".into()],
            ..Constraints::default()
        };
        assert_eq!(eval(&rule, &p, &c, &m), RuleOutcome::Pass);

        // allow 为空 → 放行
        assert_eq!(
            eval(&rule, &p, &Constraints::default(), &m),
            RuleOutcome::Pass
        );
    }

    #[test]
    fn tag_require_rule_truth_table() {
        let rule = builtin::TagRequireRule;
        let p = profile();
        let mut m = base_model(); // tags = ["chat"]

        // 缺一即滤(取第一个缺失的 tag)
        let c = Constraints {
            require_tags: vec!["chat".into(), "long".into(), "tools".into()],
            ..Constraints::default()
        };
        let (k, params) = expect_filter(eval(&rule, &p, &c, &m));
        assert_eq!(k, "tag_missing");
        assert_eq!(params["tag"], "long");

        // 全含 → 通过
        let c = Constraints {
            require_tags: vec!["chat".into()],
            ..Constraints::default()
        };
        assert_eq!(eval(&rule, &p, &c, &m), RuleOutcome::Pass);

        // 无要求 → 通过;模型 tags 为空也可
        m.tags.clear();
        assert_eq!(
            eval(&rule, &p, &Constraints::default(), &m),
            RuleOutcome::Pass
        );
    }

    struct FilterA;
    impl Rule for FilterA {
        fn name(&self) -> &str {
            "filter_a"
        }
        fn eval(&self, _ctx: &RuleCtx, m: &ModelEntry) -> RuleOutcome {
            if m.id == "m2" {
                RuleOutcome::Filter {
                    reason_key: "blocked_a".into(),
                    params: BTreeMap::new(),
                }
            } else {
                RuleOutcome::Pass
            }
        }
    }

    struct FilterB;
    impl Rule for FilterB {
        fn name(&self) -> &str {
            "filter_b"
        }
        fn eval(&self, _ctx: &RuleCtx, m: &ModelEntry) -> RuleOutcome {
            if m.id == "m2" {
                RuleOutcome::Filter {
                    reason_key: "blocked_b".into(),
                    params: BTreeMap::new(),
                }
            } else {
                RuleOutcome::Pass
            }
        }
    }

    struct PenaltyStub;
    impl Rule for PenaltyStub {
        fn name(&self) -> &str {
            "penalty_stub"
        }
        fn eval(&self, _ctx: &RuleCtx, _m: &ModelEntry) -> RuleOutcome {
            RuleOutcome::Penalty {
                value: 0.1,
                reason_key: "stub_penalty".into(),
                params: BTreeMap::from([("k".into(), "v".into())]),
            }
        }
    }

    #[test]
    fn engine_aggregates_penalties_and_filters_in_order() {
        let rules: Vec<Box<dyn Rule>> =
            vec![Box::new(FilterA), Box::new(FilterB), Box::new(PenaltyStub)];
        let mut m2 = base_model();
        m2.id = "m2".into();
        let mut m3 = base_model();
        m3.id = "m3".into();
        let models = vec![base_model(), m2, m3]; // 目录顺序 m1, m2, m3

        let p = profile();
        let c = Constraints::default();
        let o = Options::default();
        let report = apply_rules(
            &rules,
            &RuleCtx {
                profile: &p,
                constraints: &c,
                options: &o,
            },
            &models,
        );

        // 候选 = 未被滤者,顺序 = 目录顺序
        assert_eq!(report.candidates.len(), 2);
        assert_eq!(report.candidates[0].model.id, "m1");
        assert_eq!(report.candidates[1].model.id, "m3");

        // 被滤记录:m2 仅出现一次,且是首个命中它的规则(filter_a 先于 filter_b)
        assert_eq!(report.filtered_by_rule.len(), 1);
        let f = &report.filtered_by_rule[0];
        assert_eq!(f.rule, "filter_a");
        assert_eq!(f.reason_key, "blocked_a");
        assert_eq!(f.model_id, "m2");

        // 通过模型: 罚分聚合 + 命中记录格式
        for cand in &report.candidates {
            assert_eq!(cand.penalties, 0.1);
            assert_eq!(cand.penalty_records.len(), 1);
            let pr = &cand.penalty_records[0];
            assert_eq!(pr.rule, "penalty_stub");
            assert_eq!(pr.value, 0.1);
            assert_eq!(pr.reason_key, "stub_penalty");
            assert_eq!(pr.params["k"], "v");
            assert_eq!(
                cand.rule_hits,
                vec!["filter_a✓", "filter_b✓", "penalty_stub:penalty"]
            );
        }
    }

    #[test]
    fn build_rules_from_shipped_config() {
        let cfg = RulesConfig::from_yaml(include_str!("../../../../configs/rules.yaml")).unwrap();
        let rules = build_rules(&cfg);
        let names: Vec<&str> = rules.iter().map(|r| r.name()).collect();
        assert_eq!(names.len(), 5);
        assert_eq!(
            names,
            vec![
                "capability",
                "context_length",
                "budget",
                "vendor",
                "tag_require"
            ]
        );

        // 配置中 safety_factor: 1.15 生效(est=1000,max=100 ⇒ needed=1250)
        let p = profile();
        let c = Constraints::default();
        let mut m = base_model();
        m.context_window = 1249;
        assert!(matches!(
            rules[1].eval(
                &RuleCtx {
                    profile: &p,
                    constraints: &c,
                    options: &Options::default()
                },
                &m
            ),
            RuleOutcome::Filter { .. }
        ));
        m.context_window = 1250;
        assert_eq!(
            rules[1].eval(
                &RuleCtx {
                    profile: &p,
                    constraints: &c,
                    options: &Options::default()
                },
                &m
            ),
            RuleOutcome::Pass
        );
    }

    #[test]
    fn build_rules_skips_disabled() {
        let mut cfg =
            RulesConfig::from_yaml(include_str!("../../../../configs/rules.yaml")).unwrap();
        cfg.rules[0].enabled = false; // capability
        let rules = build_rules(&cfg);
        assert_eq!(rules.len(), 4);
        assert_eq!(rules[0].name(), "context_length");

        // 全禁用 → 空链
        for r in &mut cfg.rules {
            r.enabled = false;
        }
        assert!(build_rules(&cfg).is_empty());
    }

    #[test]
    fn build_rules_default_safety_factor_is_1_15() {
        let cfg = RulesConfig::from_yaml("rules:\n  - kind: context_length\n").unwrap();
        let rules = build_rules(&cfg);
        assert_eq!(rules.len(), 1);

        // 默认 1.15 → needed = 1150 + 100 = 1250(若误用 1.0 则 1249 会通过)
        let p = profile();
        let c = Constraints::default();
        let mut m = base_model();
        m.context_window = 1249;
        let (k, _) = expect_filter(eval(rules[0].as_ref(), &p, &c, &m));
        assert_eq!(k, "context_overflow");
        m.context_window = 1250;
        assert_eq!(eval(rules[0].as_ref(), &p, &c, &m), RuleOutcome::Pass);
    }
}
