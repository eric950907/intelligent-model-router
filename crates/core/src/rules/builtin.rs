//! 内置规则。全部确定性、无网络。

use super::{Rule, RuleCtx, RuleOutcome};
use crate::model::ModelEntry;

fn filter(
    reason_key: &str,
    params: impl IntoIterator<Item = (&'static str, String)>,
) -> RuleOutcome {
    RuleOutcome::Filter {
        reason_key: reason_key.into(),
        params: params
            .into_iter()
            .map(|(k, v)| (k.to_string(), v))
            .collect(),
    }
}

pub struct CapabilityRule;
impl Rule for CapabilityRule {
    fn name(&self) -> &str {
        "capability"
    }
    fn eval(&self, ctx: &RuleCtx, m: &ModelEntry) -> RuleOutcome {
        let p = ctx.profile;
        if p.tools_count > 0 && !m.capabilities.tools {
            return filter("capability_tools", [("need", "tools".into())]);
        }
        if p.has_images && !m.capabilities.vision {
            return filter("capability_vision", [("need", "vision".into())]);
        }
        if p.json_mode && !m.capabilities.json_mode {
            return filter("capability_json_mode", [("need", "json_mode".into())]);
        }
        RuleOutcome::Pass
    }
}

pub struct ContextLengthRule {
    pub safety_factor: f64,
}
impl Rule for ContextLengthRule {
    fn name(&self) -> &str {
        "context_length"
    }
    fn eval(&self, ctx: &RuleCtx, m: &ModelEntry) -> RuleOutcome {
        let needed = (ctx.profile.est_tokens as f64 * self.safety_factor).ceil() as u64
            + ctx.profile.max_tokens.unwrap_or(0) as u64;
        if needed > m.context_window {
            filter(
                "context_overflow",
                [
                    ("needed", needed.to_string()),
                    ("window", m.context_window.to_string()),
                ],
            )
        } else {
            RuleOutcome::Pass
        }
    }
}

pub struct BudgetRule;
impl Rule for BudgetRule {
    fn name(&self) -> &str {
        "budget"
    }
    fn eval(&self, ctx: &RuleCtx, m: &ModelEntry) -> RuleOutcome {
        if let Some(b) = &ctx.constraints.budget {
            if let Some(maxp) = b.max_input_price_per_mtok {
                if m.price.in_per_mtok > maxp {
                    return filter(
                        "budget_exceeded",
                        [
                            ("price", m.price.in_per_mtok.to_string()),
                            ("max", maxp.to_string()),
                        ],
                    );
                }
            }
        }
        RuleOutcome::Pass
    }
}

pub struct VendorRule;
impl Rule for VendorRule {
    fn name(&self) -> &str {
        "vendor"
    }
    fn eval(&self, ctx: &RuleCtx, m: &ModelEntry) -> RuleOutcome {
        let c = ctx.constraints;
        if c.vendor_deny.iter().any(|v| v == &m.vendor) {
            return filter("vendor_denied", [("vendor", m.vendor.clone())]);
        }
        if !c.vendor_allow.is_empty() && !c.vendor_allow.iter().any(|v| v == &m.vendor) {
            return filter("vendor_not_allowed", [("vendor", m.vendor.clone())]);
        }
        RuleOutcome::Pass
    }
}

pub struct TagRequireRule;
impl Rule for TagRequireRule {
    fn name(&self) -> &str {
        "tag_require"
    }
    fn eval(&self, ctx: &RuleCtx, m: &ModelEntry) -> RuleOutcome {
        if let Some(missing) = ctx
            .constraints
            .require_tags
            .iter()
            .find(|t| !m.tags.iter().any(|x| x == *t))
        {
            return filter("tag_missing", [("tag", missing.clone())]);
        }
        RuleOutcome::Pass
    }
}
