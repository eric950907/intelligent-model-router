# 模型推荐服务 v1 核心(里程碑 M0 + M1)实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 从空仓库实现到"真实 Jev 端到端可用":输入原始请求体 + 可选约束,输出 top-K 模型推荐及中/英模板理由;支持离线 `dry-run`、`/debug` 调试页、后端不可用时降级排序。

**Spec(唯一事实源):** `docs/superpowers/specs/2026-10-16-model-recommender-design.md`,下文引用写作"spec §N"。本计划与 spec 冲突时,以 spec 为准并停下来修正计划。

**Architecture:** Rust workspace 四 crate。流水线(全在 `recommender-core`,纯逻辑无网络):
`ProfileExtractor → HardRuleEngine → CoarseRank/分片规划 → systemone 批量调用(judge) → Fusion → Explain`。
`recommender-systemone` 是 `/v1/systemone` 契约客户端;`recommender-server` 用 axum 组装;`recommender-cli` 提供 `validate-catalog` / `dry-run`。

**Tech Stack:** Rust 2021(MSRV 1.85+)、axum 0.8、tokio、reqwest(rustls)、serde/serde_json/serde_yaml、minijinja、clap、blake3;dev: wiremock、insta、tower。

**范围(重要):** 本计划 = M0 骨架 + M1 核心。**明确不含**(属 M2,之后另写计划):缓存(moka)、重试/退避/总预算/信号量(spec 8.2 的完整重试语义,本计划只实现"单次尝试 + 失败降级")、指标/审计日志、`eval` 工具、Docker、README 之外的运维文档。本计划中出现的降级路径是最简正确版本,后续 M2 计划在其上加厚。

**约定:**
- 所有命令在仓库根 `/Users/mapxus/model-recommender/` 执行。
- 代码块中 `…` 表示"此处有省略说明",执行者按上下文补全;未标注省略的代码块应逐字采用。
- 每个 Task 末尾必须提交(commit message 用英文,风格与仓库现存一致: `feat(core): …`)。
- Rust 代码遵循 `cargo fmt` 默认风格,提交前跑 `cargo fmt --all`。

---

## 前置条件(开工前核对)

1. **工具链**:Rust ≥ 1.85(本机当前 1.76,Task 1 处理)。
2. **网络**:GitHub 直连在本机不稳定,仓库已配 `git config --local http.proxy http://127.0.0.1:7897`;crates.io 拉依赖若缓慢,先 `export https_proxy=http://127.0.0.1:7897 http_proxy=http://127.0.0.1:7897`。
3. **真实联调(Task 15/20/21)**:需要仓库根 `.env` 中的 `TYPESAFE_API_KEY` 与本机代理 `http://127.0.0.1:7897`(区域限制,spec §7 实测)。**不做真实联调不影响其余任务**(20/21 有降级 fallback 说明);`.env` 已 gitignore,严禁提交。
4. **工作区**:`git status` 干净;`docs/superpowers/specs/` 里 spec 已批准(状态行"已批准")。

---

## 文件结构(全部新建,标注所属 Task)

```
Cargo.toml                              # T1  workspace(成员/共享依赖/MSRV)
crates/core/Cargo.toml                  # T1  recommender-core
crates/core/src/lib.rs                  # T1  模块导出(后续任务追加 pub mod)
crates/core/src/api.rs                  # T4  HTTP DTO(推荐请求/响应/约束/选项/错误码)
crates/core/src/model.rs                # T2  ModelEntry/Capabilities/Price/校验
crates/core/src/catalog.rs              # T2  LiteLLM model_list + enrichment 加载、hash、CatalogProvider
crates/core/src/config.rs               # T3  rules/questions/server serde 模型 + load_configs(dir)(catalog 由 server.yaml 路径装配)
crates/core/src/profile.rs              # T5  ProfileExtractor(OpenAI/Claude 容错解析)
crates/core/src/rules/mod.rs            # T6  Rule trait、RuleCtx、引擎与命中记录
crates/core/src/rules/builtin.rs        # T6  5 条内置规则
crates/core/src/coarse.rs               # T7  needs/重叠率/价格分位/粗排
crates/core/src/judgment.rs             # T8  JudgeRequest/Response/Error、JudgmentBackend、容错解码
crates/core/src/mock.rs                 # T9  MockBackend(确定性,供测试与离线运行)
crates/core/src/questions.rs            # T10 CandidateView、state/问题构造;T11 分片规划
crates/core/src/fusion.rs               # T12 权重/归一化/融合/tie-break/置信度
crates/core/src/explain.rs              # T13 ReasonRenderer + 模板渲染(minijinja)
crates/core/src/pipeline.rs             # T14 prepare/recommend 全流水线
crates/core/tests/golden_e2e.rs         # T20 录制夹具离线全链路快照
crates/systemone/Cargo.toml             # T1  recommender-systemone
crates/systemone/src/lib.rs             # T15 SystemoneClient(鉴权/代理/超时/错误映射)
crates/systemone/tests/contract.rs      # T15 wiremock 契约测试
crates/server/Cargo.toml                # T1  recommender-server
crates/server/src/main.rs               # T17 启动、tracing、状态装配
crates/server/src/app.rs                # T17 Router/中间件/AppState
crates/server/src/handlers.rs           # T17 各端点 handler
crates/server/src/backends.rs           # T16 kind→JudgmentBackend 注册表
crates/server/src/error.rs              # T17 错误信封映射
crates/server/assets/debug.html         # T18 调试页(include_str!)
crates/server/tests/api.rs              # T17 oneshot 路由测试
crates/cli/Cargo.toml                   # T1  recommender-cli
crates/cli/src/main.rs                  # T19 clap: validate-catalog / dry-run
crates/cli/tests/cli.rs                 # T19 CARGO_BIN_EXE 集成测试
configs/model_list.yaml                 # T2  LiteLLM model_list 格式示例(与网关同构,可指向真实配置)
configs/enrichment.yaml                 # T2  富化:vendor/tags/capabilities/context_window…
configs/rules.yaml                      # T3  = spec §5.2 逐字
configs/questions.yaml                  # T3  = spec §5.3 逐字
configs/server.yaml                     # T3  = spec §5.4(含 catalog 段)+ templates_dir 扩展
templates/reason.zh.toml                # T13 中文理由模板
templates/reason.en.toml                # T13 英文理由模板
examples/sample-request.json            # T20 试用样例(OpenAI 风格)
eval/fixtures/jev_code_debug.json       # T20 录制夹具(真实响应;无 key 时合成占位)
eval/fixtures/README.md                 # T20 夹具说明与重录步骤
crates/core/tests/snapshots/            # T20 insta 快照(随测试提交)
README.md                               # T21 最小快速开始
```

---

## Task 1: 工具链升级与 workspace 骨架

**Files:**
- Create: `Cargo.toml`、`crates/core/Cargo.toml`、`crates/systemone/Cargo.toml`、`crates/server/Cargo.toml`、`crates/cli/Cargo.toml`、各 crate 的 `src/lib.rs`/`src/main.rs` 占位、`.gitignore`(追加,已存在)

- [ ] **Step 1: 升级 Rust 工具链**

本机是 Homebrew rust 1.76(2024 年),新 crate 普遍要求 ≥1.85。优先:

```bash
brew upgrade rust
cargo --version    # 期望 ≥ 1.85(2026 年应为 1.9x)
```

brew 卡网络时先设代理再重试:`export ALL_PROXY=http://127.0.0.1:7897 HOMEBREW_NO_AUTO_UPDATE=1`。
若 brew 路线失败,fallback 用 rustup(国内镜像加速):

```bash
RUSTUP_DIST_SERVER=https://rsproxy.cn RUSTUP_UPDATE_ROOT=https://rsproxy.cn/rustup \
  bash -c 'curl --proto "=https" --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal --component rustfmt --component clippy'
source "$HOME/.cargo/env"
cargo --version
```

确认 `rustfmt`、`clippy` 可用(`cargo fmt --version`;`cargo clippy --version`)。

- [ ] **Step 2: 写 workspace 根与四个 crate 清单**

`Cargo.toml`(根,逐字):

```toml
[workspace]
resolver = "2"
members = ["crates/core", "crates/systemone", "crates/server", "crates/cli"]

[workspace.package]
version = "0.1.0"
edition = "2021"
rust-version = "1.85"

[workspace.dependencies]
serde = { version = "1", features = ["derive"] }
serde_json = "1"
serde_yaml = "0.9"
thiserror = "2"
anyhow = "1"
blake3 = "1"
futures = "0.3"
minijinja = "2"
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
axum = "0.8"
reqwest = { version = "0.12", default-features = false, features = ["json", "rustls-tls", "http2"] }
uuid = { version = "1", features = ["v4"] }
clap = { version = "4", features = ["derive"] }
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }
tower = { version = "0.5", features = ["util"] }
wiremock = "0.6"
insta = "1"
```

> 若 `reqwest` 报 "unknown feature http2",删掉该项(退化为 HTTP/1.1,不影响功能);同理以 cargo 报错为准微调版本号,不改变结构。

`crates/core/Cargo.toml`:

```toml
[package]
name = "recommender-core"
version.workspace = true
edition.workspace = true
rust-version.workspace = true

[dependencies]
serde = { workspace = true }
serde_json = { workspace = true }
serde_yaml = { workspace = true }
thiserror = { workspace = true }
blake3 = { workspace = true }
futures = { workspace = true }
minijinja = { workspace = true }

[dev-dependencies]
insta = { workspace = true }
tokio = { workspace = true }
```

`crates/systemone/Cargo.toml`:

```toml
[package]
name = "recommender-systemone"
version.workspace = true
edition.workspace = true
rust-version.workspace = true

[dependencies]
recommender-core = { path = "../core" }
reqwest = { workspace = true }
serde = { workspace = true }
serde_json = { workspace = true }
thiserror = { workspace = true }
futures = { workspace = true }

[dev-dependencies]
tokio = { workspace = true }
wiremock = { workspace = true }
```

`crates/server/Cargo.toml`:

```toml
[package]
name = "recommender-server"
version.workspace = true
edition.workspace = true
rust-version.workspace = true

[dependencies]
recommender-core = { path = "../core" }
recommender-systemone = { path = "../systemone" }
axum = { workspace = true }
tokio = { workspace = true }
serde = { workspace = true }
serde_json = { workspace = true }
uuid = { workspace = true }
anyhow = { workspace = true }
tracing = { workspace = true }
tracing-subscriber = { workspace = true }

[dev-dependencies]
tower = { workspace = true }
```

`crates/cli/Cargo.toml`:

```toml
[package]
name = "recommender-cli"
version.workspace = true
edition.workspace = true
rust-version.workspace = true

[dependencies]
recommender-core = { path = "../core" }
clap = { workspace = true }
serde_json = { workspace = true }
anyhow = { workspace = true }
```

- [ ] **Step 3: 占位源码 + .gitignore**

`crates/core/src/lib.rs`(占位,后续任务逐个加 `pub mod`):

```rust
//! recommender-core: 模型推荐纯逻辑(画像/规则/提问/融合/理由)。
```

`crates/systemone/src/lib.rs`:`//! recommender-systemone: /v1/systemone 契约客户端。`
`crates/server/src/main.rs`:

```rust
fn main() {
    println!("recommender-server 骨架");
}
```

`crates/cli/src/main.rs`:同上风格占位。

`.gitignore` 追加(若已存在条目则不重复):`target/`、`.env`、`.DS_Store`、`*.snap.new`;确认 `.env` 在其中(`git check-ignore .env` 应输出 `.env`)。

- [ ] **Step 4: 验证**

```bash
cargo check --workspace    # 期望:Compiling 各 crate 后 Finished
cargo fmt --all
cargo clippy --workspace --all-targets   # 期望:无 error(warning 可暂留)
```

- [ ] **Step 5: Commit**

```bash
git add -A && git commit -m "chore: scaffold rust workspace with four crates"
git push
```

---

## Task 2: core::model + core::catalog(LiteLLM model_list + enrichment)+ configs/{model_list,enrichment}.yaml

**背景**: catalog 输入 = 网关同构的 LiteLLM `model_list`(发起人 2026-10-16 选定;spec §5.1),`catalog.model_list_path` 可直接指向网关真实配置;推荐所需上下文/能力/标签由 `enrichment.yaml` 富化(字段级覆盖,缺省走 defaults)。

**Files:**
- Create: `crates/core/src/model.rs`、`crates/core/src/catalog.rs`、`configs/model_list.yaml`、`configs/enrichment.yaml`
- Modify: `crates/core/src/lib.rs`(加 `pub mod model; pub mod catalog;`)

- [ ] **Step 1: 写失败测试**

在 `crates/core/src/model.rs` 末尾加测试模块(连同本 Task 的全部实现一起写在同文件;先写测试跑失败再补实现——下列代码块给出最终形态):

```rust
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
            capabilities: Capabilities { tools: true, vision: false, json_mode: true, audio_in: false },
            context_window: 100_000,
            max_output: Some(100_000),
            price: Price { in_per_mtok: 1.0, out_per_mtok: 2.0 },
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
        assert!(validate_model(&m).iter().any(|e| e.contains("context_window")));

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
```

`catalog.rs` 测试(同文件末尾):

```rust
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
        assert_eq!(gpt.price, Price { in_per_mtok: 2.5, out_per_mtok: 15.0 }); // ×1e6,无浮点噪声
        assert_eq!(gpt.context_window, 400000);
        assert_eq!(gpt.max_output, Some(128000));
        assert_eq!(gpt.vendor, "openai");
        assert!(gpt.capabilities.tools);
        assert!(!gpt.capabilities.json_mode); // 未写字段继承 defaults
        assert_eq!(c.version_hash.len(), 16);
        assert!(c.skipped.iter().any(|s| s.id == "text-embedding-3-small" && s.reason.contains("embedding")));
        assert!(c.skipped.iter().any(|s| s.id == "gpt-image-2" && s.reason.contains("image_generation")));
        assert!(c.skipped.iter().any(|s| s.id == "no-price-model" && s.reason.contains("缺价格")));
    }

    #[test]
    fn defaults_and_vendor_guess_for_unlisted() {
        let c = Catalog::from_strs(MODEL_LIST, ENRICH).unwrap();
        let ds = &c.models[0]; // deepseek-v4.1-flash:未富化
        assert_eq!(ds.vendor, "deepseek"); // 名称前缀推导
        assert_eq!(ds.context_window, 8000); // defaults
        assert!(ds.tags.is_empty());
        assert!(c.warnings.iter().any(|w| w.contains("deepseek-v4.1-flash") && w.contains("context_window")));
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
        assert!(c.skipped.iter().any(|s| s.id == "gpt-5.4" && s.reason.contains("disabled")));
        assert!(c.skipped.iter().any(|s| s.id == "deepseek-v4.1-flash" && s.reason.contains("embedding")));
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
}
```

- [ ] **Step 2: 跑测试确认失败**

```bash
cargo test -p recommender-core
```

期望:编译失败(model/catalog 尚未实现),证明测试确实在测目标代码。

- [ ] **Step 3: 实现 model.rs**

```rust
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
    if m.id.trim().is_empty() { errs.push("id 不能为空".into()); }
    if m.name.trim().is_empty() { errs.push(format!("{}: name 不能为空", m.id)); }
    if m.vendor.trim().is_empty() { errs.push(format!("{}: vendor 不能为空", m.id)); }
    if m.context_window == 0 { errs.push(format!("{}: context_window 必须 > 0", m.id)); }
    if let Some(mo) = m.max_output {
        if mo == 0 { errs.push(format!("{}: max_output 必须 > 0", m.id)); }
    }
    for (name, v) in [("in_per_mtok", m.price.in_per_mtok), ("out_per_mtok", m.price.out_per_mtok)] {
        if !v.is_finite() || v < 0.0 {
            errs.push(format!("{}: price.{name} 必须为非负有限数", m.id));
        }
    }
    errs
}
```

> 说明:`ModelEntry` 不实现 `Deserialize`——条目由 catalog.rs 从 model_list + enrichment 逐字段归一化构建;`Serialize` 保留(供 `/v1/catalog` 与 canonical hash)。`Capabilities` 实现 `Default`(作为 enrichment 字段级合并的基底);`Price`/`ModelStatus` 保留 `Deserialize`(在 enrichment 中出现)。`tags` 允许为空:`validate-catalog` 以警告提示未富化条目。

- [ ] **Step 4: 实现 catalog.rs**

```rust
//! 模型目录:LiteLLM model_list + enrichment 解析、归一化、校验、内容 hash(spec §4.3/§5.1)。

use crate::model::{validate_model, Capabilities, ModelEntry, ModelStatus, Price};
use serde::Deserialize;
use std::collections::BTreeMap;
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
    defaults: EnrichmentEntry,
    #[serde(default)]
    models: BTreeMap<String, EnrichmentEntry>,
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
    ("gpt", "openai"), ("o1", "openai"), ("o3", "openai"), ("o4", "openai"), ("chatgpt", "openai"),
    ("claude", "anthropic"),
    ("gemini", "google"), ("gemma", "google"),
    ("deepseek", "deepseek"),
    ("qwen", "alibaba"), ("qwq", "alibaba"),
    ("kimi", "moonshot"), ("moonshot", "moonshot"),
    ("glm", "zhipu"), ("chatglm", "zhipu"),
    ("phi", "microsoft"),
    ("llama", "meta"),
    ("mistral", "mistralai"), ("mixtral", "mistralai"), ("magistral", "mistralai"),
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
    pub fn from_strs(model_list_yaml: &str, enrichment_yaml: &str) -> Result<Catalog, CatalogError> {
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
            let id = e.model_name;
            let en = enrich.models.get(&id);

            // ① mode:enrichment > model_info.mode > 名称启发式(spec §5.1)
            let mode = en
                .and_then(|x| x.mode.clone())
                .or_else(|| e.model_info.mode.clone())
                .unwrap_or_else(|| guess_mode(&id).to_string());
            if mode != "chat" {
                skipped.push(SkippedModel { id, reason: format!("mode={mode}") });
                continue;
            }

            // ② status:disabled 直接排除
            if en.and_then(|x| x.status) == Some(ModelStatus::Disabled) {
                skipped.push(SkippedModel { id, reason: "status=disabled".into() });
                continue;
            }

            // ③ 价格:enrichment 覆盖 > model_list 换算 ×1e6
            let price = en.and_then(|x| x.price.clone()).or_else(|| {
                match (e.model_info.input_cost_per_token, e.model_info.output_cost_per_token) {
                    (Some(i), Some(o)) if i.is_finite() && o.is_finite() && i >= 0.0 && o >= 0.0 => {
                        Some(Price { in_per_mtok: round6(i * 1e6), out_per_mtok: round6(o * 1e6) })
                    }
                    _ => None,
                }
            });
            let Some(price) = price else {
                skipped.push(SkippedModel {
                    id,
                    reason: "缺价格(model_info 无 input/output_cost_per_token;可用 enrichment.price 覆盖)".into(),
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
                None => (defaults.context_window.unwrap_or(DEFAULT_CONTEXT_WINDOW), true),
            };
            if ctx_defaulted {
                warnings.push(format!("{id}: 未富化 context_window,使用默认值 {context_window}"));
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

            by_id.insert(id.clone(), ModelEntry {
                name: en.and_then(|x| x.name.clone()).unwrap_or_else(|| id.clone()),
                id,
                vendor,
                description: en.and_then(|x| x.description.clone()).unwrap_or_default(),
                tags: en.and_then(|x| x.tags.clone()).unwrap_or_default(),
                capabilities: caps,
                context_window,
                max_output: Some(en.and_then(|x| x.max_output).unwrap_or(context_window)),
                price,
                groups: en.and_then(|x| x.groups.clone()).unwrap_or_else(|| vec!["default".to_string()]),
                quality_notes: en.and_then(|x| x.quality_notes.clone()).unwrap_or_default(),
                status: ModelStatus::Enabled,
            });
        }

        // enrichment 里引用了不在 catalog 的模型 → 警告(陈旧富化清理提示)
        for key in enrich.models.keys() {
            if !by_id.contains_key(key) && !skipped.iter().any(|s| &s.id == key) {
                warnings.push(format!("enrichment 中的 {key} 不在 model_list(catalog 未收录)"));
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
    fn models(&self) -> &[ModelEntry] { &self.models }
    fn version(&self) -> &str { &self.version_hash }
}
```

> 说明:空 catalog(全部条目被排除)直接报错;`version_hash` 基于按 id 升序的模型数组 canonical JSON 计算,与 YAML 写法、条目顺序无关(仅内容敏感)。`warnings`/`skipped` 供 validate-catalog 与 `/debug` 展示。

- [ ] **Step 5: 写 `configs/model_list.yaml` 与 `configs/enrichment.yaml`(逐字)**

**为什么是这个格式**:`model_list` 与网关(LiteLLM 配置)同构——`catalog.model_list_path` 可直接指向网关真实文件(价格随网关同步);模型上下文/能力/标签等推荐元数据由 `enrichment.yaml` 富化,缺省走 `defaults`。本仓库内的示例为脱敏子集(不含 api_base/api_key/内网主机名)。

`configs/model_list.yaml`:

```yaml
# 模型清单:与网关 LiteLLM 配置同构;本文件为脱敏示例。
# 实操:把 server.yaml 的 catalog.model_list_path 指向网关真实配置(绝对路径即可),价格自动同步。
# 非 chat 条目(embedding/图片/视频/语音)会被自动排除;下列末尾 3 条保留以演示排除逻辑。
model_list:
  # --- OpenAI ---
  - model_name: gpt-5.4
    litellm_params: { model: azure/gpt-5.4 }
    model_info: { input_cost_per_token: 2.5e-06, output_cost_per_token: 1.5e-05 }
  - model_name: gpt-5.4                       # 第二部署:同名自动合并(价格一致,无警告)
    litellm_params: { model: azure/gpt-5.4 }
    model_info: { input_cost_per_token: 2.5e-06, output_cost_per_token: 1.5e-05 }
  - model_name: gpt-5.4-mini
    litellm_params: { model: azure/gpt-5.4-mini }
    model_info: { input_cost_per_token: 7.5e-07, output_cost_per_token: 4.5e-06 }
  - model_name: gpt-5.6-luna
    litellm_params: { model: azure/gpt-5.6-luna }
    model_info: { input_cost_per_token: 2e-07, output_cost_per_token: 1.2e-06 }
  - model_name: o4-mini
    litellm_params: { model: azure/o4-mini }
    model_info: { input_cost_per_token: 1.1e-06, output_cost_per_token: 4.4e-06 }
  # --- Google ---
  - model_name: gemini-3.1-pro
    litellm_params: { model: vertex_ai/gemini-3.1-pro-preview }
    model_info: { input_cost_per_token: 2e-06, output_cost_per_token: 1.2e-05 }
  - model_name: gemini-3.7-flash
    litellm_params: { model: vertex_ai/gemini-3.7-flash }
    model_info: { input_cost_per_token: 7.5e-07, output_cost_per_token: 3.75e-06 }
  # --- 其余厂商 ---
  - model_name: deepseek-v4.1-flash
    litellm_params: { model: openai/deepseek-v4.1-flash }
    model_info: { input_cost_per_token: 2.83e-07, output_cost_per_token: 1.131e-06 }
  - model_name: qwen3.7-max
    litellm_params: { model: openai/qwen3.7-max }
    model_info: { input_cost_per_token: 1.65e-06, output_cost_per_token: 4.951e-06 }
  - model_name: kimi-k3
    litellm_params: { model: moonshot/kimi-k3 }
    model_info: { input_cost_per_token: 2.827e-06, output_cost_per_token: 1.4133e-05 }
  - model_name: glm-5.2
    litellm_params: { model: openai/glm-5.2 }
    model_info: { input_cost_per_token: 1.1e-06, output_cost_per_token: 3.851e-06 }
  - model_name: private-ai/qwen3.5-35B-A3B-V2
    litellm_params: { model: openai/private-alias }   # 私有网关别名(已脱敏)
    model_info: { input_cost_per_token: 2.5e-06, output_cost_per_token: 1e-05 }
  # --- 非 chat(自动排除;仅为演示) ---
  - model_name: text-embedding-3-small
    litellm_params: { model: azure/text-embedding-3-small }
    model_info: { input_cost_per_token: 1e-07, output_cost_per_token: 0.0 }
  - model_name: gpt-image-2
    litellm_params: { model: azure/gpt-image-2 }
    model_info: { input_cost_per_token: 5e-06, output_cost_per_token: 4e-05, mode: image_generation }
  - model_name: whisper-large-v3-cantonese-v2
    litellm_params: { model: hosted_vllm/whisper }
    model_info: { input_cost_per_token: 0.0, output_cost_per_token: 0.0, mode: audio_transcription }
```

