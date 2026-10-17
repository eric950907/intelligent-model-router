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
#[command(
    name = "recommender-cli",
    version,
    about = "模型推荐服务 CLI(spec §9.6)"
)]
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
        Cmd::DryRun {
            request,
            raw,
            shard,
        } => dry_run(&cli.configs, request, *raw, *shard),
    }
}

/// 体检报告(spec §5.1):收录 N / 排除 M(按原因分类)/ 依赖默认富化的 K 个 / 一致性警告。
fn validate_catalog(configs_dir: &Path) -> Result<()> {
    let cfg = load_configs(configs_dir)
        .with_context(|| format!("加载配置失败: {}", configs_dir.display()))?;
    let c = &cfg.catalog;

    println!(
        "模型目录:{} 个模型(按 id 升序)· hash {}",
        c.models.len(),
        c.version_hash
    );
    println!(
        "收录 {} / 排除 {}(按原因:{})/ 依赖默认富化 {} / 一致性警告 {}",
        c.models.len(),
        c.skipped.len(),
        summarize_skipped(&c.skipped),
        // 口径与 catalog.rs 的警告措辞耦合(已有单测锁定该措辞)
        c.warnings
            .iter()
            .filter(|w| w.contains("使用默认值"))
            .count(),
        c.warnings
            .iter()
            .filter(|w| !w.contains("使用默认值"))
            .count(),
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
        println!(
            "  - {} ({} · in ${}/M · ctx {})",
            m.id, m.vendor, m.price.in_per_mtok, m.context_window
        );
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
        let key = s
            .reason
            .split('(')
            .next()
            .unwrap_or(&s.reason)
            .trim()
            .to_string();
        *counts.entry(key).or_insert(0) += 1;
    }
    if counts.is_empty() {
        return "无".into();
    }
    counts
        .iter()
        .map(|(k, n)| format!("{k}×{n}"))
        .collect::<Vec<_>>()
        .join("、")
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
        let sr = p
            .shard_requests
            .get(shard)
            .with_context(|| format!("分片 {shard} 不存在(共 {} 片)", p.shard_requests.len()))?;
        println!("{}", serde_json::to_string_pretty(sr)?);
        return Ok(());
    }

    println!(
        "catalog hash {} · 目录 {} 个模型",
        cfg.catalog.version_hash,
        cfg.catalog.models.len()
    );
    println!(
        "backend model(与真实调用一致): {}",
        if hint.is_empty() { "-" } else { &hint }
    );
    println!(
        "画像: est_tokens={} · tools={} · images={} · json_mode={} · task_hints={:?}",
        p.profile.est_tokens,
        p.profile.tools_count,
        p.profile.has_images,
        p.profile.json_mode,
        p.profile.task_hints
    );
    println!(
        "候选 {} 个 → 判断 {} 个 · 分片 {} 个 · 规则命中 {} 类",
        p.candidates.len(),
        p.judge_indices.len(),
        p.shard_requests.len(),
        p.filtered.len()
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
