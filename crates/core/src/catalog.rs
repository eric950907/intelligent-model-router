//! 模型目录:LiteLLM model_list + enrichment 解析、归一化、校验、内容 hash(spec §4.3/§5.1)。

use crate::model::{validate_model, Capabilities, ModelEntry, ModelStatus, Price};
use serde::Deserialize;
use std::collections::{BTreeMap, HashSet};
use std::path::Path;

#[derive(Debug, thiserror::Error)]
pub enum CatalogError {
    #[error("{0}")]
    Io(String),
    #[error("{0}")]
    Parse(String),
    #[error("catalog 校验失败:\n{}", .0.join("\n"))]
    Validation(Vec<String>),
}

/// 目录来源抽象。默认实现 = LiteLLM model_list + enrichment 加载(spec §4.3)。
pub trait CatalogProvider: Send + Sync {
    fn models(&self) -> &[ModelEntry];
    /// 内容 hash,用于缓存 key 与审计(16 位十六进制)。
    fn version(&self) -> &str;
}

#[derive(Debug, Clone)]
pub struct Catalog {
    /// 按 id 升序(同一 model_name 多条已合并)。
    pub models: Vec<ModelEntry>,
    pub version_hash: String,
    /// 非致命问题(价格不一致、未富化用默认值、enrichment 孤儿条目),validate-catalog 输出。
    pub warnings: Vec<String>,
    /// 被排除的条目(非 chat、缺价格、disabled),validate-catalog 输出。
    pub skipped: Vec<SkippedModel>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SkippedModel {
    pub id: String,
    pub reason: String,
}

// ---------- LiteLLM model_list(忽略无关字段,不设 deny_unknown_fields) ----------

#[derive(Debug, Deserialize)]
struct LiteLlmFile {
    model_list: Vec<LiteLlmEntry>,
}

#[derive(Debug, Deserialize)]
struct LiteLlmEntry {
    model_name: String,
    #[serde(default)]
    model_info: LiteLlmModelInfo,
}

#[derive(Debug, Default, Deserialize)]
struct LiteLlmModelInfo {
    input_cost_per_token: Option<f64>,
    output_cost_per_token: Option<f64>,
    mode: Option<String>,
}

// ---------- enrichment ----------

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct EnrichmentFile {
    #[serde(default)]
    defaults: EnrichmentDefaults,
    #[serde(default)]
    models: BTreeMap<String, EnrichmentEntry>,
}

/// `defaults` 只消费下面两个键(spec §5.1);写其他字段会直接解析报错而非静默丢弃。
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct EnrichmentDefaults {
    context_window: Option<u64>,
    #[serde(default)]
    capabilities: CapabilitiesRaw,
}

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct EnrichmentEntry {
    vendor: Option<String>,
    name: Option<String>,
    description: Option<String>,
    tags: Option<Vec<String>>,
    #[serde(default)]
    capabilities: CapabilitiesRaw,
    context_window: Option<u64>,
    max_output: Option<u64>,
    groups: Option<Vec<String>>,
    quality_notes: Option<String>,
    status: Option<ModelStatus>,
    mode: Option<String>,
    price: Option<Price>,
}

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct CapabilitiesRaw {
    tools: Option<bool>,
    vision: Option<bool>,
    json_mode: Option<bool>,
    audio_in: Option<bool>,
}

impl CapabilitiesRaw {
    fn merge_onto(&self, base: &Capabilities) -> Capabilities {
        Capabilities {
            tools: self.tools.unwrap_or(base.tools),
            vision: self.vision.unwrap_or(base.vision),
            json_mode: self.json_mode.unwrap_or(base.json_mode),
            audio_in: self.audio_in.unwrap_or(base.audio_in),
        }
    }
}

// ---------- 内置启发式(名称 → mode / vendor) ----------

/// 无 mode 字段时的非 chat 名称特征(对 model_name 最后一段做子串匹配)。
const NON_CHAT_PATTERNS: &[(&str, &str)] = &[
    ("embed", "embedding"),
    ("imagen", "image_generation"),
    ("dall", "image_generation"),
    ("-image", "image_generation"),
    ("veo", "video_generation"),
    ("sora", "video_generation"),
    ("seedance", "video_generation"),
    ("happyhorse", "video_generation"),
    ("-video", "video_generation"),
    ("lyria", "music_generation"),
    ("whisper", "audio_transcription"),
    ("tts", "audio_speech"),
    ("rerank", "rerank"),
];

fn guess_mode(model_name: &str) -> &'static str {
    let lower = model_name.to_ascii_lowercase();
    let seg = lower.rsplit('/').next().unwrap_or(&lower);
    for (pat, mode) in NON_CHAT_PATTERNS {
        if seg.contains(pat) {
            return mode;
        }
    }
    "chat"
}