`configs/enrichment.yaml`:

```yaml
# 推荐元数据富化:补齐 model_list 没有的字段(vendor/tags/capabilities/context_window…;spec §5.1),全部可选。
# 未在此列出的模型整条走 defaults;validate-catalog 会列出所有依赖默认值的模型。
defaults:
  context_window: 32768        # 无信息时的保守默认
  capabilities: { tools: false, vision: false, json_mode: false, audio_in: false }
models:
  gpt-5.4:
    vendor: openai
    description: 旗舰通用模型;推理与代码强;支持工具与视觉。
    tags: [reasoning, code, tools, vision, long_context]
    capabilities: { tools: true, vision: true, json_mode: true }
    context_window: 400000
    max_output: 128000
  gpt-5.4-mini:
    vendor: openai
    description: 高性价比均衡款;日常任务与简单代码。
    tags: [chat, cheap, code, tools, vision]
    capabilities: { tools: true, vision: true, json_mode: true }
    context_window: 400000
  gpt-5.6-luna:
    vendor: openai
    description: 超低价轻量模型;适合大批量简单处理。
    tags: [chat, cheap, tools]
    capabilities: { tools: true, json_mode: true }
    context_window: 400000
  o4-mini:
    vendor: openai
    description: 紧凑推理模型;数学与逻辑题性价比高。
    tags: [reasoning, cheap, tools, vision]
    capabilities: { tools: true, vision: true, json_mode: true }
    context_window: 200000
  gemini-3.1-pro:
    vendor: google
    description: 超长上下文均衡旗舰;视觉与长文档强。
    tags: [long_context, vision, tools, reasoning, code]
    capabilities: { tools: true, vision: true, json_mode: true, audio_in: true }
    context_window: 1000000
    max_output: 65536
  gemini-3.7-flash:
    vendor: google
    description: 快速便宜、超长上下文;适合批量常规处理。
    tags: [chat, cheap, long_context, vision, tools]
    capabilities: { tools: true, vision: true, json_mode: true, audio_in: true }
    context_window: 1000000
  deepseek-v4.1-flash:
    vendor: deepseek
    description: 高性价比;中文强;推理与代码均衡。
    tags: [chat, cheap, tools, zh_strong]
    capabilities: { tools: true, json_mode: true }
    context_window: 128000
  qwen3.7-max:
    vendor: alibaba
    description: 中文与推理强;工具调用完善。
    tags: [chat, reasoning, zh_strong, tools]
    capabilities: { tools: true, json_mode: true }
    context_window: 256000
  kimi-k3:
    vendor: moonshot
    description: 强推理与长文;中文强。
    tags: [chat, reasoning, zh_strong, tools, long_context]
    capabilities: { tools: true, json_mode: true }
    context_window: 256000
  glm-5.2:
    vendor: zhipu
    description: 均衡通用;中文强;性价比好。
    tags: [chat, code, zh_strong, tools]
    capabilities: { tools: true, json_mode: true }
    context_window: 200000
  private-ai/qwen3.5-35B-A3B-V2:
    vendor: alibaba
    description: 私有部署 Qwen3.5 MoE;数据不出内网。
    tags: [chat, cheap, tools, zh_strong]
    capabilities: { tools: true, json_mode: true }
    context_window: 128000
```

> 校验锚点:`validate-catalog`(Task 19)对示例 catalog 期望输出「收录 11 / 排除 3(按原因)/ 依赖默认富化 0 / 警告 0」(11 = 同名合并后的唯一 chat 模型数)。

- [ ] **Step 6: 验证 + Commit**

```bash
cargo test -p recommender-core    # 期望:model/catalog 测试全过
cargo fmt --all
git add -A && git commit -m "feat(core): litellm model_list catalog with enrichment"
```

---

## Task 3: core::config(其余三份配置)+ configs/{rules,questions,server}.yaml

**Files:**
- Create: `crates/core/src/config.rs`、`configs/rules.yaml`、`configs/questions.yaml`、`configs/server.yaml`
- Modify: `crates/core/src/lib.rs`(加 `pub mod config;`)

- [ ] **Step 1: 写三份 YAML 配置文件**

- `configs/rules.yaml`:逐字复制 **spec §5.2 的 YAML 代码块**。
- `configs/questions.yaml`:逐字复制 **spec §5.3 的 YAML 代码块**(含全部 instructions/criteria/weights/confidence)。
- `configs/server.yaml`:逐字复制 **spec §5.4 的 YAML 代码块**(其中 `catalog:` 段保持默认 `model_list.yaml`/`enrichment.yaml`——两个文件就在 configs/ 目录内;相对路径基于 configs 目录解析,也可写绝对路径直达网关真实配置),并做两处追加:
  1. 顶层加一行 `templates_dir: templates`(理由模板目录,v1 扩展字段;加载器默认值即 `templates`;解析口径 = **CWD/仓库根相对** —— server 主程序、CLI dry-run 与 golden 测试三者一致)。
  2. 在 `backends:` 列表后追加注释,便于离线开发:

```yaml
# 离线开发可将下面这段取消注释,并把 kind: mock 的条目挪到列表首位(首个 backend 为默认):
#  - id: mock
#    kind: mock
```

- [ ] **Step 2: 写失败测试(config.rs 末尾)**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_repo_configs() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../configs");
        let cfg = load_configs(&dir).expect("仓库 configs 必须可加载");
        assert!(cfg.catalog.models.iter().any(|m| m.id == "gpt-5.4"));
        assert!(!cfg.catalog.skipped.is_empty(), "示例 model_list 应含被排除的非 chat 条目");
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
        let err = serde_yaml::from_str::<RulesConfig>(y).unwrap_err().to_string();
        assert!(err.contains("nope") || err.contains("unknown variant"), "got: {err}");
    }

    #[test]
    fn score_criteria_bounds_validated() {
        let mut q = QuestionsConfig::from_yaml(include_str!("../../../configs/questions.yaml")).unwrap();
        q.per_candidate.quality_fit.criteria = Some(serde_json::json!(["only-one"]));
        let errs = q.validate();
        assert!(errs.iter().any(|e| e.contains("quality_fit")), "got: {errs:?}");
    }

    #[test]
    fn adjustment_delta_parsing() {
        let q = QuestionsConfig::from_yaml(include_str!("../../../configs/questions.yaml")).unwrap();
        let w = &q.weights;
        assert_eq!(w.base.quality_fit, 0.75);
        let deltas: Vec<f32> = w.adjustments[0].set.values().map(|d| d.delta().unwrap()).collect();
        assert!(deltas.contains(&0.10) && deltas.contains(&-0.10));
    }
}
```

> 注:测试里 `include_str!("../../../configs/questions.yaml")` 相对 `crates/core/src/` 解析;若 cargo 报路径问题,以 `concat!(env!("CARGO_MANIFEST_DIR"), "/../../configs/questions.yaml")` + `std::fs::read_to_string` 替代。

- [ ] **Step 3: 跑测试确认失败**

```bash
cargo test -p recommender-core config    # 期望:编译失败(config 未实现)
```

- [ ] **Step 4: 实现 config.rs**

结构(全部 `#[serde(deny_unknown_fields)]`;`Default` 值按 spec §5.4 括号内缺省):

```rust
//! rules/questions/server 配置的 serde 模型与加载(spec §5);catalog 由 server.yaml 的
//! `catalog` 段路径装配(LiteLLM model_list + enrichment)。所有校验错误聚合返回,便于 validate-catalog 全量打印。

use crate::catalog::Catalog;
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, thiserror::Error)]
#[error("配置加载失败:\n{}", .0.join("\n"))]
pub struct ConfigErrors(pub Vec<String>);

pub struct LoadedConfigs {
    pub catalog: Catalog,
    pub rules: RulesConfig,
    pub questions: QuestionsConfig,
    pub server: ServerConfig,
}

fn read_file(dir: &Path, name: &str, errs: &mut Vec<String>) -> Option<String> {
    match std::fs::read_to_string(dir.join(name)) {
        Ok(s) => Some(s),
        Err(e) => { errs.push(format!("{name}: 读取失败: {e}")); None }
    }
}

pub fn load_configs(dir: &Path) -> Result<LoadedConfigs, ConfigErrors> {
    let mut errs = Vec::new();

    // ① server.yaml 先读:其 catalog 段决定 model_list / enrichment 的路径
    let server = read_file(dir, "server.yaml", &mut errs).and_then(|s| match ServerConfig::from_yaml(&s) {
        Ok(v) => { errs.extend(v.validate().into_iter().map(|e| format!("server.yaml: {e}"))); Some(v) }
        Err(e) => { errs.push(format!("server.yaml: {e}")); None }
    });

    // ② catalog:相对路径基于 configs 目录解析;绝对路径原样(可直达网关真实配置)
    let catalog = server.as_ref().and_then(|srv| {
        let ml = resolve_config_path(dir, &srv.catalog.model_list_path);
        let en = resolve_config_path(dir, &srv.catalog.enrichment_path);
        match Catalog::load(&ml, &en) {
            Ok(c) => Some(c),
            Err(e) => { errs.push(format!("catalog: {e}")); None }
        }
    });

    // ③ rules / questions(独立于 server.yaml)
    let rules = read_file(dir, "rules.yaml", &mut errs).and_then(|s| match RulesConfig::from_yaml(&s) {
        Ok(v) => { errs.extend(v.validate().into_iter().map(|e| format!("rules.yaml: {e}"))); Some(v) }
        Err(e) => { errs.push(format!("rules.yaml: {e}")); None }
    });
    let questions = read_file(dir, "questions.yaml", &mut errs).and_then(|s| match QuestionsConfig::from_yaml(&s) {
        Ok(v) => { errs.extend(v.validate().into_iter().map(|e| format!("questions.yaml: {e}"))); Some(v) }
        Err(e) => { errs.push(format!("questions.yaml: {e}")); None }
    });

    match (catalog, rules, questions, server) {
        (Some(catalog), Some(rules), Some(questions), Some(server)) if errs.is_empty() =>
            Ok(LoadedConfigs { catalog, rules, questions, server }),
        _ => Err(ConfigErrors(errs)),
    }
}

/// 相对路径基于 configs 目录;绝对路径原样(spec §5.4)。
fn resolve_config_path(dir: &Path, p: &str) -> std::path::PathBuf {
    let path = Path::new(p);
    if path.is_absolute() { path.to_path_buf() } else { dir.join(path) }
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

fn default_true() -> bool { true }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleKind { Capability, ContextLength, Budget, Vendor, TagRequire }

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

fn default_value_each() -> f64 { 0.05 }
impl Default for VendorPreference {
    fn default() -> Self { Self { white: vec![], value_each: 0.05 } }
}

impl RulesConfig {
    pub fn from_yaml(raw: &str) -> Result<Self, serde_yaml::Error> { serde_yaml::from_str(raw) }
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
pub enum QuestionType { Choice, Score, Noul }

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
pub struct PriceBands { pub cheap_max: f64, pub mid_max: f64 }

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
pub struct BaseWeights { pub quality_fit: f32, pub cost_fit: f32 }

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Clamp { pub min: f32, pub max: f32 }

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Adjustment {
    pub when: Conditions,
    pub set: std::collections::BTreeMap<String, DeltaValue>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Conditions {
    #[serde(default)] pub difficulty_at_least: Option<f32>,
    #[serde(default)] pub frontier_needed_above: Option<f32>,
    #[serde(default)] pub budget_present: Option<bool>,
    #[serde(default)] pub cost_sensitive: Option<bool>,
}

/// `"+0.10"` / `"-0.10"` 字符串或数字。
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(untagged)]
pub enum DeltaValue { Num(f32), Str(String) }

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
pub struct ConfidenceConfig { pub flag_below: f32 }

impl QuestionsConfig {
    pub fn from_yaml(raw: &str) -> Result<Self, serde_yaml::Error> { serde_yaml::from_str(raw) }
    pub fn validate(&self) -> Vec<String> {
        let mut errs = vec![];
        let check = |name: &str, q: &QuestionSpec, errs: &mut Vec<String>| {
            match (q.qtype, &q.criteria) {
                (QuestionType::Choice, Some(serde_json::Value::Object(_))) => {}
                (QuestionType::Choice, _) => errs.push(format!("{name}: choice 必须提供 map 形式的 criteria")),
                (QuestionType::Score, Some(serde_json::Value::Array(a))) if (2..=10).contains(&a.len()) => {}
                (QuestionType::Score, Some(serde_json::Value::Array(a))) =>
                    errs.push(format!("{name}: score criteria 档位数 {} 应在 2..=10", a.len())),
                (QuestionType::Score, _) => errs.push(format!("{name}: score 必须提供数组 criteria")),
                (QuestionType::Noul, _) => {}
            }
        };
        for (id, q) in &self.shared { check(&format!("shared.{id}"), q, &mut errs); }
        check("per_candidate.quality_fit", &self.per_candidate.quality_fit, &mut errs);
        check("per_candidate.cost_fit", &self.per_candidate.cost_fit, &mut errs);

        let pb = &self.price_bands;
        if !(pb.cheap_max.is_finite() && pb.mid_max.is_finite() && pb.cheap_max <= pb.mid_max) {
            errs.push("price_bands: 需满足 cheap_max ≤ mid_max 且为有限数".into());
        }
        let w = &self.weights;
        if w.base.quality_fit < 0.0 || w.base.cost_fit < 0.0 || w.base.quality_fit + w.base.cost_fit <= 0.0 {
            errs.push("weights.base: 需为非负且和 > 0".into());
        }
        if !(0.0..=1.0).contains(&w.clamp.min) || !(0.0..=1.0).contains(&w.clamp.max) || w.clamp.min > w.clamp.max {
            errs.push("weights.clamp: 需满足 0 ≤ min ≤ max ≤ 1".into());
        }
        if !(0.0..=1.0).contains(&self.confidence.flag_below) {
            errs.push("confidence.flag_below 应在 [0,1]".into());
        }
        for (i, adj) in w.adjustments.iter().enumerate() {
            let conds = [adj.when.difficulty_at_least.is_some(), adj.when.frontier_needed_above.is_some(),
                         adj.when.budget_present.is_some(), adj.when.cost_sensitive.is_some()];
            if !conds.iter().any(|x| *x) {
                errs.push(format!("weights.adjustments[{i}]: when 至少需要一个条件"));
            }
            for (k, v) in &adj.set {
                if !matches!(k.as_str(), "quality_fit" | "cost_fit") {
                    errs.push(format!("weights.adjustments[{i}]: set 键 `{k}` 非法(仅 quality_fit/cost_fit)"));
                }
                if let Err(e) = v.delta() {
                    errs.push(format!("weights.adjustments[{i}]: delta 解析失败 {e}"));
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

fn default_templates_dir() -> String { "templates".into() }

/// catalog 文件路径(spec §5.4)。相对路径基于 configs 目录;绝对路径直达网关真实配置。
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CatalogConfig {
    #[serde(default = "default_model_list_path")]
    pub model_list_path: String,
    #[serde(default = "default_enrichment_path")]
    pub enrichment_path: String,
}

fn default_model_list_path() -> String { "model_list.yaml".into() }
fn default_enrichment_path() -> String { "enrichment.yaml".into() }

impl Default for CatalogConfig {
    fn default() -> Self {
        Self { model_list_path: default_model_list_path(), enrichment_path: default_enrichment_path() }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerOpts {
    pub listen: String,
    #[serde(default)] pub client_auth_token_env: Option<String>,
    #[serde(default)] pub admin_token_env: Option<String>,
    #[serde(default)] pub metrics_enabled: bool,
    #[serde(default = "default_true")] pub debug_page_enabled: bool,
    #[serde(default = "default_max_body")] pub max_request_bytes: usize,
    #[serde(default = "default_request_timeout")] pub request_timeout_ms: u64,
}

fn default_max_body() -> usize { 4 * 1024 * 1024 }
fn default_request_timeout() -> u64 { 15_000 }

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BackendSpec {
    pub id: String,
    pub kind: BackendKind,
    #[serde(default)] pub base_url: Option<String>,
    #[serde(default)] pub model: Option<String>,
    #[serde(default)] pub api_key_env: Option<String>,
    #[serde(default)] pub proxy: Option<String>,
    #[serde(default = "default_timeout_ms")] pub timeout_ms: u64,
    #[serde(default = "default_total_budget_ms")] pub total_budget_ms: u64,
    #[serde(default = "default_max_inflight")] pub max_inflight: usize,
    #[serde(default)] pub retry: RetrySpec,
    #[serde(default)] pub long_context_header: bool,
}

fn default_timeout_ms() -> u64 { 5000 }
fn default_total_budget_ms() -> u64 { 8000 }
fn default_max_inflight() -> usize { 16 }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackendKind { Systemone, Mock }

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetrySpec {
    #[serde(default = "default_max_attempts")] pub max_attempts: u32,
    #[serde(default = "default_base_delay")] pub base_delay_ms: u64,
    #[serde(default = "default_true")] pub honor_retry_after: bool,
}
fn default_max_attempts() -> u32 { 2 }
fn default_base_delay() -> u64 { 250 }
impl Default for RetrySpec {
    fn default() -> Self { Self { max_attempts: 2, base_delay_ms: 250, honor_retry_after: true } }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CacheConfig {
    #[serde(default = "default_true")] pub enabled: bool,
    #[serde(default = "default_ttl")] pub ttl_secs: u64,
    #[serde(default = "default_cache_entries")] pub max_entries: u64,
}
fn default_ttl() -> u64 { 600 }
fn default_cache_entries() -> u64 { 2048 }
impl Default for CacheConfig {
    fn default() -> Self { Self { enabled: true, ttl_secs: 600, max_entries: 2048 } }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LimitsConfig {
    #[serde(default = "default_max_in_call")] pub max_candidates_in_call: usize,
    #[serde(default = "default_max_judged")] pub max_candidates_judged: usize,
    #[serde(default = "default_top_k")] pub default_top_k: u32,
}
fn default_max_in_call() -> usize { 30 }
fn default_max_judged() -> usize { 60 }
fn default_top_k() -> u32 { 5 }
impl Default for LimitsConfig {
    fn default() -> Self {
        Self { max_candidates_in_call: 30, max_candidates_judged: 60, default_top_k: 5 }
    }
}

impl ServerConfig {
    pub fn from_yaml(raw: &str) -> Result<Self, serde_yaml::Error> { serde_yaml::from_str(raw) }
    pub fn validate(&self) -> Vec<String> {
        let mut errs = vec![];
        for (i, b) in self.backends.iter().enumerate() {
            if b.kind == BackendKind::Systemone {
                if b.base_url.as_deref().unwrap_or("").trim().is_empty() {
                    errs.push(format!("backends[{i}] ({}): kind=systemone 需要 base_url", b.id));
                }
                if b.model.as_deref().unwrap_or("").trim().is_empty() {
                    errs.push(format!("backends[{i}] ({}): kind=systemone 需要 model", b.id));
                }
            }
        }
        let mut seen = std::collections::HashSet::new();
        for b in &self.backends {
            if !seen.insert(b.id.clone()) { errs.push(format!("backends: duplicate id {}", b.id)); }
        }
        let l = &self.limits;
        if l.max_candidates_in_call == 0 || l.max_candidates_judged < l.max_candidates_in_call {
            errs.push("limits: 需满足 0 < max_candidates_in_call ≤ max_candidates_judged".into());
        }
        if l.default_top_k == 0 { errs.push("limits.default_top_k 必须 > 0".into()); }
        errs
    }
}
```

- [ ] **Step 5: 验证 + Commit**

```bash
cargo test -p recommender-core    # 期望:全部通过(含新 config 测试)
cargo fmt --all
git add -A && git commit -m "feat(core): load and validate rules/questions/server configs"
```

---

## Task 4: core::api(HTTP DTO)

**Files:**
- Create: `crates/core/src/api.rs`
- Modify: `crates/core/src/lib.rs`(加 `pub mod api;`)

- [ ] **Step 1: 写失败测试(api.rs 末尾)**

```rust
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
        let mut r: RecommendRequest = serde_json::from_str(r#"{"request":{},"options":{"top_k":0}}"#).unwrap();
        assert!(r.validate().unwrap_err().contains("top_k"));
        r.options.top_k = None;
        r.constraints.budget = Some(BudgetConstraint { max_input_price_per_mtok: Some(-1.0), cost_sensitive: false });
        assert!(r.validate().unwrap_err().contains("max_input_price_per_mtok"));
    }

    #[test]
    fn response_json_matches_spec_field_names() {
        // 与 spec §8.1 响应示例逐字段对齐(存在性检查)
        let resp = RecommendResponse {
            recommendations: vec![Recommendation {
                model: "gpt-5.4".into(), rank: 1, score: 0.773, confidence: Some(0.61),
                low_confidence: false,
                breakdown: Breakdown {
                    quality_fit: Some(0.92), cost_fit: Some(0.50),
                    weights: Some(WeightsOut { quality_fit: 0.65, cost_fit: 0.35 }),
                    penalty: Some(0.0),
                    rule_hits: vec!["capability:tools✓".into()],
                    coarse: None,
                },
                reasons: vec!["✅ …".into()],
            }],
            profile: ProfileView { task_type: Some("code_debug".into()), difficulty: Some(1.2),
                frontier_needed: Some(0.3), est_tokens: 8200, has_images: false, tools_count: 3,
                json_mode: false, locale_hint: Some("zh".into()), task_hints: vec![] },
            degraded: false,
            debug: None,
            meta: Meta { backend: "jev-1.13.0".into(), latency_ms: 412, cache_hit: false,
                catalog_version: "a3f9".into(), weights_version: "b21c".into(),
                usage: Some(UsageOut { input_tokens: 5210, output_tokens: 12 }),
                filtered: None, degraded_reason: None, partial_answers: None,
                request_id: "req-1".into() },
        };
        let v = serde_json::to_value(&resp).unwrap();
        for key in ["recommendations", "profile", "degraded", "meta"] {
            assert!(v.get(key).is_some(), "missing {key}");
        }
        let rec = &v["recommendations"][0];
        for key in ["model", "rank", "score", "confidence", "low_confidence", "breakdown", "reasons"] {
            assert!(rec.get(key).is_some(), "missing recommendations[0].{key}");
        }
        let meta = &v["meta"];
        for key in ["backend", "latency_ms", "cache_hit", "catalog_version", "weights_version", "usage"] {
            assert!(meta.get(key).is_some(), "missing meta.{key}");
        }
        // include_debug=false 时 debug 字段不出现
        assert!(v.get("debug").is_none());
    }

    #[test]
    fn error_codes_frozen() {
        assert_eq!(ApiErrorCode::InvalidJson.as_str(), "invalid_json");
        assert_eq!(ApiErrorCode::RequestTooLarge.as_str(), "request_too_large");
        assert_eq!(ApiErrorCode::InvalidRequestBody.as_str(), "invalid_request_body");
        assert_eq!(ApiErrorCode::InvalidConstraints.as_str(), "invalid_constraints");
        assert_eq!(ApiErrorCode::UpstreamError.as_str(), "upstream_error");
        assert_eq!(ApiErrorCode::RequestTimeout.as_str(), "request_timeout");
        assert_eq!(ApiErrorCode::InternalError.as_str(), "internal_error");
    }
}
```

