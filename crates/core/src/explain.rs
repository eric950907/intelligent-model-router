//! 理由渲染(spec §6.5): 模板化、zh/en、零额外 LLM 调用。
//! 模板只做 `{{ var }}` 插值;所有数值/措辞在 Rust 侧预格式化(降低模板能力依赖)。

use crate::api::Locale;
use crate::config::PriceBands;
use crate::fusion::Weights;
use crate::model::ModelEntry;
use crate::profile::Profile;
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Debug, thiserror::Error)]
pub enum ExplainError {
    #[error("模板文件读取失败 {0}: {1}")]
    Io(String, std::io::Error),
    #[error("模板 TOML 解析失败 {0}: {1}")]
    Toml(String, toml::de::Error),
    #[error("模板渲染失败: {0}")]
    Template(String),
}

#[derive(Debug, serde::Deserialize)]
struct TemplateFile {
    hard_constraints: String,
    hard_constraints_basic: String,
    quality: String,
    cost_with_budget: String,
    cost_without_budget: String,
    low_confidence: String,
    penalty: String,
    degraded: String,
    band_cheap: String,
    band_mid: String,
    band_high: String,
    #[serde(default)]
    quality_note_default: String,
    #[serde(default)]
    quality_notes: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PenaltyView {
    pub value: f32,
    pub reason_key: String,
}

/// 语义判断视图(降级时为 None)。
#[derive(Debug, Clone)]
pub struct VerdictView {
    pub quality_norm: f32,
    pub cost_norm: f32,
    pub weights: Weights,
    pub confidence: f32,
    pub low_confidence: bool,
    /// (档位标签, 概率) —— 概率最高的档位
    pub quality_top: Option<(String, f32)>,
    pub cost_top: Option<(String, f32)>,
    pub task_type: Option<String>,
}

pub struct ReasonCtx<'a> {
    pub rank: u32,
    pub model: &'a ModelEntry,
    pub profile: &'a Profile,
    pub safety_factor: f64,
    pub rule_hits: &'a [String],
    pub penalties: &'a [PenaltyView],
    pub degraded: bool,
    pub verdict: Option<VerdictView>,
    /// 调用方是否提供了预算约束(选择成本行措辞);由 pipeline 填充
    pub budget_present: bool,
    /// 价格档位映射(来自 questions.yaml);由 pipeline 填充
    pub price_bands: PriceBands,
}

pub trait ReasonRenderer: Send + Sync {
    fn render(&self, ctx: &ReasonCtx, locale: Locale) -> Vec<String>;
}

pub struct TemplateReasonRenderer {
    zh: TemplateFile,
    en: TemplateFile,
}

impl TemplateReasonRenderer {
    pub fn from_files(zh_path: &Path, en_path: &Path) -> Result<Self, ExplainError> {
        let load = |p: &Path| -> Result<TemplateFile, ExplainError> {
            let s = std::fs::read_to_string(p)
                .map_err(|e| ExplainError::Io(p.display().to_string(), e))?;
            let t: TemplateFile =
                toml::from_str(&s).map_err(|e| ExplainError::Toml(p.display().to_string(), e))?;
            validate_templates(p, &t)?;
            Ok(t)
        };
        Ok(Self {
            zh: load(zh_path)?,
            en: load(en_path)?,
        })
    }

    /// 便捷构造:templates_dir/reason.{zh,en}.toml
    pub fn from_dir(dir: &Path) -> Result<Self, ExplainError> {
        Self::from_files(&dir.join("reason.zh.toml"), &dir.join("reason.en.toml"))
    }
}