/// vendor 名称前缀推导表(顺序即优先级)。
const VENDOR_PREFIXES: &[(&str, &str)] = &[
    ("gpt", "openai"),
    ("o1", "openai"),
    ("o3", "openai"),
    ("o4", "openai"),
    ("chatgpt", "openai"),
    ("claude", "anthropic"),
    ("gemini", "google"),
    ("gemma", "google"),
    ("deepseek", "deepseek"),
    ("qwen", "alibaba"),
    ("qwq", "alibaba"),
    ("kimi", "moonshot"),
    ("moonshot", "moonshot"),
    ("glm", "zhipu"),
    ("chatglm", "zhipu"),
    ("phi", "microsoft"),
    ("llama", "meta"),
    ("mistral", "mistralai"),
    ("mixtral", "mistralai"),
    ("magistral", "mistralai"),
    ("grok", "xai"),
];

fn guess_vendor(model_name: &str) -> &'static str {
    let lower = model_name.to_ascii_lowercase();
    let seg = lower.rsplit('/').next().unwrap_or(&lower);
    for (prefix, vendor) in VENDOR_PREFIXES {
        if seg.starts_with(prefix) {
            return vendor;
        }
    }
    "unknown"
}

/// 价格 ×1e6 后四舍五入到 6 位小数,消除 f64 换算噪声(2.83e-07 → 0.283)。
/// 极端输入(如 ~1e302)乘 1e6 会溢出为 inf,由 validate_model 的非有限数校验兜底。
fn round6(x: f64) -> f64 {
    (x * 1e6).round() / 1e6
}

const DEFAULT_CONTEXT_WINDOW: u64 = 32768;

// ---------- 构建 ----------

impl Catalog {
    /// 从文件加载(生产路径):model_list = LiteLLM 网关配置;enrichment = 推荐元数据。
    pub fn load(model_list_path: &Path, enrichment_path: &Path) -> Result<Catalog, CatalogError> {
        let read = |p: &Path| {
            std::fs::read_to_string(p)
                .map_err(|e| CatalogError::Io(format!("{}: 读取失败: {e}", p.display())))
        };
        let ml = read(model_list_path)?;
        let en = read(enrichment_path)?;
        Self::from_strs_with_names(
            &ml,
            &en,
            &model_list_path.display().to_string(),
            &enrichment_path.display().to_string(),
        )
    }

    /// 内存构造(测试/调试用;错误信息以 model_list/enrichment 名义)。
    pub fn from_strs(
        model_list_yaml: &str,
        enrichment_yaml: &str,
    ) -> Result<Catalog, CatalogError> {
        Self::from_strs_with_names(model_list_yaml, enrichment_yaml, "model_list", "enrichment")
    }

    fn from_strs_with_names(
        model_list_yaml: &str,
        enrichment_yaml: &str,
        ml_name: &str,
        en_name: &str,
    ) -> Result<Catalog, CatalogError> {
        let file: LiteLlmFile = serde_yaml::from_str(model_list_yaml)
            .map_err(|e| CatalogError::Parse(format!("{ml_name}: YAML 解析失败: {e}")))?;
        let enrich: EnrichmentFile = serde_yaml::from_str(enrichment_yaml)
            .map_err(|e| CatalogError::Parse(format!("{en_name}: YAML 解析失败: {e}")))?;
        Self::build(file, enrich)
    }