- [ ] **Step 2: 跑测试确认失败**

```bash
cargo test -p recommender-core api    # 期望:编译失败(api 未实现)
```

- [ ] **Step 3: 实现 api.rs**

要点(逐字段类型见下;serde 属性严格按 spec §8.1 字段名):

```rust
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
                    return Err("constraints.budget.max_input_price_per_mtok 必须为非负有限数".into());
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
    #[serde(default)] pub budget: Option<BudgetConstraint>,
    #[serde(default)] pub require_tags: Vec<String>,
    #[serde(default)] pub vendor_allow: Vec<String>,
    #[serde(default)] pub vendor_deny: Vec<String>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BudgetConstraint {
    #[serde(default)] pub max_input_price_per_mtok: Option<f64>,
    #[serde(default)] pub cost_sensitive: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Options {
    #[serde(default)] pub top_k: Option<u32>,
    #[serde(default)] pub locale: Locale,
    #[serde(default)] pub include_debug: bool,
}
impl Default for Options {
    fn default() -> Self { Self { top_k: None, locale: Locale::Zh, include_debug: false } }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Locale { #[default] Zh, En }
impl Locale { pub fn as_str(&self) -> &'static str { match self { Locale::Zh => "zh", Locale::En => "en" } } }

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
pub struct WeightsOut { pub quality_fit: f32, pub cost_fit: f32 }

#[derive(Debug, Clone, Copy, Serialize)]
pub struct CoarseOut { pub tag_overlap: f32, pub price_score: f32 }

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
    #[serde(skip_serializing_if = "Option::is_none")] pub filtered: Option<Vec<FilteredByRule>>,
    #[serde(skip_serializing_if = "Option::is_none")] pub degraded_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")] pub partial_answers: Option<Vec<String>>,
    pub request_id: String,
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct UsageOut { pub input_tokens: u64, pub output_tokens: u64 }

#[derive(Debug, Clone, Serialize)]
pub struct FilteredByRule { pub rule: String, pub count: u32 }

// ---- 错误码(spec §8.2)----

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApiErrorCode {
    InvalidJson, RequestTooLarge, InvalidRequestBody, InvalidConstraints,
    UpstreamError, RequestTimeout, InternalError,
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
```

> 注:`Recommendation.score` 用 f32,输出前统一 `(x*1000.0).round()/1000.0` 到 3 位小数(在 pipeline 完成)。

- [ ] **Step 4: 验证 + Commit**

```bash
cargo test -p recommender-core    # 期望:全过
cargo fmt --all
git add -A && git commit -m "feat(core): http dtos and error codes per api contract"
```

---

## Task 5: core::profile(ProfileExtractor)

**Files:**
- Create: `crates/core/src/profile.rs`
- Modify: `crates/core/src/lib.rs`(加 `pub mod profile;`)

- [ ] **Step 1: 写失败测试(profile.rs 末尾,随实现一并落盘;先跑 Step 2 看到编译失败)**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn openai_basic() {
        let p = Profile::from_value(&json!({
            "messages": [{"role":"user","content":"hello"}]
        })).unwrap();
        assert_eq!(p.est_tokens, 6); // "hello\n" 6 ascii /3.5 + 4×1 消息 → ceil ≈ 6
        assert!(!p.has_images && !p.json_mode && !p.stream);
        assert_eq!(p.tools_count, 0);
        assert_eq!(p.locale_hint.as_deref(), Some("other"));
        assert_eq!(p.request_excerpt, "hello\n");
    }

    #[test]
    fn claude_system_and_content_blocks() {
        let p = Profile::from_value(&json!({
            "system": "你是助手",
            "messages": [{"role":"user","content":[
                {"type":"text","text":"看这张图"},
                {"type":"image","source":{"data":"x"}}
            ]}],
            "max_tokens": 1024
        })).unwrap();
        assert!(p.has_images);
        assert_eq!(p.max_tokens, Some(1024));
        assert!(p.request_excerpt.contains("你是助手"));
        assert!(p.request_excerpt.contains("看这张图"));
        assert_eq!(p.locale_hint.as_deref(), Some("zh")); // CJK 占比 > 20%
    }

    #[test]
    fn openai_flags() {
        let p = Profile::from_value(&json!({
            "messages": [{"role":"user","content":"hi"}],
            "tools": [{"a":1},{"b":2}],
            "response_format": {"type":"json_schema"},
            "stream": true,
            "max_completion_tokens": 2048
        })).unwrap();
        assert_eq!(p.tools_count, 2);
        assert!(p.json_mode && p.stream);
        assert_eq!(p.max_tokens, Some(2048));
    }

    #[test]
    fn wrap_non_chat_json_and_raw_text() {
        let p = Profile::from_value(&json!({"foo": 1})).unwrap();
        assert!(p.request_excerpt.contains("foo"));
        let p2 = Profile::from_text("这不是 JSON{").unwrap();
        assert_eq!(p2.request_excerpt, "这不是 JSON{");
        assert!(p2.est_tokens > 0);
    }

    #[test]
    fn empty_inputs_rejected() {
        assert_eq!(Profile::from_text("   ").unwrap_err(), ProfileError::Empty);
        assert!(Profile::from_value(&json!("")).is_err());
        assert!(Profile::from_value(&json!({})).is_err());
        assert!(Profile::from_value(&json!([])).is_err());
        assert!(Profile::from_value(&json!(null)).is_err());
    }

    #[test]
    fn excerpt_truncation_and_cleaning() {
        let long = "a".repeat(3000);
        let p = Profile::from_text(&long).unwrap();
        let n = p.request_excerpt.chars().count();
        assert_eq!(n, 2000 + 6 + 500); // head + "…[截断]…" + tail
        assert!(p.request_excerpt.contains("[截断]"));

        let p2 = Profile::from_value(&json!({
            "messages": [{"role":"user","content":"a\u{0000}b\nc\tx"}]
        })).unwrap();
        assert_eq!(p2.request_excerpt, "ab\nc\tx\n"); // 控制字符去除,\n \t 保留
    }

    #[test]
    fn hints_are_chinese_strings() {
        let p = Profile::from_text("```rust\ncode\n```\nError: boom\nhttps://x.com").unwrap();
        assert!(p.task_hints.contains(&"包含代码块".to_string()));
        assert!(p.task_hints.contains(&"提到报错堆栈".to_string()));
        assert!(p.task_hints.contains(&"包含链接".to_string()));
    }

    #[test]
    fn cjk_ranges() {
        assert!(is_cjk('你') && is_cjk('あ') && is_cjk('한'));
        assert!(!is_cjk('a') && !is_cjk('é'));
    }
}
```

- [ ] **Step 2: 跑测试确认失败**

```bash
cargo test -p recommender-core profile   # 期望:编译失败(profile 未实现)
```

- [ ] **Step 3: 实现 profile.rs**

```rust
//! 请求画像提取(spec §6.1)。容错: 非 chat JSON → 包一层;空输入 → 422 级错误。

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const EXCERPT_HEAD: usize = 2000;
pub const EXCERPT_TAIL: usize = 500;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Profile {
    pub est_tokens: u32,
    pub text_chars: usize,
    pub has_images: bool,
    pub tools_count: u32,
    pub json_mode: bool,
    pub stream: bool,
    pub max_tokens: Option<u32>,
    pub locale_hint: Option<String>, // "zh" | "other";空文本为 None
    pub task_hints: Vec<String>,
    pub request_excerpt: String,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProfileError {
    #[error("请求体为空,无法解析为 OpenAI/Claude 格式")]
    Empty,
}

impl Profile {
    /// 服务端入口:内层 request 字段(信封保证是合法 JSON Value)。
    pub fn from_value(body: &Value) -> Result<Profile, ProfileError> {
        let mut text = String::new();
        let mut message_count = 0usize;
        let mut has_images = false;

        let is_chat = body.get("messages").and_then(Value::as_array).is_some();
        if is_chat {
            if let Some(sys) = body.get("system") {
                collect_blocks(sys, &mut text, &mut has_images);
            }
            if let Some(msgs) = body["messages"].as_array() {
                message_count = msgs.len();
                for m in msgs {
                    if let Some(c) = m.get("content") {
                        collect_blocks(c, &mut text, &mut has_images);
                    }
                }
            }
            let tools_count = body.get("tools").and_then(Value::as_array).map(|a| a.len() as u32).unwrap_or(0);
            let json_mode = matches!(
                body.get("response_format").and_then(|r| r.get("type")).and_then(Value::as_str),
                Some("json_object") | Some("json_schema")
            );
            let stream = body.get("stream").and_then(Value::as_bool).unwrap_or(false);
            let max_tokens = body.get("max_tokens")
                .or_else(|| body.get("max_completion_tokens"))
                .and_then(Value::as_u64)
                .map(|v| v.min(u32::MAX as u64) as u32);
            return Ok(Self::assemble(text, message_count, has_images, tools_count, json_mode, stream, max_tokens));
        }

        // 非 chat:字符串直接当正文;空/null/空容器 → Empty;其余 JSON → 序列化文本包一层。
        if let Some(s) = body.as_str() {
            if s.trim().is_empty() { return Err(ProfileError::Empty); }
            return Ok(Self::assemble_one(s.to_string()));
        }
        let empty_container = body.is_null()
            || body.as_object().map(|o| o.is_empty()).unwrap_or(false)
            || body.as_array().map(|a| a.is_empty()).unwrap_or(false);
        if empty_container { return Err(ProfileError::Empty); }
        Ok(Self::assemble_one(body.to_string()))
    }

    /// CLI/测试入口:任意文本(可能根本不是 JSON)。
    pub fn from_text(raw: &str) -> Result<Profile, ProfileError> {
        match serde_json::from_str::<Value>(raw) {
            Ok(v) => Profile::from_value(&v),
            Err(_) => {
                if raw.trim().is_empty() { return Err(ProfileError::Empty); }
                Ok(Self::assemble_one(raw.to_string()))
            }
        }
    }

    fn assemble_one(text: String) -> Profile {
        Self::assemble(text, 1, false, 0, false, false, None)
    }

    fn assemble(text: String, message_count: usize, has_images: bool, tools_count: u32,
                json_mode: bool, stream: bool, max_tokens: Option<u32>) -> Profile {
        Profile {
            est_tokens: estimate_tokens(&text, message_count),
            text_chars: text.chars().count(),
            has_images, tools_count, json_mode, stream, max_tokens,
            locale_hint: locale_hint(&text),
            task_hints: task_hints(&text),
            request_excerpt: make_excerpt(&text),
        }
    }
}

fn collect_blocks(content: &Value, text: &mut String, images: &mut bool) {
    match content {
        Value::String(s) => { text.push_str(s); text.push('\n'); }
        Value::Array(items) => {
            for it in items {
                match it.get("type").and_then(Value::as_str) {
                    Some("text") | Some("input_text") | Some("output_text") => {
                        if let Some(t) = it.get("text").and_then(Value::as_str) {
                            text.push_str(t); text.push('\n');
                        }
                    }
                    Some("image_url") | Some("image") | Some("input_image") => *images = true,
                    _ => {}
                }
            }
        }
        _ => {}
    }
}

/// spec §5.2: ASCII 字符/3.5 + CJK 字符/1.5 + 每消息 4 token;向上取整(保守)。
pub fn estimate_tokens(text: &str, message_count: usize) -> u32 {
    let (mut ascii, mut cjk) = (0u64, 0u64);
    for c in text.chars() {
        if is_cjk(c) { cjk += 1; } else { ascii += 1; }
    }
    let t = ascii as f64 / 3.5 + cjk as f64 / 1.5 + 4.0 * message_count as f64;
    t.ceil() as u32
}

pub fn is_cjk(c: char) -> bool {
    matches!(c as u32,
        0x3000..=0x303F | 0x3040..=0x30FF | 0x3400..=0x4DBF
        | 0x4E00..=0x9FFF | 0xAC00..=0xD7A3 | 0xF900..=0xFAFF | 0xFF00..=0xFFEF)
}

/// 清洗(去控制字符,\n \t 保留)+ 头 2000/尾 500 截断(spec §6.1)。
pub fn make_excerpt(text: &str) -> String {
    let cleaned: String = text.chars()
        .filter(|c| !c.is_control() || matches!(c, '\n' | '\t'))
        .collect();
    let chars: Vec<char> = cleaned.chars().collect();
    if chars.len() <= EXCERPT_HEAD + EXCERPT_TAIL { return cleaned; }
    let head: String = chars[..EXCERPT_HEAD].iter().collect();
    let tail: String = chars[chars.len() - EXCERPT_TAIL..].iter().collect();
    format!("{head}…[截断]…{tail}")
}

fn locale_hint(text: &str) -> Option<String> {
    let total = text.chars().count();
    if total == 0 { return None; }
    let cjk = text.chars().filter(|c| is_cjk(*c)).count();
    Some(if cjk as f64 / total as f64 > 0.2 { "zh".into() } else { "other".into() })
}

/// spec §6.1 固定检测表 v1 三条;命中才加入。
fn task_hints(text: &str) -> Vec<String> {
    let mut v = vec![];
    if text.contains("```") { v.push("包含代码块".to_string()); }
    let lower = text.to_lowercase();
    if ["error", "exception", "traceback", "异常", "堆栈"].iter().any(|k| lower.contains(k)) {
        v.push("提到报错堆栈".to_string());
    }
    if text.contains("://") || lower.contains("www.") { v.push("包含链接".to_string()); }
    v
}
```

- [ ] **Step 4: 验证 + Commit**

```bash
cargo test -p recommender-core    # 期望:全过
cargo fmt --all
git add -A && git commit -m "feat(core): tolerant profile extractor for openai/claude bodies"
```

---

## Task 6: core::rules(引擎 + 5 条内置规则)

**Files:**
- Create: `crates/core/src/rules/mod.rs`、`crates/core/src/rules/builtin.rs`
- Modify: `crates/core/src/lib.rs`(加 `pub mod rules;`)

- [ ] **Step 1~3: 测试 + 实现(同文件落盘;先跑 `cargo test -p recommender-core rules` 看编译失败)**

`rules/mod.rs`:

```rust
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
    Filter { reason_key: String, params: BTreeMap<String, String> },
    Penalty { value: f32, reason_key: String, params: BTreeMap<String, String> },
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
        if !spec.enabled { continue; }
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
            model: m.clone(), penalties: 0.0, penalty_records: vec![], rule_hits: vec![],
        };
        for r in rules {
            match r.eval(ctx, m) {
                RuleOutcome::Pass => candidate.rule_hits.push(format!("{}✓", r.name())),
                RuleOutcome::Filter { reason_key, .. } => {
                    filtered = Some((r.name().to_string(), reason_key));
                    break;
                }
                RuleOutcome::Penalty { value, reason_key, params } => {
                    candidate.penalties += value;
                    candidate.penalty_records.push(PenaltyRecord {
                        rule: r.name().to_string(), value, reason_key, params,
                    });
                    candidate.rule_hits.push(format!("{}:penalty", r.name()));
                }
            }
        }
        match filtered {
            Some((rule, reason_key)) => report.filtered_by_rule.push(FilteredRecord {
                rule, reason_key, model_id: m.id.clone(),
            }),
            None => report.candidates.push(candidate),
        }
    }
    report
}
```

`rules/builtin.rs`(5 条规则,语义逐条按 spec §5.2;完整代码):

```rust
//! 内置规则。全部确定性、无网络。

use super::{Rule, RuleCtx, RuleOutcome};
use crate::model::ModelEntry;
use std::collections::BTreeMap;

fn filter(reason_key: &str, params: impl IntoIterator<Item = (&'static str, String)>) -> RuleOutcome {
    RuleOutcome::Filter { reason_key: reason_key.into(), params: params.into_iter().map(|(k, v)| (k.to_string(), v)).collect() }
}

pub struct CapabilityRule;
impl Rule for CapabilityRule {
    fn name(&self) -> &str { "capability" }
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

pub struct ContextLengthRule { pub safety_factor: f64 }
impl Rule for ContextLengthRule {
    fn name(&self) -> &str { "context_length" }
    fn eval(&self, ctx: &RuleCtx, m: &ModelEntry) -> RuleOutcome {
        let needed = (ctx.profile.est_tokens as f64 * self.safety_factor).ceil() as u64
            + ctx.profile.max_tokens.unwrap_or(0) as u64;
        if needed > m.context_window {
            filter("context_overflow", [
                ("needed", needed.to_string()),
                ("window", m.context_window.to_string()),
            ])
        } else {
            RuleOutcome::Pass
        }
    }
}

pub struct BudgetRule;
impl Rule for BudgetRule {
    fn name(&self) -> &str { "budget" }
    fn eval(&self, ctx: &RuleCtx, m: &ModelEntry) -> RuleOutcome {
        if let Some(b) = &ctx.constraints.budget {
            if let Some(maxp) = b.max_input_price_per_mtok {
                if m.price.in_per_mtok > maxp {
                    return filter("budget_exceeded", [
                        ("price", m.price.in_per_mtok.to_string()),
                        ("max", maxp.to_string()),
                    ]);
                }
            }
        }
        RuleOutcome::Pass
    }
}

pub struct VendorRule;
impl Rule for VendorRule {
    fn name(&self) -> &str { "vendor" }
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
    fn name(&self) -> &str { "tag_require" }
    fn eval(&self, ctx: &RuleCtx, m: &ModelEntry) -> RuleOutcome {
        if let Some(missing) = ctx.constraints.require_tags.iter().find(|t| !m.tags.iter().any(|x| x == *t)) {
            return filter("tag_missing", [("tag", missing.clone())]);
        }
        RuleOutcome::Pass
    }
}
```

测试要点(`rules/mod.rs` 末尾测试模块,自己写具体断言):
1. 每条内置规则的真值表各 ≥3 例(命中/不命中/无约束):
   - capability: tools/vision/json_mode 三种过滤 + 全满足通过 + stream 不产生约束;
   - context_length: `est=1000, safety=1.15, max_tokens=Some(100), window=1250` → 过滤(1150+100>1250);window=1251 → 通过;max_tokens=None 视为 0;
   - budget: 超限过滤 / 无预算通过 / 恰好等于上限通过(> 才算超);
   - vendor: deny 命中、allow 拦截、allow 空放行;
   - tag_require: 缺一即滤、全含通过。
2. 引擎聚合:构造一个自定义 `PenaltyRule`(返回 `Penalty{0.1}`)+ 一条过滤规则,断言:被滤模型进 `filtered_by_rule` 且带规则名与 reason_key;通过模型的 `penalties == 0.1`、`penalty_records.len()==1`、`rule_hits` 含 `"<name>✓"` 与 `"<name>:penalty"`;候选顺序 = 目录顺序。
3. `build_rules`:从 spec 的 rules.yaml(5 条)构建 → len 5;把某条 `enabled: false` → len 4。
4. ctx 用 `Profile::assemble_one` 不可直接构造 → 用 `Profile::from_text("hi").unwrap()` + 手写 `Constraints::default()`/`Options::default()` 组合。

- [ ] **Step 4: 验证 + Commit**

```bash
cargo test -p recommender-core    # 期望:全过
cargo fmt --all
git add -A && git commit -m "feat(core): hard rule engine with five builtin rules"
```

---

## Task 7: core::coarse(CoarseRank)

**Files:**
- Create: `crates/core/src/coarse.rs`
- Modify: `crates/core/src/lib.rs`(加 `pub mod coarse;`)

- [ ] **Step 1~4: 实现(含测试)**

```rust
//! CoarseRank(spec §6.2): >60 截断与降级排序;确定性。

use crate::model::ModelEntry;
use crate::profile::Profile;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CoarseScore {
    pub tag_overlap: f32,
    pub price_score: f32,
}

impl CoarseScore {
    pub fn total(&self) -> f32 { 0.5 * self.tag_overlap + 0.5 * self.price_score }
}

/// 画像推导的 needs 标签集(spec §6.2)。
pub fn needs(p: &Profile) -> Vec<&'static str> {
    let mut n = vec![];
    if p.has_images { n.push("vision"); }
    if p.tools_count > 0 { n.push("tools"); }
    if p.json_mode { n.push("json_mode"); }
    if p.est_tokens > 32_000 { n.push("long_context"); }
    n
}

pub fn tag_overlap(needs: &[&str], tags: &[String]) -> f32 {
    if needs.is_empty() { return 1.0; }
    let hit = needs.iter().filter(|n| tags.iter().any(|t| t == *n)).count();
    hit as f32 / needs.len() as f32
}

/// 输入单价升序名次 → 分位:最便宜 1.0,最贵 0.0;N=1 → 1.0。等价格保持稳定序。
pub fn price_percentiles(models: &[ModelEntry]) -> Vec<f32> {
    let n = models.len();
    if n <= 1 { return vec![1.0; n]; }
    let mut idx: Vec<usize> = (0..n).collect();
    idx.sort_by(|&a, &b| models[a].price.in_per_mtok
        .partial_cmp(&models[b].price.in_per_mtok)
        .unwrap_or(std::cmp::Ordering::Equal));
    let mut out = vec![0.0f32; n];
    for (rank, &i) in idx.iter().enumerate() {
        out[i] = 1.0 - rank as f32 / (n - 1) as f32;
    }
    out
}

/// 降序排好序的 (模型, 粗排分)。tie-break: 输入价格升序 → id 字典序
/// (采用 spec §6.4 的融合链;§6.2 行文仅写“id 字典序”,此处一致地细化到价格优先)。
pub fn coarse_rank<'a>(profile: &Profile, models: &'a [ModelEntry]) -> Vec<(&'a ModelEntry, CoarseScore)> {
    let needs = needs(profile);
    let pct = price_percentiles(models);
    let mut scored: Vec<(&ModelEntry, CoarseScore)> = models.iter().enumerate()
        .map(|(i, m)| (m, CoarseScore { tag_overlap: tag_overlap(&needs, &m.tags), price_score: pct[i] }))
        .collect();
    scored.sort_by(|a, b| {
        b.1.total().partial_cmp(&a.1.total()).unwrap_or(std::cmp::Ordering::Equal)
            .then(a.0.price.in_per_mtok.partial_cmp(&b.0.price.in_per_mtok).unwrap_or(std::cmp::Ordering::Equal))
            .then(a.0.id.cmp(&b.0.id))
    });
    scored
}