/// 加载时一次性校验全部模板的 minijinja 语法:坏模板必须在启动期报 `Template`,
/// 否则请求期 `render` 里的 `expect` 会 panic(TOML 合法 ≠ 模板语法合法)。
fn validate_templates(path: &Path, t: &TemplateFile) -> Result<(), ExplainError> {
    let env = minijinja::Environment::new();
    let probe = serde_json::json!({
        "ctx": 1, "est": 1, "safety": 1.15, "caps": "x", "q": "0.00", "note": "x",
        "top_part": "x", "c": "0.00", "in": "$0.00", "band": "x", "conf": "0.00",
        "value": "0.00", "reason": "x",
    });
    for (key, tpl) in [
        ("hard_constraints", &t.hard_constraints),
        ("hard_constraints_basic", &t.hard_constraints_basic),
        ("quality", &t.quality),
        ("cost_with_budget", &t.cost_with_budget),
        ("cost_without_budget", &t.cost_without_budget),
        ("low_confidence", &t.low_confidence),
        ("penalty", &t.penalty),
        ("degraded", &t.degraded),
        ("band_cheap", &t.band_cheap),
        ("band_mid", &t.band_mid),
        ("band_high", &t.band_high),
    ] {
        env.render_str(tpl, &probe)
            .map_err(|e| ExplainError::Template(format!("{}:{key}: {e}", path.display())))?;
    }
    // quality_note_default / quality_notes 只作为数据插入 quality 模板,无需单独渲染。
    Ok(())
}

impl ReasonRenderer for TemplateReasonRenderer {
    fn render(&self, ctx: &ReasonCtx, locale: Locale) -> Vec<String> {
        let (t, caps_join, top_fmt): (&TemplateFile, &str, fn(&str, f32) -> String) = match locale {
            Locale::Zh => (&self.zh, "、", zh_top),
            Locale::En => (&self.en, ", ", en_top),
        };
        let mut out = Vec::new();

        if ctx.degraded {
            out.push(render_tpl(&t.degraded, &serde_json::json!({})).expect("常量模板"));
        }

        // 硬约束行:caps = profile 需要的、且模型满足的能力(过滤后必然全满足)
        let mut caps_vec: Vec<&str> = Vec::new();
        if ctx.profile.tools_count > 0 && ctx.model.capabilities.tools {
            caps_vec.push(match locale {
                Locale::Zh => "支持 tool_calls",
                Locale::En => "tool calling supported",
            });
        }
        if ctx.profile.has_images && ctx.model.capabilities.vision {
            caps_vec.push(match locale {
                Locale::Zh => "支持视觉",
                Locale::En => "vision supported",
            });
        }
        if ctx.profile.json_mode && ctx.model.capabilities.json_mode {
            caps_vec.push(match locale {
                Locale::Zh => "支持 JSON 模式",
                Locale::En => "JSON mode supported",
            });
        }
        let vars = serde_json::json!({
            "ctx": ctx.model.context_window,
            "est": ctx.profile.est_tokens,
            "safety": ctx.safety_factor,
            "caps": caps_vec.join(caps_join),
        });
        let tpl = if caps_vec.is_empty() {
            &t.hard_constraints_basic
        } else {
            &t.hard_constraints
        };
        out.push(render_tpl(tpl, &vars).expect("硬约束模板"));

        // 质量/成本/置信度/软惩罚(降级时跳过前三类)
        if let Some(v) = &ctx.verdict {
            let note = v
                .task_type
                .as_deref()
                .and_then(|tt| t.quality_notes.get(tt))
                .unwrap_or(&t.quality_note_default)
                .clone();
            let top_part = v
                .quality_top
                .as_ref()
                .map(|(label, p)| top_fmt(label, *p))
                .unwrap_or_default();
            out.push(
                render_tpl(
                    &t.quality,
                    &serde_json::json!({
                        "q": format!("{:.2}", v.quality_norm),
                        "note": note,
                        "top_part": top_part,
                    }),
                )
                .expect("quality 模板"),
            );

            let band =
                match crate::questions::price_band(ctx.model.price.in_per_mtok, &ctx.price_bands) {
                    crate::questions::PriceBand::Cheap => &t.band_cheap,
                    crate::questions::PriceBand::Mid => &t.band_mid,
                    crate::questions::PriceBand::High => &t.band_high,
                };
            let cost_tpl = if ctx.budget_present {
                &t.cost_with_budget
            } else {
                &t.cost_without_budget
            };
            out.push(
                render_tpl(
                    cost_tpl,
                    &serde_json::json!({
                        "c": format!("{:.2}", v.cost_norm),
                        "in": format!("${:.2}", ctx.model.price.in_per_mtok),
                        "band": band,
                    }),
                )
                .expect("cost 模板"),
            );

            if v.low_confidence {
                out.push(
                    render_tpl(
                        &t.low_confidence,
                        &serde_json::json!({
                            "conf": format!("{:.2}", v.confidence),
                        }),
                    )
                    .expect("low_confidence 模板"),
                );
            }
        }

        for p in ctx.penalties {
            out.push(
                render_tpl(
                    &t.penalty,
                    &serde_json::json!({
                        "value": format!("{:.2}", p.value),
                        "reason": p.reason_key,
                    }),
                )
                .expect("penalty 模板"),
            );
        }
        out
    }
}