    fn build(file: LiteLlmFile, enrich: EnrichmentFile) -> Result<Catalog, CatalogError> {
        let defaults = &enrich.defaults;
        let base_caps = defaults.capabilities.merge_onto(&Capabilities::default());
        let mut warnings = Vec::new();
        let mut skipped = Vec::new();
        let mut by_id: BTreeMap<String, ModelEntry> = BTreeMap::new();

        for e in file.model_list {
            let id = e.model_name.trim().to_string();
            let en = enrich.models.get(&id);

            // ① mode:enrichment > model_info.mode > 名称启发式(spec §5.1)
            let mode = en
                .and_then(|x| x.mode.as_deref())
                .or(e.model_info.mode.as_deref())
                .unwrap_or_else(|| guess_mode(&id))
                .trim()
                .to_ascii_lowercase();
            if mode != "chat" {
                skipped.push(SkippedModel {
                    id,
                    reason: format!("mode={mode}"),
                });
                continue;
            }

            // ② status:disabled 直接排除
            if en.and_then(|x| x.status) == Some(ModelStatus::Disabled) {
                skipped.push(SkippedModel {
                    id,
                    reason: "status=disabled".into(),
                });
                continue;
            }

            // ③ 价格:enrichment 覆盖 > model_list 换算 ×1e6
            let price = en.and_then(|x| x.price.clone()).or_else(|| {
                match (
                    e.model_info.input_cost_per_token,
                    e.model_info.output_cost_per_token,
                ) {
                    (Some(i), Some(o))
                        if i.is_finite() && o.is_finite() && i >= 0.0 && o >= 0.0 =>
                    {
                        Some(Price {
                            in_per_mtok: round6(i * 1e6),
                            out_per_mtok: round6(o * 1e6),
                        })
                    }
                    _ => None,
                }
            });
            let Some(price) = price else {
                skipped.push(SkippedModel {
                    id,
                    reason: "缺价格或非法(model_info 无有效 input/output_cost_per_token;可用 enrichment.price 覆盖)".into(),
                });
                continue;
            };

            // ④ 同名合并:保留首条;价格不一致 → 警告
            if let Some(prev) = by_id.get(&id) {
                if prev.price != price {
                    warnings.push(format!(
                        "{id}: 同名条目价格不一致,保留首条({}/M 输入 / {}/M 输出)",
                        prev.price.in_per_mtok, prev.price.out_per_mtok
                    ));
                }
                continue;
            }

            // ⑤ 其余字段:enrichment > defaults > 内置默认
            let (context_window, ctx_defaulted) = match en.and_then(|x| x.context_window) {
                Some(c) => (c, false),
                None => (
                    defaults.context_window.unwrap_or(DEFAULT_CONTEXT_WINDOW),
                    true,
                ),
            };
            if ctx_defaulted {
                warnings.push(format!(
                    "{id}: 未富化 context_window,使用默认值 {context_window}"
                ));
            }
            let caps = en
                .map(|x| x.capabilities.merge_onto(&base_caps))
                .unwrap_or_else(|| base_caps.clone());
            let vendor = en
                .and_then(|x| x.vendor.clone())
                .unwrap_or_else(|| guess_vendor(&id).to_string());
            if vendor == "unknown" {
                warnings.push(format!("{id}: vendor 未富化且无法从名称推导"));
            }

            by_id.insert(
                id.clone(),
                ModelEntry {
                    name: en
                        .and_then(|x| x.name.clone())
                        .unwrap_or_else(|| id.clone()),
                    id,
                    vendor,
                    description: en.and_then(|x| x.description.clone()).unwrap_or_default(),
                    tags: en.and_then(|x| x.tags.clone()).unwrap_or_default(),
                    capabilities: caps,
                    context_window,
                    max_output: Some(en.and_then(|x| x.max_output).unwrap_or(context_window)),
                    price,
                    groups: en
                        .and_then(|x| x.groups.clone())
                        .unwrap_or_else(|| vec!["default".to_string()]),
                    quality_notes: en.and_then(|x| x.quality_notes.clone()).unwrap_or_default(),
                    status: ModelStatus::Enabled,
                },
            );
        }

        // 同一 id 多条部署:只要最终收录就不再进入排除清单;同 id 同 reason 只保留一条。
        let mut seen_skips = HashSet::new();
        skipped.retain(|s| {
            !by_id.contains_key(&s.id) && seen_skips.insert((s.id.clone(), s.reason.clone()))
        });

        // enrichment 里引用了不在 catalog 的模型 → 警告(陈旧富化清理提示)
        for key in enrich.models.keys() {
            if !by_id.contains_key(key) && !skipped.iter().any(|s| &s.id == key) {
                warnings.push(format!(
                    "enrichment 中的 {key} 不在 model_list(catalog 未收录)"
                ));
            }
        }

        let models: Vec<ModelEntry> = by_id.into_values().collect();
        let mut errs: Vec<String> = models.iter().flat_map(validate_model).collect();
        if models.is_empty() {
            errs.push("catalog 为空(model_list 无可用 chat 模型或全部被排除)".into());
        }
        if !errs.is_empty() {
            return Err(CatalogError::Validation(errs));
        }

        let canonical = serde_json::to_string(&models).expect("ModelEntry 可序列化");
        let hash = blake3::hash(canonical.as_bytes()).to_hex().to_string();
        Ok(Catalog {
            models,
            version_hash: hash[..16].to_string(),
            warnings,
            skipped,
        })
    }
}