#[cfg(test)]
mod tests {
    // 测试要点:
    // 1. needs: 全空 → [];est_tokens=32001 → 含 long_context;32000 → 不含(> 32k 才算)。
    // 2. tag_overlap: needs 空 → 1.0;命中一半 → 0.5;全不中 → 0.0。
    // 3. price_percentiles: 3 个价格 [1,2,3] → [1.0,0.5,0.0];N=1 → [1.0]。
    // 4. coarse_rank: 构造 3 个模型(同 tag、不同价)断言顺序按 total;价格相同且重叠相同 → 按 id。
    //    构造工具函数 make_model(id, price, tags) 复用于本模块。
}
```

- [ ] **Step 5: 验证 + Commit**

```bash
cargo test -p recommender-core    # 期望:全过
cargo fmt --all
git add -A && git commit -m "feat(core): deterministic coarse rank for truncation and degrade"
```

---

## Task 8: core::judgment(契约类型 + 容错解码)

**Files:**
- Create: `crates/core/src/judgment.rs`
- Modify: `crates/core/src/lib.rs`(加 `pub mod judgment;`)、`crates/core/src/config.rs`(给 `QuestionType` 补 `Serialize` derive)

- [ ] **Step 1~3: 实现(含测试)**

```rust
//! /v1/systemone 契约类型(spec §7)、JudgmentBackend trait(spec §4.3)、响应容错解码(spec §8.2)。

use crate::config::QuestionType;
use futures::future::BoxFuture;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JudgeRequest {
    /// 发送时由客户端覆盖为配置的钉死版本 ID(pipeline 构造时不填)。
    #[serde(default)]
    pub model: String,
    pub state: serde_json::Value,
    pub questions: BTreeMap<String, Question>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Question {
    #[serde(rename = "type")]
    pub qtype: QuestionType,
    pub instructions: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub criteria: Option<serde_json::Value>,
}

impl From<&crate::config::QuestionSpec> for Question {
    fn from(s: &crate::config::QuestionSpec) -> Self {
        Question { qtype: s.qtype, instructions: s.instructions.clone(), criteria: s.criteria.clone() }
    }
}

// 响应:宽松解析(不 deny_unknown_fields,服务端可加字段);answers 先收 Value,再逐个容错解码。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JudgeResponse {
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub answers: BTreeMap<String, serde_json::Value>,
    #[serde(default)]
    pub usage: Option<WireUsage>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct WireUsage {
    #[serde(default)] pub input_tokens: u64,
    #[serde(default)] pub output_tokens: u64,
}

#[derive(Debug, Clone, thiserror::Error)]
pub enum JudgeError {
    #[error("后端 API key 未配置")]
    MissingKey,
    #[error("后端 HTTP {status}: {body}")]
    Http { status: u16, body: String },
    #[error("后端超时")]
    Timeout,
    #[error("后端连接失败: {0}")]
    Connect(String),
    #[error("后端响应解析失败: {0}")]
    Decode(String),
}

impl JudgeError {
    /// 降级原因(spec §8.2):4xx(非 429) → backend_4xx;429 → backend_429;其余 → backend_unavailable。
    pub fn degraded_reason(&self) -> &'static str {
        match self {
            JudgeError::Http { status, .. } if (400..500).contains(status) && *status != 429 => "backend_4xx",
            JudgeError::Http { status: 429, .. } => "backend_429",
            _ => "backend_unavailable",
        }
    }
}

pub trait JudgmentBackend: Send + Sync {
    fn id(&self) -> &str;
    fn judge<'a>(&'a self, req: JudgeRequest) -> BoxFuture<'a, Result<JudgeResponse, JudgeError>>;
}

// ---- 容错解码(spec §8.2“缺答案”口径)----

#[derive(Debug, Clone, Default, PartialEq)]
pub struct SharedAnswers {
    pub task_type: Option<String>,
    pub difficulty: Option<f32>,
    pub frontier_needed: Option<f32>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DimAnswer {
    pub raw_score: f32,
    pub confidence: f32,
    pub legend: BTreeMap<String, String>,
    pub probabilities: BTreeMap<String, f32>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct CandidateAnswers {
    pub quality: Option<DimAnswer>,
    pub cost: Option<DimAnswer>,
}

#[derive(Debug, Clone, Default)]
pub struct DecodedAnswers {
    pub shared: SharedAnswers,
    pub per_candidate: Vec<CandidateAnswers>,
    /// 缺失或不可解析的问题 ID(→ meta.partial_answers)
    pub partial: Vec<String>,
}

pub fn decode_answers(resp: &JudgeResponse, candidate_count: usize) -> DecodedAnswers {
    let mut out = DecodedAnswers::default();
    let mut get = |id: &str| resp.answers.get(id);

    match get("task_type") {
        Some(v) if v.get("type").and_then(|t| t.as_str()) == Some("choice") => {
            match v.get("choice").and_then(|c| c.as_str()) {
                Some(c) => out.shared.task_type = Some(c.to_string()),
                None => out.partial.push("task_type".into()),
            }
        }
        Some(_) => out.partial.push("task_type".into()),
        None => out.partial.push("task_type".into()),
    }
    // difficulty / frontier_needed 同模式:type == "score"/"noul" 且数值合法 → Some,否则 partial
    // (执行者按上述模式补全两段;务必覆盖“值类型错误”(如 score 是字符串) → partial)

    for i in 0..candidate_count {
        let mut ca = CandidateAnswers::default();
        ca.quality = decode_dim(get(&format!("c{i}.quality_fit")), &mut out.partial, &format!("c{i}.quality_fit"));
        ca.cost = decode_dim(get(&format!("c{i}.cost_fit")), &mut out.partial, &format!("c{i}.cost_fit"));
        out.per_candidate.push(ca);
    }
    out
}

fn decode_dim(v: Option<&serde_json::Value>, partial: &mut Vec<String>, id: &str) -> Option<DimAnswer> {
    let v = v?;
    if v.get("type").and_then(|t| t.as_str()) != Some("score") {
        partial.push(id.to_string());
        return None;
    }
    let Some(score) = v.get("score").and_then(|s| s.as_f64()) else {
        partial.push(id.to_string());
        return None;
    };
    let legend = v.get("legend").and_then(|l| l.as_object()).map(|o| {
        o.iter().filter_map(|(k, val)| val.as_str().map(|s| (k.clone(), s.to_string()))).collect()
    }).unwrap_or_default();
    let probabilities = v.get("probabilities").and_then(|p| p.as_object()).map(|o| {
        o.iter().filter_map(|(k, val)| val.as_f64().map(|f| (k.clone(), f as f32))).collect()
    }).unwrap_or_default();
    let confidence = v.get("confidence").and_then(|c| c.as_f64()).unwrap_or(0.0) as f32;
    Some(DimAnswer { raw_score: score as f32, confidence, legend, probabilities })
}

#[cfg(test)]
mod tests {
    // 测试要点(用 helper 构造 JudgeResponse):
    // 1. 正常响应(3 个共享 + 2 候选 × 2 维)→ 全部 Some,partial 空;
    // 2. 缺 c1.quality_fit → 该维 None,partial 含 "c1.quality_fit",其余不受影响;
    // 3. type 编码错误(score 字段是字符串 / type 写错)→ partial 且 None;
    // 4. noul 缺失 → frontier_needed None + partial;
    // 5. degraded_reason 映射:400→backend_4xx、401→backend_4xx、429→backend_429、
    //    529→backend_unavailable、Timeout→backend_unavailable、MissingKey→backend_unavailable。
    // 6. JudgeRequest 序列化形状:questions 里 type 字段名是 "type"(json 断言)。
}
```

- [ ] **Step 4: 验证 + Commit**

```bash
cargo test -p recommender-core    # 期望:全过
cargo fmt --all
git add -A && git commit -m "feat(core): systemone contract types and tolerant answer decoding"
```

---

## Task 9: core::mock(MockBackend)

**Files:**
- Create: `crates/core/src/mock.rs`
- Modify: `crates/core/src/lib.rs`(加 `pub mod mock;`)

- [ ] **Step 1~3: 实现(含测试)**

```rust
//! 确定性 Mock 后端: 供测试与离线开发(spec M0“mock 后端”)。
//! 行为: 按问题 ID 生成固定合法答案;不认识的问题不给答案。

use crate::judgment::*;
use futures::future::BoxFuture;
use serde_json::json;

pub struct MockBackend { pub backend_id: String }

impl MockBackend {
    pub fn new(id: impl Into<String>) -> Self { Self { backend_id: id.into() } }
}

impl JudgmentBackend for MockBackend {
    fn id(&self) -> &str { &self.backend_id }

    fn judge<'a>(&'a self, req: JudgeRequest) -> BoxFuture<'a, Result<JudgeResponse, JudgeError>> {
        Box::pin(async move {
            let mut answers = serde_json::Map::new();
            for id in req.questions.keys() {
                let ans = match id.as_str() {
                    "task_type" => json!({"type":"choice","choice":"chat",
                        "probabilities":{"chat":0.9},"confidence":0.9}),
                    "difficulty" => json!({"type":"score","score":1.0,
                        "legend":{"0":"简单","1":"中等","2":"困难"},
                        "probabilities":{"0":0.2,"1":0.6,"2":0.2},"confidence":0.8}),
                    "frontier_needed" => json!({"type":"noul","noul":0.3}),
                    _ if id.ends_with(".quality_fit") => json!({"type":"score","score":1.5,
                        "legend":{},"probabilities":{},"confidence":0.7}),
                    _ if id.ends_with(".cost_fit") => json!({"type":"score","score":1.0,
                        "legend":{},"probabilities":{},"confidence":0.7}),
                    _ => continue,
                };
                answers.insert(id.clone(), ans);
            }
            Ok(JudgeResponse {
                model: "mock-1".into(),
                answers: answers.into_iter().collect(),
                usage: Some(WireUsage { input_tokens: 300, output_tokens: 10 }),
            })
        })
    }
}

#[cfg(test)]
mod tests {
    // 测试要点:
    // 1. 传入 questions 含 5 类 id → 全部有答案;未知 id("whatever")→ 无答案;
    // 2. 两次调用输出完全相同(确定性);
    // 3. 输出可被 decode_answers 全量解码(partial 为空)。
    // 用法: tokio::test(async) 中直接 .judge(req).await(核心 dev-deps 已含 tokio)。
}
```

- [ ] **Step 4: 验证 + Commit**

```bash
cargo test -p recommender-core    # 期望:全过
cargo fmt --all
git add -A && git commit -m "feat(core): deterministic mock judgment backend"
```

---

## Task 10: core::questions(state + 问题构造)

**Files:**
- Create: `crates/core/src/questions.rs`
- Modify: `crates/core/src/lib.rs`(加 `pub mod questions;`)

- [ ] **Step 1~4: 实现(含测试)**

```rust
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
pub enum PriceBand { Cheap, Mid, High }

impl PriceBand {
    pub fn as_str(&self) -> &'static str {
        match self { PriceBand::Cheap => "cheap", PriceBand::Mid => "mid", PriceBand::High => "high" }
    }
}

/// 边界含端值:≤ cheap_max → cheap;≤ mid_max → mid;否则 high(spec §5.3)。
pub fn price_band(in_price: f64, bands: &PriceBands) -> PriceBand {
    if in_price <= bands.cheap_max { PriceBand::Cheap }
    else if in_price <= bands.mid_max { PriceBand::Mid }
    else { PriceBand::High }
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
pub fn build_state(profile: &Profile, constraints: &Constraints, choices: &[CandidateView]) -> Value {
    let mut cons = serde_json::Map::new();
    if let Some(b) = &constraints.budget {
        if let Some(p) = b.max_input_price_per_mtok {
            cons.insert("budget_max_in_price".into(), json!(p));
        }
        cons.insert("cost_sensitive".into(), json!(b.cost_sensitive));
    }

    let candidates: Vec<Value> = choices.iter().map(|c| json!({
        "i": c.i, "id": c.id, "desc": c.desc, "tags": c.tags, "ctx": c.ctx,
        "price_band": c.price_band.as_str(), "in_price": c.in_price, "out_price": c.out_price,
    })).collect();

    let mut state = serde_json::Map::new();
    state.insert("profile".into(), json!({
        "est_tokens": profile.est_tokens,
        "has_images": profile.has_images,
        "tools_count": profile.tools_count,
        "json_mode": profile.json_mode,
        "max_tokens": profile.max_tokens,
        "locale_hint": profile.locale_hint,
    }));
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
            questions.insert(format!("c{}.{}", c.i, suffix), question_from(spec, Some(c.i)));
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
        Value::Object(o) => Value::Object(o.iter().map(|(k, x)| (k.clone(), substitute_i(x, idx))).collect()),
        other => other.clone(),
    }
}

#[cfg(test)]
mod tests {
    // 测试要点:
    // 1. price_band 边界:1.0→cheap、1.0001→mid、5.0→mid、5.0001→high(用 spec 默认 bands)。
    // 2. build_state:
    //    - 无预算 → state["constraints"] == {} (空对象),无 "task_hints" 键(hints 为空时);
    //    - 有预算 → budget_max_in_price 与 cost_sensitive 出现;task_hints 非空 → 出现;
    //    - candidates 元素键恰好 {i,id,desc,tags,ctx,price_band,in_price,out_price};
    //    - 顺序/序号 = choices 顺序。
    // 3. build_judge_request:
    //    - 问题数 = shared.len() + 2×候选数;ID 集合正确;
    //    - c0.quality_fit.instructions 是字符串且包含 "candidates[0]"(用仓库 questions.yaml,含 <i>);
    //    - criteria 原样透传(type=score 时是数组)。
    // 4. insta 快照:一个固定 profile + 2 候选的完整 JudgeRequest(pretty JSON)。
    //    首次生成快照: `INSTA_UPDATE=always cargo test -p recommender-core questions` 然后重跑确认绿。
    // 5. substitute_i: 嵌套对象/数组字符串都被替换;None → 原样。
}
```

- [ ] **Step 5: 验证 + Commit**

```bash
cargo test -p recommender-core    # 期望:全过(快照文件已生成)
cargo fmt --all
git add -A && git commit -m "feat(core): judge request builder with state and per-candidate questions"
```

---

## Task 11: 分片与截断规划(questions.rs 追加)

**Files:**
- Modify: `crates/core/src/questions.rs`(追加 `plan_shards` + 测试)

- [ ] **Step 1~4: 实现(追加到 questions.rs)**

```rust
/// 候选规模策略(spec §6.2):
/// N ≤ in_call → 单次;in_call < N ≤ judged → 分片(每片 ≤ in_call);
/// N > judged → 先截断到 judged(截断由调用方按粗排结果执行),再分片。
#[derive(Debug, Clone, PartialEq)]
pub struct ShardPlan {
    pub truncate_to: Option<usize>,
    pub shards: Vec<std::ops::Range<usize>>,
}

pub fn plan_shards(n: usize, in_call: usize, judged: usize) -> ShardPlan {
    let truncate_to = if n > judged { Some(judged) } else { None };
    let effective = truncate_to.unwrap_or(n);
    let mut shards = Vec::new();
    let mut start = 0;
    while start < effective {
        let end = (start + in_call).min(effective);
        shards.push(start..end);
        start = end;
    }
    ShardPlan { truncate_to, shards }
}
```

测试要点(追加在既有 tests 模块内):
- `plan_shards(0,30,60)` → shards 空;
- `(1,30,60)` → `[0..1]`;`(30,30,60)` → `[0..30]`;
- `(31,30,60)` → `[0..30, 30..31]`,truncate None;
- `(60,30,60)` → `[0..30, 30..60]`;
- `(61,30,60)` → truncate Some(60),shards `[0..30, 30..60]`;
- `(90,30,60)` → truncate Some(60);
- 边界:`in_call=1, judged=2, n=5` → truncate Some(2),shards `[0..1,1..2]`。

- [ ] **Step 5: 验证 + Commit**

```bash
cargo test -p recommender-core && cargo fmt --all
git add -A && git commit -m "feat(core): shard and truncation planner"
```

---

## Task 12: core::fusion(权重/融合/排序键/置信度)

**Files:**
- Create: `crates/core/src/fusion.rs`
- Modify: `crates/core/src/lib.rs`(加 `pub mod fusion;`)

- [ ] **Step 1~4: 实现(含测试)**

```rust
//! 融合(spec §6.4): 归一化 = score/(档数-1);权重 = base + adjustments(全条件 AND)→ 归一化 → clamp;
//! final = w_q·q + w_c·c − penalty;tie-break: 价格升序 → id 字典序;confidence = min(维度 confidence)。

use crate::api::Constraints;
use crate::config::{Conditions, QuestionsConfig};
use crate::judgment::{DimAnswer, SharedAnswers};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Weights { pub quality_fit: f32, pub cost_fit: f32 }

/// adjustments 命中判定:同一调整内**所有**出现的条件都需满足;未出现的条件不约束(spec §6.4)。
fn cond_hit(c: &Conditions, s: &SharedAnswers, cons: &Constraints) -> bool {
    let d = c.difficulty_at_least.map_or(true, |t| s.difficulty.is_some_and(|v| v >= t));
    let f = c.frontier_needed_above.map_or(true, |t| s.frontier_needed.is_some_and(|v| v > t));
    let b = c.budget_present.map_or(true, |t| t == cons.budget.is_some());
    let cs = c.cost_sensitive.map_or(true, |t| {
        t == cons.budget.as_ref().is_some_and(|b| b.cost_sensitive)
    });
    d && f && b && cs
}

pub fn compute_weights(cfg: &QuestionsConfig, shared: &SharedAnswers, constraints: &Constraints) -> Weights {
    let mut wq = cfg.weights.base.quality_fit;
    let mut wc = cfg.weights.base.cost_fit;
    for adj in &cfg.weights.adjustments {
        if !cond_hit(&adj.when, shared, constraints) { continue; }
        for (k, dv) in &adj.set {
            let Ok(delta) = dv.delta() else { continue }; // 校验已拒绝坏值;此处防御
            match k.as_str() {
                "quality_fit" => wq += delta,
                "cost_fit" => wc += delta,
                _ => {}
            }
        }
    }
    let sum = wq + wc;
    if sum > 0.0 { wq /= sum; wc /= sum; }
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
pub fn fuse_candidate(w: &Weights, q: Option<&DimAnswer>, c: Option<&DimAnswer>,
                      q_levels: usize, c_levels: usize, penalty: f32) -> FusedScore {
    let q_norm = q.map(|d| norm_score(d.raw_score, q_levels)).unwrap_or(0.5);
    let c_norm = c.map(|d| norm_score(d.raw_score, c_levels)).unwrap_or(0.5);
    let confidence = q.map(|d| d.confidence).unwrap_or(0.0)
        .min(c.map(|d| d.confidence).unwrap_or(0.0));
    FusedScore {
        quality_norm: q_norm,
        cost_norm: c_norm,
        score: final_score(*w, q_norm, c_norm, penalty),
        confidence,
    }
}

/// 排序比较器:score 降序 → 输入价升序 → id 字典序(完全确定性)。
pub fn cmp_rank(score_a: f32, price_a: f64, id_a: &str,
                score_b: f32, price_b: f64, id_b: &str) -> std::cmp::Ordering {
    score_b.partial_cmp(&score_a).unwrap_or(std::cmp::Ordering::Equal)
        .then(price_a.partial_cmp(&price_b).unwrap_or(std::cmp::Ordering::Equal))
        .then(id_a.cmp(id_b))
}

/// 输出前统一舍入到 3 位小数(与 spec §8.1 示例一致)。
pub fn round3(x: f32) -> f32 { (x * 1000.0).round() / 1000.0 }

#[cfg(test)]
mod tests {
    // 测试用法: 用 `test_cfg(extra_adjustments: &str) -> QuestionsConfig` 拼一份完整小 YAML
    // (shared 含 task_type/difficulty/frontier_needed 最小合法形状、per_candidate 两问、price_bands、weights、confidence)。
    //
    // 测试要点:
    // 1. spec §8.1 数值闭环: base 0.75/0.25;仅 budget_present 命中(difficulty=1.2、无 frontier)
    //    → weights == 0.65/0.35;q raw=2.76(4 档)→ norm=0.92;c raw=1.0(3 档)→ norm=0.50;
    //    final == 0.773(误差 < 1e-4);round3 后 == 0.773。
    // 2. 四个调整全命中(difficulty=1.6、frontier=0.7、有预算、cost_sensitive=true)→ 仍为 0.65/0.35。
    // 3. difficulty 恰好 = 1.5 → 命中(>=);frontier 恰好 = 0.6 → 不命中(严格 >)。
    // 4. clamp:构造极端 adjustments({quality_fit:"-0.70", cost_fit:"+0.90"})使归一化后 wc>0.95
    //    (wq=0.05、wc=1.15,sum=1.20 → wc≈0.958)→ 被 clamp 到 0.95(只写 cost_fit "+0.90" 时 sum=1.90 → wc≈0.605,触发不了钳制)。
    // 5. norm_score: raw 3.0/4 档 → 1.0;raw -1 → 0.0;levels=1 → raw/1。
    // 6. 缺答案:q=None → q_norm 0.5 且 confidence 0.0;两者都 None → score = w_q*0.5+w_c*0.5。
    // 7. cmp_rank:同分低价优先;同分同价 id 升序。
}
```

- [ ] **Step 5: 验证 + Commit**

```bash
cargo test -p recommender-core && cargo fmt --all
git add -A && git commit -m "feat(core): deterministic fusion, weights normalization and tie-breaks"
```

---

## Task 13: core::explain(理由模板 + 渲染器)

**Files:**
- Create: `crates/core/src/explain.rs`、`templates/reason.zh.toml`、`templates/reason.en.toml`
- Modify: `crates/core/src/lib.rs`(加 `pub mod explain;`)

- [ ] **Step 1: 写两份模板文件(逐字)**

`templates/reason.zh.toml`:

```toml
hard_constraints = "✅ 硬约束全部满足:上下文 {{ ctx }} ≥ 估算 {{ est }};safety 系数 {{ safety }};{{ caps }}"
hard_constraints_basic = "✅ 硬约束全部满足:上下文 {{ ctx }} ≥ 估算 {{ est }};safety 系数 {{ safety }}"
quality = "🧠 质量匹配 {{ q }}:{{ note }}{{ top_part }}"
cost_with_budget = "💰 成本适配 {{ c }}({{ in }}/M 输入,{{ band }}):在预算上限内"
cost_without_budget = "💰 成本适配 {{ c }}({{ in }}/M 输入,{{ band }}):未设置预算约束"
low_confidence = "⚠️ 置信度偏低({{ conf }}):建议结合相邻候选对比决策"
penalty = "➖ 软惩罚 -{{ value }}({{ reason }})"
degraded = "⚠️ 未经语义判断(后端不可用):以下为确定性粗排结果"
band_cheap = "便宜"
band_mid = "中档"
band_high = "昂贵"
quality_note_default = "与需求特征吻合"

[quality_notes]
chat = "适合常规对话与问答"
code_debug = "与代码调试类任务特征吻合"
code_gen = "适合代码生成类任务"
reasoning = "适合推理分析类任务"
translation = "适合翻译任务"
creative = "适合创意写作任务"
summarization = "适合摘要提炼任务"
extraction = "适合结构化抽取任务"
```

`templates/reason.en.toml`:

```toml
hard_constraints = "✅ Hard constraints satisfied: context {{ ctx }} ≥ est. {{ est }}; safety factor {{ safety }};{{ caps }}"
hard_constraints_basic = "✅ Hard constraints satisfied: context {{ ctx }} ≥ est. {{ est }}; safety factor {{ safety }}"
quality = "🧠 Quality fit {{ q }}: {{ note }}{{ top_part }}"
cost_with_budget = "💰 Cost fit {{ c }} ({{ in }}/M input, {{ band }}): within budget cap"
cost_without_budget = "💰 Cost fit {{ c }} ({{ in }}/M input, {{ band }}): no budget constraint set"
low_confidence = "⚠️ Low confidence ({{ conf }}): compare with the next ranked candidate"
penalty = "➖ Soft penalty -{{ value }} ({{ reason }})"
degraded = "⚠️ Without semantic judgment (backend unavailable): deterministic coarse ranking below"
band_cheap = "cheap"
band_mid = "mid-priced"
band_high = "expensive"
quality_note_default = "fits the request profile"

[quality_notes]
chat = "suitable for general chat"
code_debug = "a good match for debugging tasks"
code_gen = "suitable for code generation"
reasoning = "suitable for reasoning tasks"
translation = "suitable for translation"
creative = "suitable for creative writing"
summarization = "suitable for summarization"
extraction = "suitable for structured extraction"
```

- [ ] **Step 2~4: 实现 explain.rs(含测试)**

```rust
//! 理由渲染(spec §6.5): 模板化、zh/en、零额外 LLM 调用。
//! 模板只做 `{{ var }}` 插值;所有数值/措辞在 Rust 侧预格式化(降低模板能力依赖)。

use crate::api::Locale;
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
```

> **依赖补充**:本 Task 给 workspace 增加 `toml = "0.8"`(根 `Cargo.toml` 的 `[workspace.dependencies]` 加一行),并在 `crates/core/Cargo.toml` 的 `[dependencies]` 加 `toml = { workspace = true }`。

```rust
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

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PenaltyView { pub value: f32, pub reason_key: String }

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
    pub price_bands: crate::config::PriceBands,
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
            let s = std::fs::read_to_string(p).map_err(|e| ExplainError::Io(p.display().to_string(), e))?;
            toml::from_str(&s).map_err(|e| ExplainError::Toml(p.display().to_string(), e))
        };
        Ok(Self { zh: load(zh_path)?, en: load(en_path)? })
    }

    /// 便捷构造:templates_dir/reason.{zh,en}.toml
    pub fn from_dir(dir: &Path) -> Result<Self, ExplainError> {
        Self::from_files(&dir.join("reason.zh.toml"), &dir.join("reason.en.toml"))
    }
}

