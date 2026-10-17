//! 模型条目数据模型与校验(catalog.rs 从 LiteLLM model_list + enrichment 归一化构建)。

use serde::{Deserialize, Serialize};

/// 归一化后的目录条目。不实现 `Deserialize`——由 catalog.rs 逐字段构建(spec §5.1)。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ModelEntry {
    pub id: String,
    pub name: String,
    pub vendor: String,
    pub description: String,
    pub tags: Vec<String>,
    pub capabilities: Capabilities,
    pub context_window: u64,
    pub max_output: Option<u64>,
    pub price: Price,
    pub groups: Vec<String>,
    pub quality_notes: String,
    pub status: ModelStatus,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize)]
pub struct Capabilities {
    pub tools: bool,
    pub vision: bool,
    pub json_mode: bool,
    pub audio_in: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Price {
    pub in_per_mtok: f64,
    pub out_per_mtok: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ModelStatus {
    #[default]
    Enabled,
    Disabled,
}

/// 返回全部校验错误(空 vec = 通过)。validate-catalog 会逐条打印。
pub fn validate_model(m: &ModelEntry) -> Vec<String> {
    let mut errs = Vec::new();
    if m.id.trim().is_empty() {
        errs.push("id 不能为空".into());
    }
    if m.name.trim().is_empty() {
        errs.push(format!("{}: name 不能为空", m.id));
    }
    if m.vendor.trim().is_empty() {
        errs.push(format!("{}: vendor 不能为空", m.id));
    }
    if m.context_window == 0 {
        errs.push(format!("{}: context_window 必须 > 0", m.id));
    }
    if let Some(mo) = m.max_output {
        if mo == 0 {
            errs.push(format!("{}: max_output 必须 > 0", m.id));
        }
    }
    for (name, v) in [
        ("in_per_mtok", m.price.in_per_mtok),
        ("out_per_mtok", m.price.out_per_mtok),
    ] {
        if !v.is_finite() || v < 0.0 {
            errs.push(format!("{}: price.{name} 必须为非负有限数", m.id));
        }
    }
    errs
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_model() -> ModelEntry {
        ModelEntry {
            id: "m1".into(),
            name: "M1".into(),
            vendor: "v1".into(),
            description: String::new(),
            tags: vec!["chat".into()],
            capabilities: Capabilities {
                tools: true,
                vision: false,
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

    #[test]
    fn valid_model_passes() {
        assert!(validate_model(&base_model()).is_empty());
    }

    #[test]
    fn validation_catches_errors() {
        let mut m = base_model();
        m.context_window = 0;
        assert!(validate_model(&m)
            .iter()
            .any(|e| e.contains("context_window")));

        let mut m2 = base_model();
        m2.price.in_per_mtok = -1.0;
        assert!(validate_model(&m2).iter().any(|e| e.contains("price")));

        let mut m3 = base_model();
        m3.id = "  ".into();
        assert!(validate_model(&m3).iter().any(|e| e.contains("id")));
    }

    #[test]
    fn empty_tags_allowed() {
        // tags 由 enrichment 富化,允许为空(validate-catalog 以警告形式提示)
        let mut m = base_model();
        m.tags.clear();
        assert!(validate_model(&m).is_empty());
    }
}