impl CatalogProvider for Catalog {
    fn models(&self) -> &[ModelEntry] {
        &self.models
    }
    fn version(&self) -> &str {
        &self.version_hash
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MODEL_LIST: &str = r#"
model_list:
  - model_name: gpt-5.4
    litellm_params: { model: azure/gpt-5.4, api_base: os.environ/X, tpm: 1000000 }
    model_info: { input_cost_per_token: 2.5e-06, output_cost_per_token: 1.5e-05 }
  - model_name: gpt-5.4
    litellm_params: { model: azure/gpt-5.4, api_base: os.environ/Y }
    model_info: { input_cost_per_token: 2.5e-06, output_cost_per_token: 1.5e-05 }
  - model_name: deepseek-v4.1-flash
    litellm_params: { model: openai/deepseek-v4.1-flash }
    model_info: { input_cost_per_token: 2.83e-07, output_cost_per_token: 1.131e-06 }
  - model_name: text-embedding-3-small
    litellm_params: { model: azure/text-embedding-3-small }
    model_info: { input_cost_per_token: 1e-07, output_cost_per_token: 0.0 }
  - model_name: gpt-image-2
    litellm_params: { model: azure/gpt-image-2 }
    model_info: { input_cost_per_token: 5e-06, output_cost_per_token: 4e-05, mode: image_generation }
  - model_name: no-price-model
    litellm_params: { model: openai/no-price-model }
"#;

    const ENRICH: &str = r#"
defaults:
  context_window: 8000
models:
  gpt-5.4:
    vendor: openai
    description: 旗舰通用
    tags: [reasoning, tools]
    capabilities: { tools: true, vision: true }
    context_window: 400000
    max_output: 128000
"#;

    #[test]
    fn loads_dedupes_and_skips_non_chat() {
        let c = Catalog::from_strs(MODEL_LIST, ENRICH).unwrap();
        let ids: Vec<&str> = c.models.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, vec!["deepseek-v4.1-flash", "gpt-5.4"]); // 按 id 升序,同名合并
        let gpt = &c.models[1];
        assert_eq!(
            gpt.price,
            Price {
                in_per_mtok: 2.5,
                out_per_mtok: 15.0
            }
        ); // ×1e6,无浮点噪声
        assert_eq!(gpt.context_window, 400000);
        assert_eq!(gpt.max_output, Some(128000));
        assert_eq!(gpt.vendor, "openai");
        assert!(gpt.capabilities.tools);
        assert!(!gpt.capabilities.json_mode); // 未写字段继承 defaults
        assert_eq!(c.version_hash.len(), 16);
        assert!(c
            .skipped
            .iter()
            .any(|s| s.id == "text-embedding-3-small" && s.reason.contains("embedding")));
        assert!(c
            .skipped
            .iter()
            .any(|s| s.id == "gpt-image-2" && s.reason.contains("image_generation")));
        assert!(c
            .skipped
            .iter()
            .any(|s| s.id == "no-price-model" && s.reason.contains("缺价格")));
    }