impl ReasonRenderer for TemplateReasonRenderer {
    fn render(&self, ctx: &ReasonCtx, locale: Locale) -> Vec<String> {
        let (t, caps_join, top_fmt) = match locale {
            Locale::Zh => (&self.zh, "、", zh_top),
            Locale::En => (&self.en, ", ", en_top),
        };
        let mut out = Vec::new();

        if ctx.degraded {
            out.push(render_tpl(&t.degraded, &serde_json::json!({})).expect("常量模板"));
        }

        // 硬约束行:caps = profile 需要的、且模型满足的能力(过滤后必然全满足)
        let mut caps_vec: Vec<&str> = Vec::new();
        if ctx.profile.tools_count > 0 { caps_vec.push(match locale { Locale::Zh => "支持 tool_calls", Locale::En => "tool calling supported" }); }
        if ctx.profile.has_images { caps_vec.push(match locale { Locale::Zh => "支持视觉", Locale::En => "vision supported" }); }
        if ctx.profile.json_mode { caps_vec.push(match locale { Locale::Zh => "支持 JSON 模式", Locale::En => "JSON mode supported" }); }
        let vars = serde_json::json!({
            "ctx": ctx.model.context_window,
            "est": ctx.profile.est_tokens,
            "safety": ctx.safety_factor,
            "caps": caps_vec.join(caps_join),
        });
        let tpl = if caps_vec.is_empty() { &t.hard_constraints_basic } else { &t.hard_constraints };
        out.push(render_tpl(tpl, &vars).expect("硬约束模板"));

        // 质量/成本/置信度/软惩罚(降级时跳过前三类)
        if let Some(v) = &ctx.verdict {
            let note = v.task_type.as_deref()
                .and_then(|tt| t.quality_notes.get(tt))
                .cloned()
                .unwrap_or_else(|| t.quality_note_default.clone());
            let top_part = v.quality_top.as_ref()
                .map(|(label, p)| top_fmt(label, *p))
                .unwrap_or_default();
            out.push(render_tpl(&t.quality, &serde_json::json!({
                "q": format!("{:.2}", v.quality_norm),
                "note": note,
                "top_part": top_part,
            })).expect("quality 模板"));

            let band = match crate::questions::price_band(ctx.model.price.in_per_mtok, &ctx.price_bands) {
                crate::questions::PriceBand::Cheap => &t.band_cheap,
                crate::questions::PriceBand::Mid => &t.band_mid,
                crate::questions::PriceBand::High => &t.band_high,
            };
            let cost_tpl = if ctx.budget_present { &t.cost_with_budget } else { &t.cost_without_budget };
            out.push(render_tpl(cost_tpl, &serde_json::json!({
                "c": format!("{:.2}", v.cost_norm),
                "in": format!("${:.2}", ctx.model.price.in_per_mtok),
                "band": band,
            })).expect("cost 模板"));

            if v.low_confidence {
                out.push(render_tpl(&t.low_confidence, &serde_json::json!({
                    "conf": format!("{:.2}", v.confidence),
                })).expect("low_confidence 模板"));
            }
        }

        for p in ctx.penalties {
            out.push(render_tpl(&t.penalty, &serde_json::json!({
                "value": format!("{:.2}", p.value),
                "reason": p.reason_key,
            })).expect("penalty 模板"));
        }
        out
    }
}

/// zh: ` (概率集中在「最佳匹配」61%)`;en: ` (concentrated on "best match" 61%)`
fn zh_top(label: &str, p: f32) -> String { format!(" (概率集中在「{label}」{:.0}%)", p * 100.0) }
fn en_top(label: &str, p: f32) -> String { format!(" (concentrated on \"{label}\" {:.0}%)", p * 100.0) }

fn render_tpl(tpl: &str, ctx: &serde_json::Value) -> Result<String, ExplainError> {
    let env = minijinja::Environment::new();
    env.render_str(tpl, ctx).map_err(|e| ExplainError::Template(e.to_string()))
}
```

> `ReasonCtx.budget_present` 与 `price_bands` 由 Task 14(pipeline)填充:`budget_present = constraints.budget.is_some()`;`price_bands` 来自 `questions.yaml`。

测试要点:
- 用仓库 `templates/`(`CARGO_MANIFEST_DIR/../../templates`)构造渲染器;
- zh 快照:固定 ctx(有 verdict、某条 penalty、low_confidence=true、needs 含 tools+vision)→ `insta::assert_snapshot!` 整段 Vec 的 debug 输出(每条一行);
- en 快照:同 ctx 换 Locale::En;
- 降级 ctx(verdict=None、degraded=true)→ 首行含 "未经语义判断",无质量/成本行;
- 无 caps 需求 → 用 basic 模板(不出现结尾 `;`);
- 坏 TOML 文件 → `ExplainError::Toml`。

- [ ] **Step 5: 验证 + Commit**

```bash
cargo test -p recommender-core && cargo fmt --all
git add -A && git commit -m "feat(core): templated bilingual reason renderer"
```

---

## Task 14: core::pipeline(prepare + recommend 全流水线)

**Files:**
- Create: `crates/core/src/pipeline.rs`
- Modify: `crates/core/src/lib.rs`(加 `pub mod pipeline;`)

- [ ] **Step 1~4: 实现(含测试)**

```rust
//! 流水线组装(spec §4.1/§8.2): prepare 纯逻辑可离线;recommend 执行判断 + 融合 + 降级。

use crate::api::*;
use crate::catalog::CatalogProvider;
use crate::coarse::{self, CoarseScore};
use crate::config::{LimitsConfig, LoadedConfigs, QuestionsConfig};
use crate::explain::{PenaltyView, ReasonCtx, ReasonRenderer, VerdictView};
use crate::fusion::{self, Weights};
use crate::judgment::{decode_answers, DecodedAnswers, JudgeError, JudgeRequest, JudgmentBackend};
use crate::profile::Profile;
use crate::questions::{self, CandidateView};
use crate::rules::{self, Candidate, Rule};
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Instant;

#[derive(Debug, thiserror::Error)]
pub enum PipelineError {
    #[error("{0}")]
    InvalidConstraints(String),
    #[error("{0}")]
    InvalidRequestBody(String),
    #[error("{0}")]
    Upstream(String), // 仅 include_debug=true 时透出(spec §8.2)
    #[error("{0}")]
    Internal(String),
}

pub struct Pipeline {
    pub catalog: Arc<dyn CatalogProvider>,
    pub rules: Vec<Box<dyn Rule>>,
    pub questions: QuestionsConfig,
    pub limits: LimitsConfig,
    pub backend: Arc<dyn JudgmentBackend>,
    pub renderer: Arc<dyn ReasonRenderer>,
    /// 发送给后端的 model 字段(dry-run 与真实调用一致);mock 场景可为空
    pub backend_model_hint: String,
    pub weights_version: String,
    /// 灌入理由渲染的 safety 系数(与 context_length 规则同源)
    pub safety_factor: f64,
}

impl Pipeline {
    pub fn new(
        configs: &LoadedConfigs,
        catalog: Arc<dyn CatalogProvider>,
        backend: Arc<dyn JudgmentBackend>,
        renderer: Arc<dyn ReasonRenderer>,
        backend_model_hint: String,
    ) -> Self {
        let weights_version = {
            let canonical = serde_json::to_string(&serde_json::json!({
                "weights": serde_json::to_value(&configs.questions.weights).ok(),
                "confidence": serde_json::to_value(&configs.questions.confidence).ok(),
                "price_bands": serde_json::to_value(&configs.questions.price_bands).ok(),
            })).expect("可序列化");
            blake3::hash(canonical.as_bytes()).to_hex().to_string()[..16].to_string()
        };
        let safety_factor = configs.rules.rules.iter()
            .find(|r| r.enabled && r.kind == crate::config::RuleKind::ContextLength)
            .and_then(|r| r.safety_factor)
            .unwrap_or(1.15);
        Self {
            catalog,
            rules: rules::build_rules(&configs.rules),
            questions: configs.questions.clone(),
            limits: configs.server.limits.clone(),
            backend,
            renderer,
            backend_model_hint,
            weights_version,
            safety_factor,
        }
    }
}

/// prepare 的产物:dry-run 与 recommend 共用,保证"发出去的请求"零差异。
pub struct Prepared {
    pub profile: Profile,
    pub constraints: Constraints,
    pub options: Options,
    /// 通过硬规则的**全部**候选(catalog 顺序)
    pub candidates: Vec<Candidate>,
    /// 实际参与判断的候选在 candidates 中的下标(截断时按粗排顺序)
    pub judge_indices: Vec<usize>,
    /// 分片请求(局部序号已重编);与 shard_ranges 对齐
    pub shard_requests: Vec<JudgeRequest>,
    /// 各分片覆盖的 judge_indices 区间
    pub shard_ranges: Vec<std::ops::Range<usize>>,
    pub filtered: Vec<FilteredByRule>,
    /// 全量候选的粗排分(降级排序与截断共用)
    pub coarse_map: BTreeMap<String, CoarseScore>,
}

impl Pipeline {
    pub fn prepare(&self, req: &RecommendRequest) -> Result<Prepared, PipelineError> {
        req.validate().map_err(PipelineError::InvalidConstraints)?;
        let profile = Profile::from_value(&req.request)
            .map_err(|e| PipelineError::InvalidRequestBody(e.to_string()))?;

        let ctx = rules::RuleCtx { profile: &profile, constraints: &req.constraints, options: &req.options };
        let report = rules::apply_rules(&self.rules, &ctx, self.catalog.models());

        let coarse_all = coarse::coarse_rank(&profile, self.catalog.models());
        let coarse_map: BTreeMap<String, CoarseScore> = coarse_all.iter()
            .map(|(m, s)| (m.id.clone(), *s)).collect();

        let plan = questions::plan_shards(
            report.candidates.len(),
            self.limits.max_candidates_in_call,
            self.limits.max_candidates_judged,
        );

        // judge_indices: 未截断 → 原顺序;截断 → 粗排顺序取前 k
        let candidates = report.candidates;
        let judge_indices: Vec<usize> = if plan.truncate_to.is_some() {
            let mut idx: Vec<usize> = (0..candidates.len()).collect();
            idx.sort_by(|&a, &b| {
                let (ma, mb) = (&candidates[a].model, &candidates[b].model);
                let (sa, sb) = (coarse_map[&ma.id], coarse_map[&mb.id]);
                fusion::cmp_rank(sa.total(), ma.price.in_per_mtok, &ma.id,
                                 sb.total(), mb.price.in_per_mtok, &mb.id)
            });
            idx.truncate(plan.truncate_to.unwrap());
            idx
        } else {
            (0..candidates.len()).collect()
        };

        // 分片请求:每片局部重编号(问题 ID c{local},spec §6.2/§6.3)
        let mut shard_requests = Vec::with_capacity(plan.shards.len());
        for range in &plan.shards {
            let views: Vec<CandidateView> = range.clone().map(|pos| {
                let gi = judge_indices[pos];
                questions::candidate_view(pos - range.start, &candidates[gi].model, &self.questions.price_bands)
            }).collect();
            shard_requests.push(questions::build_judge_request(
                &self.backend_model_hint, &profile, &req.constraints, &self.questions, &views,
            ));
        }

        // filtered 汇总:按首次命中规则计数
        let mut counts: BTreeMap<String, u32> = BTreeMap::new();
        for f in &report.filtered_by_rule {
            *counts.entry(f.rule.clone()).or_insert(0) += 1;
        }
        let filtered = counts.into_iter().map(|(rule, count)| FilteredByRule { rule, count }).collect();

        Ok(Prepared {
            profile,
            constraints: req.constraints.clone(),
            options: req.options.clone(),
            candidates,
            judge_indices,
            shard_requests,
            shard_ranges: plan.shards,
            filtered,
            coarse_map,
        })
    }
}

impl Pipeline {
    pub async fn recommend(&self, req: &RecommendRequest, request_id: &str) -> Result<RecommendResponse, PipelineError> {
        let t0 = Instant::now();
        let p = self.prepare(req)?;
        let top_k = p.options.top_k.unwrap_or(self.limits.default_top_k).min(MAX_TOP_K) as usize;

        if p.candidates.is_empty() {
            return Ok(self.empty_response(&p, t0.elapsed().as_millis() as u64, request_id));
        }

        let futures_vec: Vec<_> = p.shard_requests.iter().cloned()
            .map(|r| self.backend.judge(r)).collect();
        let results = futures::future::join_all(futures_vec).await;

        let mut responses = Vec::with_capacity(results.len());
        for r in results {
            match r {
                Ok(resp) => responses.push(resp),
                Err(e) => {
                    if p.options.include_debug {
                        return Err(PipelineError::Upstream(e.to_string()));
                    }
                    return Ok(self.degraded_response(&p, &e, t0.elapsed().as_millis() as u64, request_id, top_k));
                }
            }
        }

        // 解码 + 按全局下标合并(spec §6.2)
        let mut decoded = DecodedAnswers {
            per_candidate: vec![Default::default(); p.judge_indices.len()],
            ..Default::default()
        };
        let mut usage_in = 0u64;
        let mut usage_out = 0u64;
        let mut actual_model = String::new();
        for (shard, resp) in responses.iter().enumerate() {
            if actual_model.is_empty() { actual_model = resp.model.clone(); }
            if let Some(u) = &resp.usage { usage_in += u.input_tokens; usage_out += u.output_tokens; }
            let range = &p.shard_ranges[shard];
            let local = decode_answers(resp, range.len());
            if shard == 0 { decoded.shared = local.shared.clone(); }
            let local_len = local.per_candidate.len();
            for (li, ca) in local.per_candidate.into_iter().enumerate() {
                let gi = range.start + li;
                if gi < decoded.per_candidate.len() {
                    decoded.per_candidate[gi] = ca;
                }
            }
            for pid in local.partial {
                decoded.partial.push(remap_partial_id(&pid, range.start, local_len));
            }
        }
        decoded.partial.sort();
        decoded.partial.dedup();

        // 融合 + 排序(全局)
        let weights = fusion::compute_weights(&self.questions, &decoded.shared, &p.constraints);
        let q_levels = self.questions.quality_levels();
        let c_levels = self.questions.cost_levels();

        // 行: (judge_indices 位置, FusedScore)
        let mut rows: Vec<(usize, fusion::FusedScore)> = (0..p.judge_indices.len()).map(|pos| {
            let cand = &p.candidates[p.judge_indices[pos]];
            let ca = &decoded.per_candidate[pos];
            let fs = fusion::fuse_candidate(&weights, ca.quality.as_ref(), ca.cost.as_ref(),
                                            q_levels, c_levels, cand.penalties);
            (pos, fs)
        }).collect();

        rows.sort_by(|a, b| {
            let (ca, cb) = (&p.candidates[p.judge_indices[a.0]], &p.candidates[p.judge_indices[b.0]]);
            fusion::cmp_rank(a.1.score, ca.model.price.in_per_mtok, &ca.model.id,
                             b.1.score, cb.model.price.in_per_mtok, &cb.model.id)
        });

        let recommendations: Vec<Recommendation> = rows.iter().take(top_k).enumerate()
            .map(|(rank0, (pos, fs))| {
                let cand = &p.candidates[p.judge_indices[*pos]];
                let ca = &decoded.per_candidate[*pos];
                let low_confidence = fs.confidence < self.questions.confidence.flag_below;
                let verdict = VerdictView {
                    quality_norm: fs.quality_norm,
                    cost_norm: fs.cost_norm,
                    weights,
                    confidence: fs.confidence,
                    low_confidence,
                    quality_top: top_legend(ca.quality.as_ref()),
                    cost_top: top_legend(ca.cost.as_ref()),
                    task_type: decoded.shared.task_type.clone(),
                };
                let penalties_view: Vec<PenaltyView> = cand.penalty_records.iter()
                    .map(|pr| PenaltyView { value: pr.value, reason_key: pr.reason_key.clone() })
                    .collect();
                let rctx = ReasonCtx {
                    rank: rank0 as u32 + 1,
                    model: &cand.model,
                    profile: &p.profile,
                    safety_factor: self.safety_factor,
                    rule_hits: &cand.rule_hits,
                    penalties: &penalties_view,
                    degraded: false,
                    verdict: Some(verdict),
                    budget_present: p.constraints.budget.is_some(),
                    price_bands: self.questions.price_bands,
                };
                Recommendation {
                    model: cand.model.id.clone(),
                    rank: rank0 as u32 + 1,
                    score: fusion::round3(fs.score),
                    confidence: Some(fusion::round3(fs.confidence)),
                    low_confidence,
                    breakdown: Breakdown {
                        quality_fit: Some(fusion::round3(fs.quality_norm)),
                        cost_fit: Some(fusion::round3(fs.cost_norm)),
                        weights: Some(WeightsOut { quality_fit: weights.quality_fit, cost_fit: weights.cost_fit }),
                        penalty: Some(fusion::round3(cand.penalties)),
                        rule_hits: cand.rule_hits.clone(),
                        coarse: None,
                    },
                    reasons: self.renderer.render(&rctx, p.options.locale),
                }
            })
            .collect();

        let debug = if p.options.include_debug {
            Some(serde_json::json!({
                "questions_sent": p.shard_requests,
                "answers_raw": responses,
                "state": p.shard_requests.iter().map(|r| r.state.clone()).collect::<Vec<_>>(),
            }))
        } else { None };

        Ok(RecommendResponse {
            recommendations,
            profile: profile_view(&p, Some(&decoded.shared)),
            degraded: false,
            debug,
            meta: Meta {
                backend: if actual_model.is_empty() { self.backend.id().to_string() } else { actual_model },
                latency_ms: t0.elapsed().as_millis() as u64,
                cache_hit: false,
                catalog_version: self.catalog.version().to_string(),
                weights_version: self.weights_version.clone(),
                usage: Some(UsageOut { input_tokens: usage_in, output_tokens: usage_out }),
                filtered: if p.filtered.is_empty() { None } else { Some(p.filtered.clone()) },
                degraded_reason: None,
                partial_answers: if decoded.partial.is_empty() { None } else { Some(decoded.partial.clone()) },
                request_id: request_id.to_string(),
            },
        })
    }
}
```

**实现细节(必须按此处理)**:
- `remap_partial_id(pid, shard_start, shard_len)`:若形如 `c{n}.质量维度`(以 `c` 开且含 `.`),把 n 解析后 + `shard_start` 再加回(分片内 n < shard_len);共享问题 ID 原样返回(最后 dedup)。
- `top_legend(Option<&DimAnswer>) -> Option<(String, f32)>`:取 `probabilities` 中概率最大项 `(key, p)`;`legend.get(key)` 有则用标签,否则用 key;无概率时返回 `legend` 第一个或 None。
- `profile_view(&p, shared: Option<&SharedAnswers>)`:task_type/difficulty/frontier_needed 取 shared(降级/空结果传 None → 全 null),其余字段取 `p.profile`。
- `empty_response`:recommendations 空、degraded=false、meta.backend=self.backend.id()、usage=None、filtered 照带。
- `degraded_response(&p, err, latency_ms, request_id, top_k)`:
  - 对 `p.candidates`(全量)按 `coarse_map` 做 `fusion::cmp_rank(total, price, id)` 降序 → top_k;
  - 每条:`score=round3(total)`、`confidence=None`、`low_confidence=false`、
    `breakdown`:`quality_fit/cost_fit/weights/penalty=None`、`rule_hits` 照带、`coarse=Some(CoarseOut{ tag_overlap: round3(s.tag_overlap), price_score: round3(s.price_score) })`;
  - 理由:`ReasonCtx{ degraded:true, verdict:None, ... }` → 渲染器输出降级提示 + 硬约束行(spec §8.2 口径);
  - `profile` 判断字段全 null、`meta.usage=None`、`meta.degraded_reason=Some(err.degraded_reason())`。

测试要点(pipeline.rs 测试模块;需要 dev-dep `tokio`——已在 Task 1 加):
1. **happy path**(MockBackend + 仓库 configs):推荐数 = min(top_k, 候选数);rank 连续;score 降序;confidence Some;
   breakdown.weights 两值≈其和 1(尾差容差 1e-3);reason_hits 非空;reasons 非空且为 zh。
2. **确定性 tie-break**:Mock 对全体给同分 → 结果顺序 = 价格升序 → id 升序(手动核对仓库目录里预算/价格)。
3. **include_debug**:debug 含 questions_sent/answers_raw/state 三键;非 debug 响应无 debug 键。
4. **空结果**:`require_tags:["no_such_tag"]` → recommendations 空、meta.filtered 含 tag_require 计数、profile.task_type null、degraded false。
5. **降级**:FailBackend(返回 `JudgeError::Http{status:401,..}` 或 Timeout)→ degraded true、meta.degraded_reason="backend_4xx"/"backend_unavailable"、confidence 全 None、breakdown.coarse Some、profile 判断字段 null、reasons 首条含"未经语义判断"。
6. **include_debug + 后端错误** → `PipelineError::Upstream`。
7. **分片**:自定义 catalog(45 个同质模型)→ shard_requests.len()==2;第二片问题 ID 从 c0 重新计数;结果跨片全局有序;RecordingBackend 记录收到的请求用于断言(测试内实现,`Mutex<Vec<JudgeRequest>>`)。
8. **截断**:自定义 catalog(70 个模型)→ 总判断数 ≤60(两片 × ≤30),judge_indices.len()==60。
9. **partial**:RecordingBackend 变体故意不答 `c1.quality_fit` → 该候选 low_confidence=true、meta.partial_answers 含 "c1.quality_fit"。
10. **top_k / locale**:top_k=2 → 2 条;locale=en → reasons 含 "Quality fit"。
11. **非法输入**:top_k=0 → `PipelineError::InvalidConstraints`;空对象 request → `PipelineError::InvalidRequestBody`。

- [ ] **Step 5: 验证 + Commit**

```bash
cargo test -p recommender-core && cargo fmt --all
git add -A && git commit -m "feat(core): end-to-end pipeline with degrade and sharding"
```

---

## Task 15: recommender-systemone(/v1/systemone 客户端)

**Files:**
- Modify: `crates/systemone/src/lib.rs`(替换占位)
- Create: `crates/systemone/tests/contract.rs`

- [ ] **Step 1~4: 实现(含 wiremock 契约测试)**

`src/lib.rs`:

```rust
//! /v1/systemone 契约客户端(spec §7): 鉴权 / 代理 / 超时 / 错误映射。
//! 单次尝试;重试/总预算/信号量属 M2。