/// zh: ` (概率集中在「最佳匹配」61%)`;en: ` (concentrated on "best match" 61%)`
fn zh_top(label: &str, p: f32) -> String {
    format!(" (概率集中在「{label}」{:.0}%)", p * 100.0)
}
fn en_top(label: &str, p: f32) -> String {
    format!(" (concentrated on \"{label}\" {:.0}%)", p * 100.0)
}

fn render_tpl(tpl: &str, ctx: &serde_json::Value) -> Result<String, ExplainError> {
    let env = minijinja::Environment::new();
    env.render_str(tpl, ctx)
        .map_err(|e| ExplainError::Template(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::QuestionsConfig;
    use crate::model::{Capabilities, ModelStatus, Price};

    fn renderer() -> TemplateReasonRenderer {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../templates");
        TemplateReasonRenderer::from_dir(&dir).expect("仓库 templates/ 必须存在且可解析")
    }

    fn shipped_price_bands() -> PriceBands {
        QuestionsConfig::from_yaml(include_str!("../../../configs/questions.yaml"))
            .expect("仓库 questions.yaml 必须可解析")
            .price_bands
    }

    fn fixed_model() -> ModelEntry {
        ModelEntry {
            id: "m1".into(),
            name: "m1".into(),
            vendor: "v1".into(),
            description: "调试助手".into(),
            tags: vec!["tools".into()],
            capabilities: Capabilities {
                tools: true,
                vision: true,
                ..Default::default()
            },
            context_window: 128_000,
            max_output: Some(16_384),
            price: Price {
                in_per_mtok: 2.5,
                out_per_mtok: 15.0,
            },
            groups: vec!["default".into()],
            quality_notes: String::new(),
            status: ModelStatus::Enabled,
        }
    }

    fn fixed_profile() -> Profile {
        let mut p = Profile::from_text("修 bug").expect("测试文本应可解析");
        p.tools_count = 2;
        p.has_images = true;
        p.json_mode = false;
        p.est_tokens = 8000;
        p.max_tokens = None;
        p.locale_hint = Some("zh".into());
        p.task_hints = vec![];
        p
    }

    fn fixed_verdict() -> VerdictView {
        VerdictView {
            quality_norm: 0.92,
            cost_norm: 0.50,
            weights: Weights {
                quality_fit: 0.65,
                cost_fit: 0.35,
            },
            confidence: 0.45,
            low_confidence: true,
            quality_top: Some(("最佳匹配".into(), 0.61)),
            cost_top: None,
            task_type: Some("code_debug".into()),
        }
    }

    /// 测试场景开关(默认:有 verdict、非降级、有预算)。
    struct CtxOpts {
        degraded: bool,
        verdict: Option<VerdictView>,
        budget_present: bool,
    }

    impl Default for CtxOpts {
        fn default() -> Self {
            Self {
                degraded: false,
                verdict: None,
                budget_present: true,
            }
        }
    }

    fn ctx<'a>(
        model: &'a ModelEntry,
        profile: &'a Profile,
        rule_hits: &'a [String],
        penalties: &'a [PenaltyView],
        opts: CtxOpts,
    ) -> ReasonCtx<'a> {
        ReasonCtx {
            rank: 1,
            model,
            profile,
            safety_factor: 1.15,
            rule_hits,
            penalties,
            degraded: opts.degraded,
            verdict: opts.verdict,
            budget_present: opts.budget_present,
            price_bands: shipped_price_bands(),
        }
    }

    fn fixed_inputs() -> (
        ModelEntry,
        Profile,
        Vec<String>,
        Vec<PenaltyView>,
        VerdictView,
    ) {
        (
            fixed_model(),
            fixed_profile(),
            vec!["context_length:ok".to_string()],
            vec![PenaltyView {
                value: 0.10,
                reason_key: "vendor_preference".into(),
            }],
            fixed_verdict(),
        )
    }

    #[test]
    fn zh_reasons_snapshot() {
        let r = renderer();
        let (model, profile, rule_hits, penalties, verdict) = fixed_inputs();
        let ctx = ctx(
            &model,
            &profile,
            &rule_hits,
            &penalties,
            CtxOpts {
                verdict: Some(verdict),
                ..Default::default()
            },
        );
        let lines = r.render(&ctx, Locale::Zh);
        insta::assert_snapshot!("zh_reasons", lines.join("\n"));
    }

    #[test]
    fn en_reasons_snapshot() {
        let r = renderer();
        let (model, profile, rule_hits, penalties, verdict) = fixed_inputs();
        let ctx = ctx(
            &model,
            &profile,
            &rule_hits,
            &penalties,
            CtxOpts {
                verdict: Some(verdict),
                ..Default::default()
            },
        );
        let lines = r.render(&ctx, Locale::En);
        insta::assert_snapshot!("en_reasons", lines.join("\n"));
    }

    #[test]
    fn degraded_ctx_has_no_quality_or_cost_lines() {
        let r = renderer();
        let (model, profile, rule_hits, penalties, _) = fixed_inputs();
        let ctx = ctx(
            &model,
            &profile,
            &rule_hits,
            &penalties,
            CtxOpts {
                degraded: true,
                verdict: None,
                budget_present: false,
            },
        );
        let lines = r.render(&ctx, Locale::Zh);
        assert!(
            lines[0].contains("未经语义判断"),
            "首行应为降级提示,got: {lines:?}"
        );
        assert!(
            !lines.iter().any(|l| l.contains("质量匹配")),
            "got: {lines:?}"
        );
        assert!(
            !lines.iter().any(|l| l.contains("成本适配")),
            "got: {lines:?}"
        );
    }

    #[test]
    fn no_capability_needs_uses_basic_hard_constraints_template() {
        let r = renderer();
        let (model, mut profile, rule_hits, penalties, verdict) = fixed_inputs();
        profile.tools_count = 0;
        profile.has_images = false;
        profile.json_mode = false;
        let ctx = ctx(
            &model,
            &profile,
            &rule_hits,
            &penalties,
            CtxOpts {
                verdict: Some(verdict),
                ..Default::default()
            },
        );
        let lines = r.render(&ctx, Locale::Zh);
        assert!(
            !lines[0].ends_with(';'),
            "无 caps 需求时不应以 `;` 结尾,got: {}",
            lines[0]
        );
        assert_eq!(
            lines[0],
            "✅ 硬约束全部满足:上下文 128000 ≥ 估算 8000;safety 系数 1.15"
        );
    }

    #[test]
    fn no_budget_uses_unconstrained_cost_wording() {
        let r = renderer();
        let (model, profile, rule_hits, penalties, verdict) = fixed_inputs();
        let ctx = ctx(
            &model,
            &profile,
            &rule_hits,
            &penalties,
            CtxOpts {
                verdict: Some(verdict),
                budget_present: false,
                ..Default::default()
            },
        );
        let lines = r.render(&ctx, Locale::Zh);
        let cost_line = lines
            .iter()
            .find(|l| l.contains("成本适配"))
            .expect("应有成本行");
        assert!(cost_line.contains("未设置预算约束"), "got: {cost_line}");
    }

    #[test]
    fn confident_verdict_omits_low_confidence_line() {
        let r = renderer();
        let (model, profile, rule_hits, penalties, mut verdict) = fixed_inputs();
        verdict.low_confidence = false;
        let ctx = ctx(
            &model,
            &profile,
            &rule_hits,
            &penalties,
            CtxOpts {
                verdict: Some(verdict),
                ..Default::default()
            },
        );
        let lines = r.render(&ctx, Locale::Zh);
        assert!(
            !lines.iter().any(|l| l.contains("置信度偏低")),
            "got: {lines:?}"
        );
    }

    #[test]
    fn bad_toml_and_missing_file_rejected() {
        let dir =
            std::env::temp_dir().join(format!("mrc-test-explain-bad-toml-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let bad = dir.join("reason.zh.toml");
        std::fs::write(&bad, "{{{").unwrap();
        let err = match TemplateReasonRenderer::from_files(&bad, &dir.join("reason.en.toml")) {
            Ok(_) => panic!("坏 TOML 应被拒绝"),
            Err(e) => e,
        };
        assert!(matches!(err, ExplainError::Toml(_, _)), "got: {err:?}");

        let err = match TemplateReasonRenderer::from_files(
            &dir.join("missing.zh.toml"),
            &dir.join("missing.en.toml"),
        ) {
            Ok(_) => panic!("缺失文件应被拒绝"),
            Err(e) => e,
        };
        assert!(matches!(err, ExplainError::Io(_, _)), "got: {err:?}");
    }

    /// 模板文案漂移守卫:这三个 note 曾被无意改写("…类任务"),快照只覆盖 code_debug,故单独断言。
    #[test]
    fn quality_notes_cover_creative_summarization_extraction() {
        let r = renderer();
        let (model, profile, rule_hits, penalties, mut verdict) = fixed_inputs();
        for (task_type, expected) in [
            ("creative", "适合创意写作任务"),
            ("summarization", "适合摘要提炼任务"),
            ("extraction", "适合结构化抽取任务"),
        ] {
            verdict.task_type = Some(task_type.into());
            let ctx = ctx(
                &model,
                &profile,
                &rule_hits,
                &penalties,
                CtxOpts {
                    verdict: Some(verdict.clone()),
                    ..Default::default()
                },
            );
            let lines = r.render(&ctx, Locale::Zh);
            assert!(
                lines.iter().any(|l| l.contains(expected)),
                "{task_type}: got {lines:?}"
            );
        }
    }

    /// TOML 合法但模板语法非法 → 加载期 `ExplainError::Template`(而非请求期 panic)。
    #[test]
    fn bad_template_syntax_rejected_at_load() {
        let repo = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../templates");
        let dir = std::env::temp_dir().join(format!(
            "mrc-test-explain-bad-syntax-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let zh = std::fs::read_to_string(repo.join("reason.zh.toml")).unwrap();
        let bad = zh.replace("{{ q }}", "{{ q ");
        assert_ne!(bad, zh, "仓库模板应包含 {{{{ q }}}} 占位");
        std::fs::write(dir.join("reason.zh.toml"), bad).unwrap();
        std::fs::copy(repo.join("reason.en.toml"), dir.join("reason.en.toml")).unwrap();

        let err = match TemplateReasonRenderer::from_dir(&dir) {
            Ok(_) => panic!("坏模板语法应在加载期被拒绝"),
            Err(e) => e,
        };
        assert!(matches!(err, ExplainError::Template(_)), "got: {err:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 能力行只声明模型真正具备的能力(与注释一致,不依赖 capability 规则一定启用)。
    #[test]
    fn caps_line_reflects_model_capabilities() {
        let r = renderer();
        let (mut model, profile, rule_hits, penalties, verdict) = fixed_inputs();
        model.capabilities = Default::default();
        let ctx = ctx(
            &model,
            &profile,
            &rule_hits,
            &penalties,
            CtxOpts {
                verdict: Some(verdict),
                ..Default::default()
            },
        );
        let lines = r.render(&ctx, Locale::Zh);
        let hard = lines.first().expect("应有硬约束行");
        assert!(
            !hard.contains("支持 tool_calls") && !hard.contains("支持视觉"),
            "got: {hard}"
        );
        assert!(!hard.ends_with(';'), "got: {hard}");
    }
}