    #[test]
    fn defaults_and_vendor_guess_for_unlisted() {
        let c = Catalog::from_strs(MODEL_LIST, ENRICH).unwrap();
        let ds = &c.models[0]; // deepseek-v4.1-flash:未富化
        assert_eq!(ds.vendor, "deepseek"); // 名称前缀推导
        assert_eq!(ds.context_window, 8000); // defaults
        assert!(ds.tags.is_empty());
        assert!(c
            .warnings
            .iter()
            .any(|w| w.contains("deepseek-v4.1-flash") && w.contains("context_window")));
    }

    #[test]
    fn price_mismatch_warns_and_keeps_first() {
        let ml = r#"
model_list:
  - model_name: m
    litellm_params: { model: openai/m }
    model_info: { input_cost_per_token: 1e-06, output_cost_per_token: 2e-06 }
  - model_name: m
    litellm_params: { model: openai/m }
    model_info: { input_cost_per_token: 3e-06, output_cost_per_token: 4e-06 }
"#;
        let c = Catalog::from_strs(ml, "{}").unwrap();
        assert_eq!(c.models.len(), 1);
        assert_eq!(c.models[0].price.in_per_mtok, 1.0);
        assert!(c.warnings.iter().any(|w| w.contains("价格不一致")));
    }

    #[test]
    fn disabled_and_mode_override() {
        let enrich = r#"
models:
  gpt-5.4:
    status: disabled
  gpt-image-2:
    mode: chat
  deepseek-v4.1-flash:
    mode: embedding
"#;
        let c = Catalog::from_strs(MODEL_LIST, enrich).unwrap();
        let ids: Vec<&str> = c.models.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, vec!["gpt-image-2"]); // 强制 chat 收录;其余被 disabled/mode 排除
        assert!(c
            .skipped
            .iter()
            .any(|s| s.id == "gpt-5.4" && s.reason.contains("disabled")));
        assert!(c
            .skipped
            .iter()
            .any(|s| s.id == "deepseek-v4.1-flash" && s.reason.contains("embedding")));
    }

    #[test]
    fn orphan_enrichment_warns() {
        let c = Catalog::from_strs(MODEL_LIST, "models:\n  not-in-list:\n    vendor: x\n").unwrap();
        assert!(c.warnings.iter().any(|w| w.contains("not-in-list")));
    }

    #[test]
    fn hash_stable_and_sensitive() {
        let c1 = Catalog::from_strs(MODEL_LIST, ENRICH).unwrap();
        let c2 = Catalog::from_strs(MODEL_LIST, ENRICH).unwrap();
        assert_eq!(c1.version_hash, c2.version_hash);
        let c3 = Catalog::from_strs(MODEL_LIST, &ENRICH.replace("400000", "500000")).unwrap();
        assert_ne!(c1.version_hash, c3.version_hash);
    }

    #[test]
    fn hash_insensitive_to_entry_order() {
        let a = r#"
model_list:
  - model_name: aa
    litellm_params: { model: openai/aa }
    model_info: { input_cost_per_token: 1e-06, output_cost_per_token: 1e-06 }
  - model_name: bb
    litellm_params: { model: openai/bb }
    model_info: { input_cost_per_token: 2e-06, output_cost_per_token: 2e-06 }
"#;
        let b = r#"
model_list:
  - model_name: bb
    litellm_params: { model: openai/bb }
    model_info: { input_cost_per_token: 2e-06, output_cost_per_token: 2e-06 }
  - model_name: aa
    litellm_params: { model: openai/aa }
    model_info: { input_cost_per_token: 1e-06, output_cost_per_token: 1e-06 }
"#;
        assert_eq!(
            Catalog::from_strs(a, "{}").unwrap().version_hash,
            Catalog::from_strs(b, "{}").unwrap().version_hash
        );
    }

    #[test]
    fn empty_or_all_skipped_catalog_rejected() {
        let ml = r#"
model_list:
  - model_name: text-embedding-ada-002
    litellm_params: { model: azure/text-embedding-ada-002 }
    model_info: { input_cost_per_token: 1e-07, output_cost_per_token: 0.0 }
"#;
        let err = Catalog::from_strs(ml, "{}").unwrap_err().to_string();
        assert!(err.contains("catalog 为空"), "got: {err}");
    }

    #[test]
    fn enrichment_unknown_field_rejected() {
        let err = Catalog::from_strs("model_list: []", "models:\n  m:\n    oops: 1\n")
            .unwrap_err()
            .to_string();
        assert!(err.contains("YAML 解析失败"), "got: {err}");
    }

    #[test]
    fn loads_from_paths() {
        let dir = std::env::temp_dir().join(format!("mrc-catalog-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let ml = dir.join("model_list.yaml");
        let en = dir.join("enrichment.yaml");
        std::fs::write(&ml, MODEL_LIST).unwrap();
        std::fs::write(&en, ENRICH).unwrap();
        let c = Catalog::load(&ml, &en).unwrap();
        assert_eq!(c.models.len(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn duplicate_first_missing_price_uses_second() {
        let ml = r#"
model_list:
  - model_name: m
    litellm_params: { model: openai/m }
  - model_name: m
    litellm_params: { model: openai/m }
    model_info: { input_cost_per_token: 1e-06, output_cost_per_token: 2e-06 }
"#;
        let c = Catalog::from_strs(ml, "{}").unwrap();
        assert_eq!(c.models.len(), 1);
        assert_eq!(
            c.models[0].price,
            Price {
                in_per_mtok: 1.0,
                out_per_mtok: 2.0
            }
        );
        // 早先“缺价格”的排除记录不得与收录结果矛盾
        assert!(!c.skipped.iter().any(|s| s.id == "m"));
    }

    #[test]
    fn enrichment_price_overrides_model_info() {
        let ml = r#"
model_list:
  - model_name: gpt-5.4
    litellm_params: { model: openai/gpt-5.4 }
    model_info: { input_cost_per_token: 1e-06, output_cost_per_token: 2e-06 }
"#;
        let enrich = r#"
models:
  gpt-5.4:
    price: { in_per_mtok: 9.0, out_per_mtok: 10.0 }
"#;
        let c = Catalog::from_strs(ml, enrich).unwrap();
        assert_eq!(
            c.models[0].price,
            Price {
                in_per_mtok: 9.0,
                out_per_mtok: 10.0
            }
        );
    }

    #[test]
    fn explicit_mode_beats_name_heuristic() {
        let ml = r#"
model_list:
  - model_name: my-image-model
    litellm_params: { model: openai/my-image-model }
    model_info: { input_cost_per_token: 1e-06, output_cost_per_token: 2e-06, mode: " chat " }
"#;
        let c = Catalog::from_strs(ml, "{}").unwrap();
        assert_eq!(c.models.len(), 1);
        assert_eq!(c.models[0].id, "my-image-model");
    }

    #[test]
    fn model_name_is_trimmed() {
        let ml = r#"
model_list:
  - model_name: "  gpt-5.4  "
    litellm_params: { model: openai/gpt-5.4 }
    model_info: { input_cost_per_token: 1e-06, output_cost_per_token: 2e-06 }
"#;
        let c = Catalog::from_strs(ml, "{}").unwrap();
        assert_eq!(c.models[0].id, "gpt-5.4");
    }

    #[test]
    fn non_finite_costs_skipped_not_panic() {
        let ml = r#"
model_list:
  - model_name: bad
    litellm_params: { model: openai/bad }
    model_info: { input_cost_per_token: .nan, output_cost_per_token: 1e-06 }
  - model_name: good
    litellm_params: { model: openai/good }
    model_info: { input_cost_per_token: 1e-06, output_cost_per_token: 2e-06 }
"#;
        let c = Catalog::from_strs(ml, "{}").unwrap();
        let ids: Vec<&str> = c.models.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, vec!["good"]);
        assert!(c
            .skipped
            .iter()
            .any(|s| s.id == "bad" && s.reason.contains("缺价格")));
    }

    #[test]
    fn defaults_unknown_field_rejected() {
        let err = Catalog::from_strs(MODEL_LIST, "defaults:\n  vendor: x\n")
            .unwrap_err()
            .to_string();
        assert!(err.contains("YAML 解析失败"), "got: {err}");
    }
}