use futures::future::BoxFuture;
use recommender_core::judgment::{JudgeError, JudgeRequest, JudgeResponse, JudgmentBackend};
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct ClientConfig {
    pub id: String,
    pub base_url: String,
    pub model: String,
    pub api_key: Option<String>,
    pub proxy: Option<String>,
    pub timeout_ms: u64,
    pub long_context_header: bool,
}

impl ClientConfig {
    /// 显式配置优先;否则沿用环境变量(大小写各试)。spec §5.4。
    pub fn effective_proxy(&self) -> Option<String> {
        if let Some(p) = &self.proxy {
            if !p.trim().is_empty() { return Some(p.clone()); }
        }
        for k in ["HTTPS_PROXY", "https_proxy", "ALL_PROXY", "all_proxy"] {
            if let Ok(v) = std::env::var(k) {
                if !v.trim().is_empty() { return Some(v); }
            }
        }
        None
    }
}

pub struct SystemoneClient {
    cfg: ClientConfig,
    http: reqwest::Client,
}

impl SystemoneClient {
    pub fn new(cfg: ClientConfig) -> Result<Self, JudgeError> {
        let mut builder = reqwest::Client::builder()
            .connect_timeout(Duration::from_millis(1000))
            .timeout(Duration::from_millis(cfg.timeout_ms))
            .user_agent(concat!("model-recommender/", env!("CARGO_PKG_VERSION")));
        if let Some(proxy) = cfg.effective_proxy() {
            builder = builder.proxy(
                reqwest::Proxy::all(&proxy).map_err(|e| JudgeError::Connect(format!("代理配置无效: {e}")))?,
            );
        }
        let http = builder.build().map_err(|e| JudgeError::Connect(e.to_string()))?;
        Ok(Self { cfg, http })
    }

    pub fn model(&self) -> &str { &self.cfg.model }
}

impl JudgmentBackend for SystemoneClient {
    fn id(&self) -> &str { &self.cfg.id }

    fn judge<'a>(&'a self, mut req: JudgeRequest) -> BoxFuture<'a, Result<JudgeResponse, JudgeError>> {
        Box::pin(async move {
            let key = self.cfg.api_key.as_deref()
                .filter(|k| !k.trim().is_empty())
                .ok_or(JudgeError::MissingKey)?;
            req.model = self.cfg.model.clone();

            let url = format!("{}/v1/systemone", self.cfg.base_url.trim_end_matches('/'));
            let mut rb = self.http.post(&url).bearer_auth(key).json(&req);
            if self.cfg.long_context_header {
                rb = rb.header("TypeSafe-Danger-Zone-Long-Context", "true");
            }

            let resp = rb.send().await.map_err(|e| {
                if e.is_timeout() { JudgeError::Timeout } else { JudgeError::Connect(e.to_string()) }
            })?;
            let status = resp.status();
            if !status.is_success() {
                let body = resp.text().await.unwrap_or_default();
                return Err(JudgeError::Http { status: status.as_u16(), body: body.chars().take(500).collect() });
            }
            let bytes = resp.bytes().await.map_err(|e| JudgeError::Decode(e.to_string()))?;
            serde_json::from_slice::<JudgeResponse>(&bytes).map_err(|e| JudgeError::Decode(e.to_string()))
        })
    }
}
```

`tests/contract.rs` 测试要点(wiremock):
1. **请求形状**:起 MockServer;挂 `POST /v1/systemone` → 200 + 固定响应夹具。调 `judge` 后从 `server.received_requests()` 取请求体断言:`model == "jev-1.13.0"`(被客户端覆盖)、Bearer 头正确、`questions` 键正确、`state` 存在;响应 `answers`/`usage`/`model` 解析成功。
2. **HTTP 错误映射**:响应 451 → `Err(Http{status:451, ..})`;429 → `Err(Http{status:429, ..})`;401 → `Err(Http{status:401, ..})`。
3. **超时**:mock `set_delay(300ms)` + `timeout_ms:100` → `Err(Timeout)`。
4. **坏响应体**:200 + `set_body_string("not json")` → `Err(Decode)`。
5. **缺 key**:`api_key: None` → `Err(MissingKey)`。
6. **代理优先级**(纯函数测试,不需要网络):显式 proxy 非空 → 用它;显式空白 + 设 `HTTPS_PROXY` 环境变量 → 用环境变量(测试内 `std::env::set_var` 用唯一变量名避免并发影响,或直接构造两个 ClientConfig 断言 `effective_proxy`;串行测试用 `--test-threads=1` 不必要,避免并发 env 写用「仅显式」与「仅环境」两 case 分别断言)。
7. **请求携带 long_context_header**:`long_context_header:true` → received request headers 含 `TypeSafe-Danger-Zone-Long-Context: true`。

用例夹具构造 helper:

```rust
fn sample_request() -> JudgeRequest {
    // 1 个共享问题 + 1 个候选 × 2 维;state 用最小合法结构
}
```

- [ ] **Step 5: 验证 + Commit**

```bash
cargo test -p recommender-systemone && cargo fmt --all
git add -A && git commit -m "feat(systemone): /v1/systemone client with proxy and error mapping"
```

---

## Task 16: server::backends(注册表)

**Files:**
- Create: `crates/server/src/backends.rs`、`crates/server/src/lib.rs`(库目标骨架:仅 `pub mod backends;`,集成测试需要库目标)
- Modify: `crates/server/src/main.rs`(本任务不动;正式接线在 Task 17)

- [ ] **Step 1~4: 实现(含测试)**

```rust
//! backend 注册表(spec §4.3 D5/§5.4): kind → JudgmentBackend;列表首个为默认。
//! 缺 key 不阻止启动:调用时返回 MissingKey → 走降级(spec §9.6)。

use recommender_core::config::{BackendKind, BackendSpec};
use recommender_core::judgment::JudgmentBackend;
use recommender_core::mock::MockBackend;
use recommender_systemone::{ClientConfig, SystemoneClient};
use std::sync::Arc;

pub struct BuiltBackends {
    pub backends: Vec<Arc<dyn JudgmentBackend>>,
    /// 与 backends 对齐的 model 提示(dry-run 发送的 model 字段)
    pub model_hints: Vec<String>,
}

impl BuiltBackends {
    pub fn default_backend(&self) -> Arc<dyn JudgmentBackend> { self.backends[0].clone() }
    pub fn default_model_hint(&self) -> &str { &self.model_hints[0] }
}

pub fn build_backends(specs: &[BackendSpec]) -> Result<BuiltBackends, String> {
    let mut backends: Vec<Arc<dyn JudgmentBackend>> = vec![];
    let mut model_hints = vec![];
    for s in specs {
        match s.kind {
            BackendKind::Systemone => {
                let api_key = s.api_key_env.as_deref().and_then(|n| std::env::var(n).ok());
                let client = SystemoneClient::new(ClientConfig {
                    id: s.id.clone(),
                    base_url: s.base_url.clone().unwrap_or_default(),
                    model: s.model.clone().unwrap_or_default(),
                    api_key,
                    proxy: s.proxy.clone(),
                    timeout_ms: s.timeout_ms,
                    long_context_header: s.long_context_header,
                }).map_err(|e| format!("backend `{}` 初始化失败: {e}", s.id))?;
                model_hints.push(client.model().to_string());
                backends.push(Arc::new(client));
            }
            BackendKind::Mock => {
                model_hints.push(format!("{}-mock", s.id));
                backends.push(Arc::new(MockBackend::new(s.id.clone())));
            }
        }
    }
    if backends.is_empty() {
        return Err("至少需要配置一个 backend".into());
    }
    Ok(BuiltBackends { backends, model_hints })
}

#[cfg(test)]
mod tests {
    use super::*;
    use recommender_core::judgment::{JudgeError, JudgeRequest};

    fn systemone_spec(id: &str, api_key_env: &str) -> BackendSpec {
        BackendSpec {
            id: id.into(),
            kind: BackendKind::Systemone,
            base_url: Some("http://127.0.0.1:1".into()), // 不会被真正访问(或缺 key 提前失败)
            model: Some("jev-1.13.0".into()),
            api_key_env: Some(api_key_env.into()),
            proxy: None,
            timeout_ms: 5000,
            total_budget_ms: 8000,
            max_inflight: 16,
            retry: recommender_core::config::RetrySpec::default(),
            long_context_header: false,
        }
    }

    fn mock_spec(id: &str) -> BackendSpec {
        BackendSpec {
            id: id.into(),
            kind: BackendKind::Mock,
            base_url: None,
            model: None,
            api_key_env: None,
            proxy: None,
            timeout_ms: 5000,
            total_budget_ms: 8000,
            max_inflight: 16,
            retry: recommender_core::config::RetrySpec::default(),
            long_context_header: false,
        }
    }

    #[tokio::test]
    async fn mock_kind_builds_and_judges() {
        let built = build_backends(&[mock_spec("mock")]).unwrap();
        assert_eq!(built.backends.len(), 1);
        assert_eq!(built.default_backend().id(), "mock");
        let resp = built.default_backend().judge(JudgeRequest {
            model: String::new(),
            state: serde_json::json!({}),
            questions: Default::default(),
        }).await.unwrap();
        assert_eq!(resp.model, "mock-1");
    }

    #[tokio::test]
    async fn systemone_without_key_still_builds_but_judge_errors() {
        let env_name = format!("TEST_KEY_UNSET_{}", uuid::Uuid::new_v4().simple());
        let built = build_backends(&[systemone_spec("jev", &env_name)]).unwrap();
        let err = built.default_backend().judge(JudgeRequest {
            model: String::new(),
            state: serde_json::json!({}),
            questions: Default::default(),
        }).await.unwrap_err();
        assert!(matches!(err, JudgeError::MissingKey));
    }

    #[tokio::test]
    async fn empty_list_rejected() {
        assert!(build_backends(&[]).is_err());
    }
}
```

> 测试里构造 `BackendSpec` 直接写结构体字面量(全部 pub 字段);env 相关用例用**唯一环境变量名**(如 `TEST_KEY_UNSET_<uuid>`),避免并发测试互踩。

- [ ] **Step 5: 验证 + Commit**

```bash
cargo test -p recommender-server && cargo fmt --all
git add -A && git commit -m "feat(server): judgment backend registry"
```

---

## Task 17: server(axum 路由 / 鉴权 / 错误信封)

**Files:**
- Create: `crates/server/src/app.rs`、`crates/server/src/handlers.rs`、`crates/server/src/error.rs`、`crates/server/tests/api.rs`
- Modify: `crates/server/src/lib.rs`(Task 16 已建骨架,加入 `pub mod app; pub mod error; pub mod handlers;`)、`crates/server/src/main.rs`(替换占位为正式入口,改为使用库目标)

- [ ] **Step 1~4: 实现(含 oneshot 路由测试)**

`src/error.rs`:

```rust
//! 错误信封(spec §8.2): 所有非 2xx → {"error","message","request_id"}。

use axum::extract::rejection::JsonRejection;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use recommender_core::api::ApiErrorCode;
use serde_json::json;

#[derive(Debug)]
pub struct ApiError {
    pub status: StatusCode,
    pub code: &'static str,
    pub message: String,
    pub request_id: Option<String>,
}

impl ApiError {
    pub fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self { status, code, message: message.into(), request_id: None }
    }
    pub fn with_request_id(mut self, id: impl Into<String>) -> Self {
        self.request_id = Some(id.into());
        self
    }
    pub fn invalid_json(msg: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, ApiErrorCode::InvalidJson.as_str(), msg)
    }
    pub fn invalid_request_body(msg: impl Into<String>) -> Self {
        Self::new(StatusCode::UNPROCESSABLE_ENTITY, ApiErrorCode::InvalidRequestBody.as_str(), msg)
    }
    pub fn invalid_constraints(msg: impl Into<String>) -> Self {
        Self::new(StatusCode::UNPROCESSABLE_ENTITY, ApiErrorCode::InvalidConstraints.as_str(), msg)
    }
    pub fn upstream(msg: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_GATEWAY, ApiErrorCode::UpstreamError.as_str(), msg)
    }
    pub fn internal(msg: impl Into<String>) -> Self {
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, ApiErrorCode::InternalError.as_str(), msg)
    }
    pub fn unauthorized() -> Self {
        Self::new(StatusCode::UNAUTHORIZED, "unauthorized", "缺少或无效的 Authorization: Bearer <token>")
    }
    pub fn request_too_large() -> Self {
        Self::new(StatusCode::PAYLOAD_TOO_LARGE, ApiErrorCode::RequestTooLarge.as_str(), "请求体超过 max_request_bytes")
    }

    pub fn from_json_rejection(rej: JsonRejection) -> Self {
        match rej {
            JsonRejection::JsonSyntaxError(e) => Self::invalid_json(e.body_text()),
            JsonRejection::JsonDataError(e) => {
                // 借用 serde 错误消息里的字段路径区分：constraints/options → invalid_constraints(spec §8.2)
                let msg = e.body_text();
                if msg.contains("constraints") || msg.contains("options") {
                    Self::invalid_constraints(msg)
                } else {
                    Self::invalid_request_body(msg)
                }
            }
            JsonRejection::MissingJsonContentType(e) => Self::invalid_json(e.body_text()),
            JsonRejection::BytesRejection(e) => {
                let msg = e.body_text();
                if msg.to_lowercase().contains("length limit") {
                    Self::request_too_large()
                } else {
                    Self::invalid_json(msg)
                }
            }
            other => Self::invalid_json(other.body_text()),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let request_id = self.request_id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        (self.status, Json(json!({
            "error": self.code,
            "message": self.message,
            "request_id": request_id,
        }))).into_response()
    }
}
```

`src/app.rs`:

```rust
//! Router 组装(spec §8.3): 路由 + 鉴权中间件 + 请求体上限。

use crate::handlers;
use recommender_core::config::ServerOpts;
use recommender_core::pipeline::Pipeline;
use std::sync::Arc;
use axum::extract::DefaultBodyLimit;
use axum::routing::{get, post};
use axum::{middleware, Router};

#[derive(Clone)]
pub struct AppState {
    pub opts: Arc<ServerOpts>,
    pub pipeline: Arc<Pipeline>,
}

impl AppState {
    pub fn new(opts: ServerOpts, pipeline: Arc<Pipeline>) -> Self {
        Self { opts: Arc::new(opts), pipeline }
    }
}

pub fn build_router(state: AppState) -> Router {
    let client_auth = middleware::from_fn_with_state(state.clone(), handlers::client_auth);
    let admin_auth = middleware::from_fn_with_state(state.clone(), handlers::admin_auth);

    Router::new()
        .route("/healthz", get(handlers::healthz))
        .route("/readyz", get(handlers::readyz))
        .route("/v1/recommend", post(handlers::recommend).route_layer(client_auth))
        .route("/v1/catalog", get(handlers::catalog).route_layer(admin_auth))
        .layer(DefaultBodyLimit::max(state.opts.max_request_bytes))
        .with_state(state)
}
```

`src/handlers.rs`:

```rust
//! 端点实现(spec §8.1/§8.3)与鉴权中间件(spec §9.4)。

use crate::app::AppState;
use crate::error::ApiError;
use axum::extract::{ConnectInfo, FromRequest, Request, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::Json;
use recommender_core::api::{RecommendRequest, RecommendResponse};
use recommender_core::pipeline::PipelineError;
use serde::de::DeserializeOwned;
use serde_json::json;
use std::net::SocketAddr;

/// 自定义 JSON 提取器:把 axum 的拒绝映射为 spec §8.2 的错误信封。
pub struct AppJson<T>(pub T);

impl<S, T> FromRequest<S> for AppJson<T>
where
    S: Send + Sync,
    T: DeserializeOwned,
{
    type Rejection = ApiError;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        match Json::<T>::from_request(req, state).await {
            Ok(Json(v)) => Ok(AppJson(v)),
            Err(rej) => Err(ApiError::from_json_rejection(rej)),
        }
    }
}

const BEARER_PREFIX: &str = "Bearer ";

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

fn bearer_ok(headers: &HeaderMap, token: &str) -> bool {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|h| h.strip_prefix(BEARER_PREFIX))
        .is_some_and(|t| constant_time_eq(t.as_bytes(), token.as_bytes()))
}

fn token_from_env(env: &Option<String>) -> Option<String> {
    env.as_deref()
        .and_then(|name| std::env::var(name).ok())
        .filter(|v| !v.trim().is_empty())
}

fn is_loopback_peer(peer: &Option<ConnectInfo<SocketAddr>>) -> bool {
    match peer {
        Some(ConnectInfo(addr)) => addr.ip().is_loopback(),
        None => true, // 无 ConnectInfo(单测/特殊部署)→ 视为本机
    }
}

/// 调用方鉴权(spec §9.4): 配置了 token → 必须 Bearer;未配置 → 仅本机。
/// 例外:GET /debug 页面本身从本机访问时豁免(页面不含任何数据;其 fetch 仍需 token),见 spec §9.7。
pub async fn client_auth(
    State(state): State<AppState>,
    peer: Option<ConnectInfo<SocketAddr>>,
    headers: HeaderMap,
    req: Request,
    next: Next,
) -> Response {
    let loopback = is_loopback_peer(&peer);
    if req.uri().path() == "/debug" && loopback {
        return next.run(req).await;
    }
    match token_from_env(&state.opts.client_auth_token_env) {
        Some(token) => {
            if bearer_ok(&headers, &token) {
                next.run(req).await
            } else {
                ApiError::unauthorized().into_response()
            }
        }
        None => {
            if loopback {
                next.run(req).await
            } else {
                ApiError::unauthorized().into_response()
            }
        }
    }
}

/// 管理端点鉴权(spec §8.3 /v1/catalog): 配置了 admin token → Bearer;未配置 → 仅本机。
pub async fn admin_auth(
    State(state): State<AppState>,
    peer: Option<ConnectInfo<SocketAddr>>,
    headers: HeaderMap,
    req: Request,
    next: Next,
) -> Response {
    match token_from_env(&state.opts.admin_token_env) {
        Some(token) => {
            if bearer_ok(&headers, &token) {
                next.run(req).await
            } else {
                ApiError::unauthorized().into_response()
            }
        }
        None => {
            if is_loopback_peer(&peer) {
                next.run(req).await
            } else {
                ApiError::unauthorized().into_response()
            }
        }
    }
}

// ---- 端点 ----

pub async fn healthz() -> Json<serde_json::Value> {
    Json(json!({ "ok": true }))
}

pub async fn readyz(State(state): State<AppState>) -> Response {
    let ok = !state.pipeline.catalog.models().is_empty();
    if ok {
        (StatusCode::OK, Json(json!({ "ok": true, "models": state.pipeline.catalog.models().len() }))).into_response()
    } else {
        (StatusCode::SERVICE_UNAVAILABLE, Json(json!({ "ok": false }))).into_response()
    }
}

pub async fn catalog(State(state): State<AppState>) -> Json<serde_json::Value> {
    Json(json!({
        "version": state.pipeline.catalog.version(),
        "models": state.pipeline.catalog.models(),
    }))
}

pub async fn recommend(
    State(state): State<AppState>,
    AppJson(req): AppJson<RecommendRequest>,
) -> Result<Json<RecommendResponse>, ApiError> {
    let request_id = uuid::Uuid::new_v4().to_string();
    match state.pipeline.recommend(&req, &request_id).await {
        Ok(resp) => Ok(Json(resp)),
        Err(PipelineError::InvalidConstraints(msg)) => {
            Err(ApiError::invalid_constraints(msg).with_request_id(request_id))
        }
        Err(PipelineError::InvalidRequestBody(msg)) => {
            Err(ApiError::invalid_request_body(msg).with_request_id(request_id))
        }
        Err(PipelineError::Upstream(msg)) => Err(ApiError::upstream(msg).with_request_id(request_id)),
        Err(PipelineError::Internal(msg)) => Err(ApiError::internal(msg).with_request_id(request_id)),
    }
}

// (无其它辅助函数;测试直接经 `recommender_server::app::build_router` 组装)
```

`src/lib.rs`(在 Task 16 骨架基础上补齐):

```rust
//! recommender-server 库目标: 模块对外可见,供 main 与集成测试复用。
pub mod app;
pub mod backends;
pub mod error;
pub mod handlers;
```

`src/main.rs`(替换占位):

```rust
use std::path::PathBuf;
use std::sync::Arc;

use recommender_server::{app, backends};

fn configs_dir_from_args() -> PathBuf {
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        if a == "--configs" {
            if let Some(v) = args.next() {
                return PathBuf::from(v);
            }
        }
    }
    PathBuf::from("configs")
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let configs_dir = configs_dir_from_args();
    let configs = recommender_core::config::load_configs(&configs_dir)
        .map_err(|e| anyhow::anyhow!(e.to_string()))?;

    let built = backends::build_backends(&configs.server.backends).map_err(anyhow::Error::msg)?;
    let renderer = Arc::new(
        recommender_core::explain::TemplateReasonRenderer::from_dir(&PathBuf::from(&configs.server.templates_dir))
            .map_err(|e| anyhow::anyhow!(e.to_string()))?,
    );
    let pipeline = Arc::new(recommender_core::pipeline::Pipeline::new(
        &configs,
        Arc::new(configs.catalog.clone()),
        built.default_backend(),
        renderer,
        built.default_model_hint().to_string(),
    ));

    let listen = configs.server.server.listen.clone();
    let state = app::AppState::new(configs.server.server.clone(), pipeline);
    let router = app::build_router(state);

    let listener = tokio::net::TcpListener::bind(&listen).await?;
    tracing::info!("recommender-server listening on {listen}");
    axum::serve(
        listener,
        router.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .await?;
    Ok(())
}
```

`tests/api.rs` 测试要点(用 `tower::ServiceExt` oneshot + `axum::body::to_bytes`):

```rust
use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use recommender_core::config::{load_configs, ServerOpts};
use recommender_core::explain::TemplateReasonRenderer;
use recommender_core::mock::MockBackend;
use recommender_core::pipeline::Pipeline;
use recommender_server::app::{build_router, AppState};
use std::sync::Arc;
use tower::ServiceExt; // oneshot

fn repo_path(rel: &str) -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..").join(rel)
}

/// 用仓库 configs + MockBackend 组装测试 router;override 回调可改 ServerOpts(鉴权/体积上限/调试页开关)。
fn test_router(override_opts: impl FnOnce(&mut ServerOpts)) -> Router {
    let mut configs = load_configs(&repo_path("configs")).expect("仓库 configs 可加载");
    override_opts(&mut configs.server.server);
    let renderer = Arc::new(TemplateReasonRenderer::from_dir(&repo_path("templates")).unwrap());
    let pipeline = Arc::new(Pipeline::new(
        &configs,
        Arc::new(configs.catalog.clone()),
        Arc::new(MockBackend::new("mock")),
        renderer,
        "mock-1".to_string(),
    ));
    build_router(AppState::new(configs.server.server.clone(), pipeline))
}

/// 发一个请求并取 (status, body 的 JSON 解析结果)。
async fn call(app: Router, req: Request<Body>) -> (StatusCode, serde_json::Value) {
    let resp = app.oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let json = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
    (status, json)
}

fn json_req(method: &str, path: &str, body: &serde_json::Value, with_content_type: bool) -> Request<Body> {
    let mut b = Request::builder().method(method).uri(path);
    if with_content_type {
        b = b.header("content-type", "application/json");
    }
    b.body(Body::from(serde_json::to_vec(body).unwrap())).unwrap()
}

fn get_req(path: &str) -> Request<Body> {
    Request::builder().method("GET").uri(path).body(Body::empty()).unwrap()
}
```

测试用例:
1. `GET /healthz` → 200 `{"ok":true}`;`GET /readyz` → 200。
2. `POST /v1/recommend` 合法信封(内联小请求 `{"request":{"messages":[{"role":"user","content":"hi"}]}}`)→ 200;response.recommendations 非空、meta.request_id 非空、meta.catalog_version 非空。
3. content-type 缺失 → 400,body.error=="invalid_json"。
4. body 非 JSON → 400 invalid_json。
5. `"options":{"top_k":"abc"}` → 422 且 body.error=="invalid_constraints"(消息含 options)。
6. 超大 body:opts_override 把 `max_request_bytes` 设为 64 → 200 字节 body → 413 request_too_large。
7. 鉴权:opts_override + 唯一 env 名设置调用方 token(`std::env::set_var`):
   - 无 Authorization → 401 `unauthorized`;
   - 错误 token → 401;
   - 正确 Bearer → 200。
8. `/v1/catalog`:admin token 设置后无 token → 401;带 Bearer → 200 且 `version` 非空、`models` 是数组(本测试不设 token 的另一 case:无 admin token → 放行)。

- [ ] **Step 5: 验证 + Commit**

```bash
cargo test -p recommender-server && cargo fmt --all
git add -A && git commit -m "feat(server): axum routes with auth, error envelope and body limit"
```

---

## Task 18: /debug 内嵌调试页

**Files:**
- Create: `crates/server/assets/debug.html`
- Modify: `crates/server/src/app.rs`(加路由)、`crates/server/src/handlers.rs`(加 `debug_page`)、`crates/server/tests/api.rs`(补测试)

- [ ] **Step 1: 创建 `crates/server/assets/debug.html`(逐字;单文件、零外部依赖)**

```html
<!doctype html>
<html lang="zh">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Model Recommender Debug</title>
<style>
 body{font-family:system-ui,-apple-system,"PingFang SC",sans-serif;margin:24px;max-width:1080px}
 textarea{width:100%;height:220px;font-family:ui-monospace,Menlo,monospace;font-size:13px;box-sizing:border-box}
 .row{display:flex;gap:12px;flex-wrap:wrap;align-items:center;margin:8px 0}
 .card{border:1px solid #ddd;border-radius:8px;padding:12px;margin:8px 0}
 .meta{padding:8px 12px;border-radius:8px;background:#f5f5f5;margin-bottom:12px;font-size:13px}
 .degraded{background:#fff3e0}.err{background:#ffebee;color:#b71c1c}
 .badge{display:inline-block;padding:1px 8px;border-radius:10px;background:#e8eaf6;margin-left:6px;font-size:12px}
 pre{background:#0f172a;color:#e2e8f0;padding:12px;border-radius:8px;overflow:auto;font-size:12px}
 button{padding:8px 20px;border-radius:6px;border:1px solid #1976d2;background:#1976d2;color:#fff;cursor:pointer}
 button:disabled{opacity:.5}
 label{font-size:13px}
 input[type=text],input[type=number],input[type=password]{padding:4px 8px;border:1px solid #ccc;border-radius:6px}
</style>
</head>
<body>
<h1>Model Recommender 调试页</h1>
<div class="meta">
  <label>调用 token(未启用鉴权可留空;仅存本机 localStorage):
    <input type="password" id="token" style="width:280px"></label>
</div>
<div class="meta">
  <label>约束: top_k <input type="number" id="topk" value="5" min="1" max="50" style="width:64px">
  locale <select id="locale"><option value="zh">zh</option><option value="en">en</option></select>
  预算上限 $/M <input type="number" id="maxprice" step="0.1" style="width:80px" placeholder="不限">
  <label><input type="checkbox" id="costsensitive"> 成本敏感</label>
  require_tags <input type="text" id="reqtags" placeholder="如 tools,vision" style="width:140px">
  vendor_allow <input type="text" id="allow" placeholder="如 anthropic,openai" style="width:170px">
  vendor_deny <input type="text" id="deny" placeholder="如 gemini" style="width:110px">
  <label><input type="checkbox" id="debug"> include_debug</label></label>
</div>
<textarea id="req" spellcheck="false"></textarea>
<div class="row">
  <button id="go">推荐</button>
  <span id="status"></span>
</div>
<div id="out"></div>
<script>
const SAMPLE = { model: "gpt-5.4", messages: [ { role: "user", content: "帮我修复这段 Rust 代码的编译错误:\n```rust\nfn main(){ let x = 1 }\n```\n报错: error[E0308]" } ], max_tokens: 4096 };
document.getElementById("req").value = JSON.stringify(SAMPLE, null, 2);

const tokenInput = document.getElementById("token");
tokenInput.value = localStorage.getItem("rec_token") || "";
tokenInput.addEventListener("change", () => localStorage.setItem("rec_token", tokenInput.value));

function csv(id){ return document.getElementById(id).value.split(",").map(s => s.trim()).filter(Boolean); }
function escapeHtml(s){ return s.replace(/[&<>"']/g, c => ({"&":"&amp;","<":"&lt;",">":"&gt;",'"':"&quot;","'":"&#39;"}[c])); }

document.getElementById("go").addEventListener("click", async () => {
  const status = document.getElementById("status");
  const out = document.getElementById("out");
  status.textContent = "请求中…";
  out.innerHTML = "";
  let requestBody;
  try { requestBody = JSON.parse(document.getElementById("req").value); }
  catch (e) { status.textContent = "请求体不是合法 JSON: " + e.message; return; }

  const constraints = {};
  const maxp = document.getElementById("maxprice").value;
  const cs = document.getElementById("costsensitive").checked;
  if (maxp) constraints.budget = { max_input_price_per_mtok: Number(maxp), cost_sensitive: cs };
  else if (cs) constraints.budget = { cost_sensitive: true };
  const rt = csv("reqtags"); if (rt.length) constraints.require_tags = rt;
  const al = csv("allow"); if (al.length) constraints.vendor_allow = al;
  const dn = csv("deny"); if (dn.length) constraints.vendor_deny = dn;

  const options = { top_k: Number(document.getElementById("topk").value) || 5, locale: document.getElementById("locale").value };
  if (document.getElementById("debug").checked) options.include_debug = true;

  const headers = { "content-type": "application/json" };
  const tok = tokenInput.value.trim();
  if (tok) headers["authorization"] = "Bearer " + tok;

  const t0 = performance.now();
  try {
    const res = await fetch("/v1/recommend", { method: "POST", headers, body: JSON.stringify({ request: requestBody, constraints, options }) });
    const ms = Math.round(performance.now() - t0);
    const data = await res.json();
    if (!res.ok) {
      status.textContent = "错误 " + res.status;
      out.innerHTML = '<div class="card err"><b>' + escapeHtml(data.error || "error") + '</b>: ' + escapeHtml(data.message || "") + '<br><small>request_id: ' + escapeHtml(data.request_id || "") + '</small></div>';
      return;
    }
    status.textContent = "完成(" + ms + " ms)";
    render(data, ms);
  } catch (e) { status.textContent = "网络错误: " + e.message; }
});

function render(data, ms) {
  const out = document.getElementById("out");
  const m = data.meta || {};
  const banner = document.createElement("div");
  banner.className = "meta" + (data.degraded ? " degraded" : "");
  banner.innerHTML = 'backend <code>' + escapeHtml(String(m.backend ?? "-")) + '</code> · latency ' + (m.latency_ms ?? ms) + " ms · cache_hit " + m.cache_hit
    + " · catalog " + escapeHtml(String(m.catalog_version ?? "-"))
    + (data.degraded ? ' · <b>degraded: ' + escapeHtml(String(m.degraded_reason ?? "yes")) + "</b>" : "")
    + (m.partial_answers ? " · partial: " + escapeHtml(m.partial_answers.join(", ")) : "")
    + (m.filtered ? " · filtered: " + escapeHtml(m.filtered.map(f => f.rule + "×" + f.count).join(", ")) : "");
  out.appendChild(banner);

  if (data.profile) {
    const p = document.createElement("div");
    p.className = "meta";
    p.textContent = "profile: task=" + (data.profile.task_type ?? "-") + " difficulty=" + (data.profile.difficulty ?? "-")
      + " frontier=" + (data.profile.frontier_needed ?? "-") + " est_tokens=" + data.profile.est_tokens;
    out.appendChild(p);
  }

  (data.recommendations || []).forEach(r => {
    const bd = r.breakdown || {};
    const card = document.createElement("div");
    card.className = "card";
    card.innerHTML = "<b>#" + r.rank + " " + escapeHtml(r.model) + "</b>"
      + '<span class="badge">score ' + r.score + "</span>"
      + '<span class="badge">conf ' + (r.confidence === null ? "null" : r.confidence) + "</span>"
      + (r.low_confidence ? '<span class="badge">low_confidence</span>' : "")
      + '<div style="font-size:12px;color:#666;margin:6px 0">quality ' + (bd.quality_fit ?? "-") + " · cost " + (bd.cost_fit ?? "-") + " · penalty " + (bd.penalty ?? "-")
      + (bd.coarse ? " · coarse overlap " + bd.coarse.tag_overlap + " price " + bd.coarse.price_score : "")
      + " · hits: " + escapeHtml((bd.rule_hits || []).join(" ")) + "</div>"
      + "<ul>" + (r.reasons || []).map(x => "<li>" + escapeHtml(x) + "</li>").join("") + "</ul>";
    out.appendChild(card);
  });

  const toggle = document.createElement("button");
  toggle.textContent = "原始 JSON";
  const pre = document.createElement("pre");
  pre.style.display = "none";
  pre.textContent = JSON.stringify(data, null, 2);
  toggle.onclick = () => { pre.style.display = pre.style.display === "none" ? "block" : "none"; };
  out.appendChild(toggle);
  out.appendChild(pre);
}
</script>
</body>
</html>
```

- [ ] **Step 2: 接线(app.rs + handlers.rs)**

`app.rs` 的 `build_router` 增加(路由层应用同一 client_auth 中间件;页面 GET 的本机豁免在 `client_auth` 中已实现):

```rust
        .route("/debug", get(handlers::debug_page).route_layer(middleware::from_fn_with_state(state.clone(), handlers::client_auth)))
```

`handlers.rs` 追加:

```rust
/// 内嵌调试页(spec §9.7)。源码经 include_str! 编进二进制。
pub async fn debug_page(State(state): State<AppState>) -> Response {
    if !state.opts.debug_page_enabled {
        return StatusCode::NOT_FOUND.into_response();
    }
    (
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        include_str!("../assets/debug.html"),
    ).into_response()
}
```

- [ ] **Step 3: 测试(tests/api.rs 追加)**

1. `debug_page_enabled=true` → `GET /debug` 200、content-type 含 text/html、body 含 `Model Recommender Debug` 与 `/v1/recommend`;
2. opts_override 关掉 `debug_page_enabled` → 404。

- [ ] **Step 4: 验证 + Commit**

```bash
cargo test -p recommender-server && cargo fmt --all
git add -A && git commit -m "feat(server): embedded debug page"
```

---

## Task 19: recommender-cli(validate-catalog + dry-run)

**Files:**
- Modify: `crates/cli/src/main.rs`(替换 Task 1 占位)
- Create: `crates/cli/tests/cli.rs`

- [ ] **Step 1: 写失败测试(`crates/cli/tests/cli.rs`)**

测试用 `CARGO_BIN_EXE_recommender-cli` 黑盒调用二进制,并把 CWD 设为仓库根(与 README 的启动形态一致,`templates` 等 CWD 相对路径才能解析):

```rust
//! CLI 黑盒测试:CARGO_BIN_EXE 定位二进制;不依赖 examples/(Task 20 才创建)。
use std::path::{Path, PathBuf};
use std::process::Command;

fn repo(path: &str) -> PathBuf {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    if path.is_empty() { root } else { root.join(path) }
}

fn cli() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_recommender-cli"));
    c.current_dir(repo("")); // 与 README「仓库根启动」一致
    c
}

/// 最小请求信封(OpenAI 风格)写入临时文件。
fn write_min_request() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("mrc-cli-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join("req.json");
    std::fs::write(&p, r#"{"request":{"model":"ignored","messages":[{"role":"user","content":"帮我修复这段 Rust 代码的编译错误: error[E0308]"}]},"options":{"top_k":5}}"#).unwrap();
    p
}

#[test]
fn validate_catalog_ok_on_repo_configs() {
    let out = cli().args(["validate-catalog", "--configs", "configs"]).output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "exit={:?}\nstdout={stdout}", out.status.code());
    assert!(stdout.contains("收录 11"), "got: {stdout}");
    assert!(stdout.contains("排除 3"), "got: {stdout}");
    assert!(stdout.contains("mode=embedding×1"), "got: {stdout}");
    assert!(stdout.contains("依赖默认富化 0"), "got: {stdout}");
}

#[test]
fn validate_catalog_errors_exit_2() {
    let dir = std::env::temp_dir().join("mrc-cli-empty-configs");
    let _ = std::fs::create_dir_all(&dir);
    let out = cli().args(["validate-catalog", "--configs"]).arg(&dir).output().unwrap();
    assert_eq!(out.status.code(), Some(2), "stderr={}", String::from_utf8_lossy(&out.stderr));
    assert!(String::from_utf8_lossy(&out.stderr).contains("server.yaml"));
}

#[test]
fn dry_run_prints_all_questions() {
    let req = write_min_request();
    let out = cli().args(["dry-run", "--configs", "configs"])
        .arg("--request").arg(&req).output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "stderr={}", String::from_utf8_lossy(&out.stderr));
    assert!(stdout.contains("task_type") && stdout.contains("frontier_needed"), "got: {stdout}");
    assert!(stdout.contains("c10.quality_fit"), "11 个候选应全部入片;got: {stdout}");
    assert!(stdout.contains("request_excerpt"), "got: {stdout}");
}

#[test]
fn dry_run_raw_shard_is_curl_ready() {
    let req = write_min_request();
    let out = cli().args(["dry-run", "--configs", "configs"])
        .arg("--request").arg(&req).args(["--raw", "--shard", "0"]).output().unwrap();
    assert!(out.status.success());
    let v: serde_json::Value = serde_json::from_slice(&out.stdout)
        .expect("--raw 输出必须是合法 JSON(可直接 curl --data-binary @file)");
    assert!(v["questions"]["c0.quality_fit"].is_object());
    assert!(v["questions"]["c10.cost_fit"].is_object());
    assert_eq!(v["state"]["candidates"].as_array().unwrap().len(), 11);
    assert_eq!(v["model"], "jev-1.13.0"); // 与真实调用一致(server.yaml 首个 backend)
}

#[test]
fn dry_run_bad_shard_fails_with_1() {
    let req = write_min_request();
    let out = cli().args(["dry-run", "--configs", "configs"])
        .arg("--request").arg(&req).args(["--raw", "--shard", "9"]).output().unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("分片 9 不存在"));
}
```

- [ ] **Step 2: 跑测试确认失败**

```bash
cargo test -p recommender-cli   # 期望:子命令不存在(clap 解析失败 / 退出码非预期)
```

- [ ] **Step 3: 实现 `crates/cli/src/main.rs`(替换占位)**

```rust
//! recommender-cli:validate-catalog(配置/目录体检)与 dry-run(离线查看将发送的完整问题)。

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use recommender_core::api::RecommendRequest;
use recommender_core::catalog::{CatalogProvider, SkippedModel};
use recommender_core::config::{load_configs, BackendKind, ServerConfig};
use recommender_core::explain::TemplateReasonRenderer;
use recommender_core::judgment::JudgmentBackend;
use recommender_core::mock::MockBackend;
use recommender_core::pipeline::Pipeline;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

#[derive(Parser)]
#[command(name = "recommender-cli", version, about = "模型推荐服务 CLI(spec §9.6)")]
struct Cli {
    /// 配置目录(也可写绝对路径,server.yaml 内路径再另行解析)
    #[arg(long, global = true, default_value = "configs/")]
    configs: PathBuf,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// 校验配置与模型目录;通过退出 0,配置错误退出 2
    ValidateCatalog,
    /// 不联网:打印将发送给判断后端的完整问题
    DryRun {
        /// 推荐请求信封 JSON({"request":…,"constraints":…,"options":…})
        #[arg(long)]
        request: PathBuf,
        /// 只输出指定分片的原始请求 JSON(可直接 curl --data-binary 使用)
        #[arg(long)]
        raw: bool,
        /// 分片号(0 起;配合 --raw,默认 0)
        #[arg(long, default_value_t = 0)]
        shard: usize,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let is_validate = matches!(cli.cmd, Cmd::ValidateCatalog);
    match run(&cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("错误: {e:#}");
            // spec §5.1:validate-catalog 以退出码 2 表示配置错误;其他命令用 1
            ExitCode::from(if is_validate { 2 } else { 1 })
        }
    }
}

fn run(cli: &Cli) -> Result<()> {
    match &cli.cmd {
        Cmd::ValidateCatalog => validate_catalog(&cli.configs),
        Cmd::DryRun { request, raw, shard } => dry_run(&cli.configs, request, *raw, *shard),
    }
}

/// 体检报告(spec §5.1):收录 N / 排除 M(按原因分类)/ 依赖默认富化的 K 个 / 一致性警告。
fn validate_catalog(configs_dir: &Path) -> Result<()> {
    let cfg = load_configs(configs_dir)
        .with_context(|| format!("加载配置失败: {}", configs_dir.display()))?;
    let c = &cfg.catalog;

    println!("模型目录:{} 个模型(按 id 升序)· hash {}", c.models.len(), c.version_hash);
    println!(
        "收录 {} / 排除 {}(按原因:{})/ 依赖默认富化 {} / 一致性警告 {}",
        c.models.len(),
        c.skipped.len(),
        summarize_skipped(&c.skipped),
        // 口径与 catalog.rs 的警告措辞耦合(已有单测锁定该措辞)
        c.warnings.iter().filter(|w| w.contains("使用默认值")).count(),
        c.warnings.iter().filter(|w| !w.contains("使用默认值")).count(),
    );

    if !c.skipped.is_empty() {
        println!("\n排除清单:");
        for s in &c.skipped {
            println!("  - {}: {}", s.id, s.reason);
        }
    }
    if !c.warnings.is_empty() {
        println!("\n警告:");
        for w in &c.warnings {
            println!("  - {w}");
        }
    }
    println!("\n收录(前 5 条):");
    for m in c.models.iter().take(5) {
        println!("  - {} ({} · in ${}/M · ctx {})", m.id, m.vendor, m.price.in_per_mtok, m.context_window);
    }
    if c.models.len() > 5 {
        println!("  … 其余 {} 条见 GET /v1/catalog", c.models.len() - 5);
    }
    Ok(())
}

/// `SkippedModel.reason` 取 `(` 前首段归类:`mode=embedding`、`status=disabled`、`缺价格`。
fn summarize_skipped(skipped: &[SkippedModel]) -> String {
    let mut counts: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    for s in skipped {
        let key = s.reason.split('(').next().unwrap_or(&s.reason).trim().to_string();
        *counts.entry(key).or_insert(0) += 1;
    }
    if counts.is_empty() { return "无".into(); }
    counts.iter().map(|(k, n)| format!("{k}×{n}")).collect::<Vec<_>>().join("、")
}

/// 不联网:复用 `Pipeline::prepare`(与真实调用"发出去的请求零差异"),只打印不发送。
fn dry_run(configs_dir: &Path, request_path: &Path, raw: bool, shard: usize) -> Result<()> {
    let cfg = load_configs(configs_dir)
        .with_context(|| format!("加载配置失败: {}", configs_dir.display()))?;
    let catalog: Arc<dyn CatalogProvider> = Arc::new(cfg.catalog.clone());
    // templates_dir 与 server 主程序同为 CWD 相对(README 约定从仓库根启动)
    let renderer = TemplateReasonRenderer::from_dir(Path::new(&cfg.server.templates_dir))
        .with_context(|| format!("加载理由模板失败: {}", cfg.server.templates_dir))?;
    // MockBackend 仅为满足 Pipeline 装配;prepare 路径不会调用后端
    let backend: Arc<dyn JudgmentBackend> = Arc::new(MockBackend::new("mock"));
    let hint = model_hint(&cfg.server);
    let pipeline = Pipeline::new(&cfg, catalog, backend, Arc::new(renderer), hint.clone());

    let body = std::fs::read_to_string(request_path)
        .with_context(|| format!("读取请求失败: {}", request_path.display()))?;
    let req: RecommendRequest = serde_json::from_str(&body)
        .with_context(|| format!("请求信封 JSON 解析失败: {}", request_path.display()))?;
    let p = pipeline.prepare(&req).map_err(|e| anyhow::anyhow!("{e}"))?;

    if raw {
        let sr = p.shard_requests.get(shard)
            .with_context(|| format!("分片 {shard} 不存在(共 {} 片)", p.shard_requests.len()))?;
        println!("{}", serde_json::to_string_pretty(sr)?);
        return Ok(());
    }

    println!("catalog hash {} · 目录 {} 个模型", cfg.catalog.version_hash, cfg.catalog.models.len());
    println!("backend model(与真实调用一致): {}", if hint.is_empty() { "-" } else { &hint });
    println!(
        "画像: est_tokens={} · tools={} · images={} · json_mode={} · task_hints={:?}",
        p.profile.est_tokens, p.profile.tools_count, p.profile.has_images, p.profile.json_mode, p.profile.task_hints
    );
    println!(
        "候选 {} 个 → 判断 {} 个 · 分片 {} 个 · 规则命中 {} 类",
        p.candidates.len(), p.judge_indices.len(), p.shard_requests.len(), p.filtered.len()
    );
    for (i, sr) in p.shard_requests.iter().enumerate() {
        println!("\n===== 分片 {i}({} 个问题)=====", sr.questions.len());
        println!("{}", serde_json::to_string_pretty(sr)?);
    }
    Ok(())
}

/// 与 `server::backends` 的推导保持一致(首个 backend 为默认):
/// systemone → 配置的 model;mock → `{id}-mock`。
fn model_hint(server: &ServerConfig) -> String {
    match server.backends.first() {
        None => String::new(),
        Some(s) => match s.kind {
            BackendKind::Systemone => s.model.clone().unwrap_or_default(),
            BackendKind::Mock => format!("{}-mock", s.id),
        },
    }
}
```

- [ ] **Step 4: 验证 + Commit**

```bash
cargo test -p recommender-cli && cargo fmt --all
git add -A && git commit -m "feat(cli): validate-catalog and dry-run commands"
```

---

## Task 20: examples/sample-request.json + 录制夹具 + golden 全链路(insta)

**Files:**
- Create: `examples/sample-request.json`、`eval/fixtures/jev_code_debug.json`、`eval/fixtures/README.md`、`crates/core/tests/golden_e2e.rs`
- Modify: 根 `Cargo.toml`/`Cargo.lock`(insta 需启用 `json` feature 才能用 `assert_json_snapshot!`);快照放 `crates/core/tests/snapshots/`

- [ ] **Step 1: 写 `examples/sample-request.json`(逐字;可直接作为 POST /v1/recommend 的 body)**

```json
{
  "request": {
    "model": "gpt-5.4",
    "messages": [
      { "role": "system", "content": "You are a helpful coding assistant." },
      { "role": "user", "content": "帮我修复这段 Rust 代码的编译错误:\n```rust\nfn main() { let x = 1 }\n```\n报错: error[E0308] mismatched types" }
    ],
    "max_tokens": 4096
  },
  "constraints": {
    "require_tags": ["tools"],
    "budget": { "max_input_price_per_mtok": 3.0, "cost_sensitive": true }
  },
  "options": { "top_k": 5, "locale": "zh" }
}
```

- [ ] **Step 2: 录制夹具 `eval/fixtures/jev_code_debug.json`(优先真实;无 key/断网用合成 fallback)**

夹具 = `/v1/systemone` 的**原始响应体**(`{model, answers, usage}`),供 golden 测试回放。问的就是 dry-run 真发的那份请求(零差异)。

```bash
# ① 生成待发请求(Task 19 的 --raw;与真实调用零差异)
cargo run -p recommender-cli -- dry-run --configs configs/ \
  --request examples/sample-request.json --raw --shard 0 > /tmp/jev_request.json

# ② 走代理调用真实端点(区域限制 451;key 只从 .env 读,勿写入仓库)
set -a && . ./.env && set +a
curl -sS --proxy http://127.0.0.1:7897 \
  -H "authorization: Bearer ${TYPESAFE_API_KEY}" -H 'content-type: application/json' \
  --data-binary @/tmp/jev_request.json \
  https://api.typesafe.ai/v1/systemone > eval/fixtures/jev_code_debug.json

# ③ 校验:answers 覆盖请求里的全部问题 ID(缺一个 golden 测试都会红)
python3 - <<'EOF'
import json
req = json.load(open('/tmp/jev_request.json'))
resp = json.load(open('eval/fixtures/jev_code_debug.json'))
missing = [k for k in req['questions'] if k not in resp.get('answers', {})]
print('model:', resp.get('model'), '| answers:', len(resp.get('answers', {})), '| missing:', missing)
assert resp.get('answers'), 'empty answers — 检查 HTTP 状态/代理/key'
EOF
```

**合成 fallback(无 key/断网时)**:用同一份 `/tmp/jev_request.json` 造一份占位响应(结构合法、覆盖全部问题 ID;快照将标注 `synthetic-fixture`,重录后自动替换):

```bash
python3 - <<'EOF' > eval/fixtures/jev_code_debug.json
import json
req = json.load(open('/tmp/jev_request.json'))
ans = {}
for qid in req['questions']:
    if qid == 'task_type':
        ans[qid] = {"type":"choice","choice":"code_debug","probabilities":{"code_debug":0.66,"chat":0.24,"code_gen":0.10},"confidence":0.62}
    elif qid == 'difficulty':
        ans[qid] = {"type":"score","score":1.0,"legend":{"0":"简单","1":"中等","2":"困难"},"probabilities":{"0":0.25,"1":0.55,"2":0.20},"confidence":0.7}
    elif qid == 'frontier_needed':
        ans[qid] = {"type":"noul","noul":0.35}
    elif qid.endswith('.quality_fit'):
        ans[qid] = {"type":"score","score":1.5,"legend":{},"probabilities":{},"confidence":0.6}
    elif qid.endswith('.cost_fit'):
        ans[qid] = {"type":"score","score":1.0,"legend":{},"probabilities":{},"confidence":0.6}
print(json.dumps({"model":"synthetic-fixture","answers":ans,
                  "usage":{"input_tokens":len(req['questions'])*30,"output_tokens":len(req['questions'])*8}},
                 ensure_ascii=False, indent=2))
EOF
```

`eval/fixtures/README.md`(逐字):

```markdown
# fixtures

- `jev_code_debug.json`:`/v1/systemone` 原始响应(录制或合成),供 `crates/core/tests/golden_e2e.rs` 回放。
- 重录/合成命令见 `docs/superpowers/plans/2026-10-16-model-recommender-core.md` Task 20。
- **绑定关系**:夹具的问题 ID 集合由 `configs/model_list.yaml`(候选集)+ `configs/questions.yaml` 决定;
  目录或问题变更后 golden 测试会以"夹具缺答案: cN.xxx"报错 —— 重跑 Step 2 重录/重合成。
- 合成夹具以 `"model": "synthetic-fixture"` 区分;真实录制为端点返回的 model 名。
```

- [ ] **Step 3: 写 `crates/core/tests/golden_e2e.rs`(含测试)**

```rust
//! Golden 全链路(离线):固定目录(configs 示例)+ 固定响应(eval/fixtures)→ insta 快照。
//! 稳定化:request_id 固定、meta.latency_ms 归零;真实/合成夹具均不含任何 key。

use recommender_core::api::{Locale, RecommendRequest};
use recommender_core::catalog::CatalogProvider;
use recommender_core::config::load_configs;
use recommender_core::explain::TemplateReasonRenderer;
use recommender_core::judgment::{JudgeError, JudgeRequest, JudgeResponse, JudgmentBackend};
use recommender_core::pipeline::Pipeline;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// 回放夹具:对任何请求返回同一份响应;记录收到的请求用于"全覆盖"断言。
struct ReplayBackend {
    fixture: JudgeResponse,
    seen: Mutex<Vec<JudgeRequest>>,
}

impl JudgmentBackend for ReplayBackend {
    fn id(&self) -> &str { "replay" }
    fn judge<'a>(&'a self, req: JudgeRequest) -> futures::future::BoxFuture<'a, Result<JudgeResponse, JudgeError>> {
        Box::pin(async move {
            self.seen.lock().unwrap().push(req);
            Ok(self.fixture.clone())
        })
    }
}

async fn run_golden(locale: Locale) -> serde_json::Value {
    let root = repo_root();
    let cfg = load_configs(&root.join("configs")).expect("仓库 configs 必须可加载");

    let fixture_text = std::fs::read_to_string(root.join("eval/fixtures/jev_code_debug.json"))
        .expect("夹具缺失:见 eval/fixtures/README.md(Task 20 Step 2)");
    let fixture: JudgeResponse = serde_json::from_str(&fixture_text).expect("夹具必须是合法 JudgeResponse JSON");

    let backend = Arc::new(ReplayBackend { fixture: fixture.clone(), seen: Mutex::new(vec![]) });
    let renderer = TemplateReasonRenderer::from_dir(&root.join(&cfg.server.templates_dir))
        .expect("理由模板必须可加载");
    let catalog: Arc<dyn CatalogProvider> = Arc::new(cfg.catalog.clone());
    let pipeline = Pipeline::new(&cfg, catalog, backend.clone(), Arc::new(renderer), "jev-1.13.0".into());

    let mut req: RecommendRequest = serde_json::from_str(
        &std::fs::read_to_string(root.join("examples/sample-request.json")).expect("样例请求缺失"),
    ).expect("样例请求必须是合法信封");
    req.options.locale = locale;

    let resp = pipeline.recommend(&req, "golden-fixed-id").await.expect("recommend 不应失败");

    // 夹具必须覆盖管线实际发出的全部问题 ID(目录/问题变更 → 在此暴露,重录夹具)
    let seen = backend.seen.lock().unwrap();
    assert_eq!(seen.len(), 1, "示例目录(11 候选)应单片发送");
    for id in seen[0].questions.keys() {
        assert!(fixture.answers.contains_key(id), "夹具缺答案:{id}(重录或重新合成)");
    }
    drop(seen);

    let mut v = serde_json::to_value(&resp).unwrap();
    v["meta"]["latency_ms"] = serde_json::json!(0); // 只锁行为,不锁耗时
    v
}

#[tokio::test]
async fn golden_code_debug_zh() {
    let v = run_golden(Locale::Zh).await;
    assert_eq!(v["meta"]["request_id"], "golden-fixed-id");
    assert_eq!(v["degraded"], serde_json::json!(false));
    let recs = v["recommendations"].as_array().unwrap();
    assert_eq!(recs.len(), 5, "样例 11 候选 + top_k=5");
    assert!(!recs[0]["reasons"].as_array().unwrap().is_empty());
    insta::assert_json_snapshot!("golden_code_debug_zh", v);
}

#[tokio::test]
async fn golden_code_debug_en() {
    let v = run_golden(Locale::En).await;
    insta::assert_json_snapshot!("golden_code_debug_en", v);
}
```

- [ ] **Step 4: 生成/校验快照并提交**

```bash
# 首次生成快照(夹具确定 → 快照确定);随后人工检查 .snap 内容(排序/理由/breakdown 合理)
INSTA_UPDATE=always cargo test -p recommender-core --test golden_e2e
cargo test -p recommender-core --test golden_e2e   # 第二次:纯比对必须通过
cargo fmt --all
git add -A && git commit -m "test(core): golden e2e with recorded systemone fixture"
```

> 重录夹具后:删除 `crates/core/tests/snapshots/` 下对应 `.snap` 并以 `INSTA_UPDATE=always` 重新生成,再 diff 复核。

---

## Task 21: README + 真实联调走查 + 验收对照

**Files:**
- Create: `README.md`(仓库根;`docs/` 之外唯一面向用户的文档)

- [ ] **Step 1: 写 `README.md`(逐字,按本仓库实际行为核对后再提交)**

````markdown
# intelligent-model-router(模型推荐服务)

输入网关(OpenAI / Claude 风格)的原始请求体(POST body 的 `request` 字段),输出 top-K 模型推荐与中/英模板理由。
三层组合:代码硬规则过滤 + TypeSafe System One(Jev)语义判断 + 代码加权融合。

- 独立 HTTP 服务:只做推荐,不转发、不代调模型;
- 模型目录直接复用网关的 LiteLLM `model_list` 文件(启动时读取,价格随文件同步;改价/改目录后重启生效);推荐元数据(能力/上下文/标签)由 `enrichment.yaml` 富化,缺省走 defaults;
- 后端不可用时自动降级为确定性粗排(`degraded: true`),服务照常应答;
- 理由为模板渲染(zh/en),不产生额外 LLM 调用。

## 前置条件

- Rust ≥ 1.85(`cargo --version`);curl;
- 以下命令均在仓库根执行(理由模板 `templates_dir` 为 CWD 相对路径);
- 真实调用需要 `TYPESAFE_API_KEY`(写入 `.env`)。无 key/断网时服务仍可启动,请求自动降级。

## 快速开始(本地)

```bash
# 1. 构建
cargo build --release

# 2. 准备 key(仅需真实调用时;.env 已 gitignore,勿提交)
cp .env.example .env      # 然后填入 TYPESAFE_API_KEY

# 3. 校验配置与模型目录(退出码 0 = 通过,2 = 配置错误)
./target/release/recommender-cli validate-catalog --configs configs/

# 4. 离线查看"将发送给 Jev 的完整问题"(不联网)
./target/release/recommender-cli dry-run --configs configs/ --request examples/sample-request.json

# 5. 启动服务(key 只从环境变量读;直连 api.typesafe.ai 若报 451 需代理,见下)
set -a && . ./.env && set +a
./target/release/recommender-server --configs configs/
#   区域限制(451)时改用:HTTPS_PROXY=http://127.0.0.1:7897 ./target/release/recommender-server --configs configs/
#   也可在 configs/server.yaml 的 backends[].proxy 固定代理

# 6. 端到端拿 top5 + 理由(或浏览器打开 http://127.0.0.1:8787/debug)
curl -s http://127.0.0.1:8787/v1/recommend \
     -H 'content-type: application/json' -d @examples/sample-request.json
```

> 无 key / 断网也能跑通:Jev 调用失败时按降级路径返回粗排结果(`degraded: true`)。
> 端口冲突:改 `configs/server.yaml` 的 `server.listen`。
> 若设置了 `RECOMMENDER_AUTH_TOKEN`,上面的 curl 还需加 `-H 'authorization: Bearer <token>'`(见「鉴权」)。

## 指向你的真实网关模型清单

仓库内 `configs/model_list.yaml` 是**脱敏示例**(不含 api_base/api_key/内网主机名)。

1. 把 `configs/server.yaml` 的 `catalog.model_list_path` 改成网关 LiteLLM 配置的绝对路径;
2. 重跑 `./target/release/recommender-cli validate-catalog --configs configs/`,按「排除/依赖默认富化/警告」清单补齐 `configs/enrichment.yaml`(vendor/能力/上下文/标签;模板驱动的理由质量与之正相关);
3. 需要独立维护富化文件时,把 `catalog.enrichment_path` 一并指向你的路径。

## 配置(五份 + 模板)

| 文件 | 作用 |
|---|---|
| `configs/model_list.yaml` | 模型清单(与网关 LiteLLM `model_list` 同构;同名多部署自动合并,价格取首条;示例为脱敏子集) |
| `configs/enrichment.yaml` | 推荐元数据富化(全部可选;字段级覆盖 defaults) |
| `configs/rules.yaml` | 硬规则:能力 / 上下文 / 预算 / 厂商 / tag |
| `configs/questions.yaml` | 提问与语义判断配置(档位、权重、置信度门槛、价格带) |
| `configs/server.yaml` | 监听、鉴权、backends(契约钉死 `jev-1.13.0`)、catalog 路径、请求体/候选限额;缓存/重试/指标为 M2 预留 |
| `templates/reason.{zh,en}.toml` | 理由模板(zh/en) |

## 端点

| 端点 | 说明 |
|---|---|
| `POST /v1/recommend` | 推荐主接口;body `{request, constraints, options}`(内层 `request` 为网关原始请求体) |
| `GET /v1/catalog` | 当前模型目录(admin token;未配置时仅本机可访问) |
| `GET /healthz` | 存活探针 |
| `GET /readyz` | 就绪探针:目录非空即 200,否则 503;不探测 Jev 连通性/密钥(缺 key 时仍 ready,但请求走降级) |
| `GET /debug` | 内嵌调试页(可由 `debug_page_enabled` 关闭) |

## 鉴权

- 配置了 `client_auth_token_env`(默认 `RECOMMENDER_AUTH_TOKEN`)→ `POST /v1/recommend` **必须** `Authorization: Bearer <token>`(本机来源也不例外);
- 未配置该环境变量 → 仅本机(loopback)可访问,非本机 401;
- `GET /debug` 页面本身对本机豁免(页面不含数据;页面里的 fetch 仍需 token);
- `GET /v1/catalog` 使用 `admin_token_env`(默认 `RECOMMENDER_ADMIN_TOKEN`);未配置时仅本机可访问;
- 部署在同机反向代理之后时,所有来源都显示为本机 —— 必须配置 token。

## 数据与隐私

判断请求会发送到 `api.typesafe.ai`:清洗/截断后的请求摘录(`request_excerpt`,头 2000/尾 500 字符)、候选目录元数据与问题定义;判断内容不含 API key(密钥仅用于请求鉴权)。敏感数据场景请使用离线降级路径,或等待 v1.1 的本地 Laya 后端。

## 测试

```bash
cargo test                                          # 单元 + 契约(wiremock)+ 服务 + CLI + golden
cargo test -p recommender-core --test golden_e2e    # golden 全链路(离线夹具回放)
```

Golden 夹具:`eval/fixtures/jev_code_debug.json`(录制/合成与重录步骤见 [`eval/fixtures/README.md`](eval/fixtures/README.md))。

## 目录结构

```
crates/core         # 纯逻辑:画像/规则/提问/融合/理由/pipeline
crates/systemone    # /v1/systemone 契约客户端
crates/server       # axum 服务 + 内嵌 /debug
crates/cli          # validate-catalog / dry-run
configs/            # 五份配置
templates/          # 理由模板(zh/en)
examples/           # 样例请求(sample-request.json)
eval/fixtures/      # golden 夹具
docs/               # 设计文档与实施计划
```

## 范围与路线

- 本版 = 设计文档 §11 的 M0 骨架 + M1 核心;
- M2(另写计划):moka 缓存、完整重试/退避/总预算/信号量、指标与审计日志、`eval` 工具(≥30 golden 场景与指标)、Docker;
- M3(按需):new-api 旁路接入示例、shadow 回放;
- v1.1+:本地 Laya 后端、学习型粗排等见设计文档 §13。

设计文档:[`docs/superpowers/specs/2026-10-16-model-recommender-design.md`](docs/superpowers/specs/2026-10-16-model-recommender-design.md)
实施计划:[`docs/superpowers/plans/2026-10-16-model-recommender-core.md`](docs/superpowers/plans/2026-10-16-model-recommender-core.md)
````

- [ ] **Step 2: 真实联调走查(需 `.env` key + 本机代理;失败不阻塞——记录现象与降级行为)**

| # | 命令 / 动作 | 期望 |
|---|---|---|
| 1 | `cargo run -p recommender-cli -- validate-catalog --configs configs/` | 退出码 0;「收录 11 / 排除 3(mode=…×3)/ 依赖默认富化 0 / 一致性警告 0」 |
| 2 | `cargo run -p recommender-cli -- dry-run --configs configs/ --request examples/sample-request.json` | 1 个分片、25 个问题(共享 3 + 11×2);state 含 `request_excerpt` |
| 3 | 启动 server(`HTTPS_PROXY=http://127.0.0.1:7897`,仓库根执行) | 日志出现监听地址;`curl /healthz` 返回 `{"ok":true,…}` |
| 4 | `curl /v1/recommend -d @examples/sample-request.json` | HTTP 200;`recommendations` 5 条;`degraded:false`;reasons 为中文;meta 含 catalog/weights 版本与 usage |
| 5 | 浏览器 `http://127.0.0.1:8787/debug` → 推荐 | 与 #4 同构;卡片显示 rank/score/confidence/reasons/breakdown |

异常路径抽查(每项一行记录即可):

- 去掉 `HTTPS_PROXY` 或断网再请求 → `degraded:true`,`meta.degraded_reason` 为 `backend_4xx`(451)或 `backend_unavailable`,recommendations 仍非空(粗排);
- `constraints.require_tags:["no_such_tag"]` → `recommendations:[]`、`meta.filtered` 出现 `tag_require` 计数;
- `options.include_debug:true` → 响应含 `debug.questions_sent`(与 dry-run 输出一致)。

若本机环境不具备(无 key / 代理不可用):以 golden 测试(Task 20)与降级路径演练替代,并在提交信息中注明"真实联调未执行"。

- [ ] **Step 3: 验收对照(spec §11 里程碑 / §10 验收标准)**

| 项 | 状态 | 证据 |
|---|---|---|
| M0:`dry-run` 对样例请求打印完整待发问题(不联网) | ✅ | Task 19 测试 + 走查 #2 |
| M1:`curl` 或 `/debug` 端到端出 top5 + 理由 | ✅ | Task 15–18 + 走查 #3–#5 |
| M1:golden fixtures 通过 | ✅ | Task 20(`cargo test -p recommender-core --test golden_e2e`) |
| §10-4:全部单测/契约/golden 通过;`validate-catalog` 对配置 0 错误 | ✅ | 走查 #1 + 全量 `cargo test` |
| §10-1(eval ≥30 场景)/ §10-2(标注集召回 ≥90%)/ §10-3(p95 ≤1.2s) | ⏳ M2 | 见下方 M2 备注;本计划仅 1 个 golden 场景与联调观测延迟 |

- [ ] **Step 4: Commit**

```bash
git add -A && git commit -m "docs: quickstart readme and acceptance walkthrough"
```

---

## 计划完成定义(Done)

全部 Task(1–21)完成后,在仓库根执行:

```bash
cargo fmt --all --check
cargo test                                                        # 全部单元/契约/wiremock/api/golden/CLI
cargo run -p recommender-cli -- validate-catalog --configs configs/   # 退出码 0
cargo run -p recommender-cli -- dry-run --configs configs/ --request examples/sample-request.json
```

应满足:M0/M1 交付物齐备(见 Task 21 验收对照),golden 快照入库且可重复比对,`/debug` 与降级路径可用。

## M2 备注(本计划明确不含;由后续 M2 计划承接)

- 缓存(moka)、完整重试/退避/总预算/信号量语义(spec §8.2)、指标(`/metrics`)与审计日志(spec §9.5);
- `eval` 工具:场景 YAML、≥30 golden 场景、召回/MRR/延迟指标;校准与 shadow 回放(M3);
- Docker(可选)与运维文档;
- `catalog-init`(从真实 model_list 生成 enrichment 骨架)等目录维护便利工具;
- 已留接口面:`CacheConfig` / `LimitsConfig` / `RetrySpec` / `metrics_enabled` / 审计所需 meta 字段(catalog_version / weights_version / usage)。
