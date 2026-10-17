//! 流水线组装(spec §4.1/§8.2): prepare 纯逻辑可离线;recommend 执行判断 + 融合 + 降级。

use crate::api::*;
use crate::catalog::CatalogProvider;
use crate::coarse::{self, CoarseScore};
use crate::config::{LimitsConfig, LoadedConfigs, QuestionsConfig};
use crate::explain::{PenaltyView, ReasonCtx, ReasonRenderer, VerdictView};
use crate::fusion;
use crate::judgment::{
    decode_answers, DecodedAnswers, DimAnswer, JudgeError, JudgeRequest, JudgmentBackend,
    SharedAnswers,
};
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
                "confidence": serde_json::to_value(configs.questions.confidence).ok(),
                "price_bands": serde_json::to_value(configs.questions.price_bands).ok(),
            }))
            .expect("可序列化");
            blake3::hash(canonical.as_bytes()).to_hex().to_string()[..16].to_string()
        };
        let safety_factor = configs
            .rules
            .rules
            .iter()
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
#[derive(Debug)]
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

        let ctx = rules::RuleCtx {
            profile: &profile,
            constraints: &req.constraints,
            options: &req.options,
        };
        let report = rules::apply_rules(&self.rules, &ctx, self.catalog.models());

        // ★ 语义修正(spec §6.2 的 N = 硬规则过滤后的候选数,而非全目录):
        // 粗排分位若按全目录计算,过滤会改变截断边界上的相对顺序。
        let cand_models: Vec<crate::model::ModelEntry> =
            report.candidates.iter().map(|c| c.model.clone()).collect();
        let coarse_all = coarse::coarse_rank(&profile, &cand_models);
        let coarse_map: BTreeMap<String, CoarseScore> =
            coarse_all.iter().map(|(m, s)| (m.id.clone(), *s)).collect();

        let plan = questions::plan_shards(
            report.candidates.len(),
            self.limits.max_candidates_in_call,
            self.limits.max_candidates_judged,
        );

        // judge_indices: 未截断 → 原顺序;截断 → 粗排顺序取前 k
        let candidates = report.candidates;
        let judge_indices: Vec<usize> = if let Some(k) = plan.truncate_to {
            let mut idx: Vec<usize> = (0..candidates.len()).collect();
            idx.sort_by(|&a, &b| {
                let (ma, mb) = (&candidates[a].model, &candidates[b].model);
                let (sa, sb) = (coarse_map[&ma.id], coarse_map[&mb.id]);
                fusion::cmp_rank(
                    sa.total(),
                    ma.price.in_per_mtok,
                    &ma.id,
                    sb.total(),
                    mb.price.in_per_mtok,
                    &mb.id,
                )
            });
            idx.truncate(k);
            idx
        } else {
            (0..candidates.len()).collect()
        };

        // 分片请求:每片局部重编号(问题 ID c{local},spec §6.2/§6.3)
        let mut shard_requests = Vec::with_capacity(plan.shards.len());
        for range in &plan.shards {
            let views: Vec<CandidateView> = range
                .clone()
                .map(|pos| {
                    let gi = judge_indices[pos];
                    questions::candidate_view(
                        pos - range.start,
                        &candidates[gi].model,
                        &self.questions.price_bands,
                    )
                })
                .collect();
            shard_requests.push(questions::build_judge_request(
                &self.backend_model_hint,
                &profile,
                &req.constraints,
                &self.questions,
                &views,
            ));
        }

        // filtered 汇总:按首次命中规则计数
        let mut counts: BTreeMap<String, u32> = BTreeMap::new();
        for f in &report.filtered_by_rule {
            *counts.entry(f.rule.clone()).or_insert(0) += 1;
        }
        let filtered = counts
            .into_iter()
            .map(|(rule, count)| FilteredByRule { rule, count })
            .collect();

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
    pub async fn recommend(
        &self,
        req: &RecommendRequest,
        request_id: &str,
    ) -> Result<RecommendResponse, PipelineError> {
        let t0 = Instant::now();
        let p = self.prepare(req)?;
        let top_k = p
            .options
            .top_k
            .unwrap_or(self.limits.default_top_k)
            .min(MAX_TOP_K) as usize;

        if p.candidates.is_empty() {
            return Ok(self.empty_response(&p, t0.elapsed().as_millis() as u64, request_id));
        }

        let futures_vec: Vec<_> = p
            .shard_requests
            .iter()
            .cloned()
            .map(|r| self.backend.judge(r))
            .collect();
        let results = futures::future::join_all(futures_vec).await;

        let mut responses = Vec::with_capacity(results.len());
        for r in results {
            match r {
                Ok(resp) => responses.push(resp),
                Err(e) => {
                    if p.options.include_debug {
                        return Err(PipelineError::Upstream(e.to_string()));
                    }
                    return Ok(self.degraded_response(
                        &p,
                        &e,
                        t0.elapsed().as_millis() as u64,
                        request_id,
                        top_k,
                    ));
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
            if actual_model.is_empty() {
                actual_model = resp.model.clone();
            }
            if let Some(u) = &resp.usage {
                usage_in += u.input_tokens;
                usage_out += u.output_tokens;
            }
            let range = &p.shard_ranges[shard];
            let local = decode_answers(resp, range.len());
            // 共享答案跨片容错合并(spec §8.2):任一分片给出即采用(先到先得);
            // 只信 shard 0 会丢失其他片的有效答案。
            let shared = local.shared;
            if decoded.shared.task_type.is_none() {
                decoded.shared.task_type = shared.task_type;
            }
            if decoded.shared.difficulty.is_none() {
                decoded.shared.difficulty = shared.difficulty;
            }
            if decoded.shared.frontier_needed.is_none() {
                decoded.shared.frontier_needed = shared.frontier_needed;
            }
            let local_len = local.per_candidate.len();
            for (li, ca) in local.per_candidate.into_iter().enumerate() {
                let gi = range.start + li;
                if gi < decoded.per_candidate.len() {
                    decoded.per_candidate[gi] = ca;
                }
            }
            for pid in local.partial {
                decoded
                    .partial
                    .push(remap_partial_id(&pid, range.start, local_len));
            }
        }
        // 某片上报的共享问题 partial 若已被其他片回答,则不算缺失。
        decoded.partial.retain(|id| match id.as_str() {
            "task_type" => decoded.shared.task_type.is_none(),
            "difficulty" => decoded.shared.difficulty.is_none(),
            "frontier_needed" => decoded.shared.frontier_needed.is_none(),
            _ => true,
        });
        decoded.partial.sort();
        decoded.partial.dedup();

        // 融合 + 排序(全局)
        let weights = fusion::compute_weights(&self.questions, &decoded.shared, &p.constraints);
        let q_levels = self.questions.quality_levels();
        let c_levels = self.questions.cost_levels();

        // 行: (judge_indices 位置, FusedScore)
        let mut rows: Vec<(usize, fusion::FusedScore)> = (0..p.judge_indices.len())
            .map(|pos| {
                let cand = &p.candidates[p.judge_indices[pos]];
                let ca = &decoded.per_candidate[pos];
                let fs = fusion::fuse_candidate(
                    &weights,
                    ca.quality.as_ref(),
                    ca.cost.as_ref(),
                    q_levels,
                    c_levels,
                    cand.penalties,
                );
                (pos, fs)
            })
            .collect();

        rows.sort_by(|a, b| {
            let (ca, cb) = (
                &p.candidates[p.judge_indices[a.0]],
                &p.candidates[p.judge_indices[b.0]],
            );
            fusion::cmp_rank(
                a.1.score,
                ca.model.price.in_per_mtok,
                &ca.model.id,
                b.1.score,
                cb.model.price.in_per_mtok,
                &cb.model.id,
            )
        });

        let recommendations: Vec<Recommendation> = rows
            .iter()
            .take(top_k)
            .enumerate()
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
                let penalties_view: Vec<PenaltyView> = cand
                    .penalty_records
                    .iter()
                    .map(|pr| PenaltyView {
                        value: pr.value,
                        reason_key: pr.reason_key.clone(),
                    })
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
                        weights: Some(WeightsOut {
                            quality_fit: weights.quality_fit,
                            cost_fit: weights.cost_fit,
                        }),
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
        } else {
            None
        };

        Ok(RecommendResponse {
            recommendations,
            profile: profile_view(&p, Some(&decoded.shared)),
            degraded: false,
            debug,
            meta: Meta {
                backend: if actual_model.is_empty() {
                    self.backend.id().to_string()
                } else {
                    actual_model
                },
                latency_ms: t0.elapsed().as_millis() as u64,
                cache_hit: false,
                catalog_version: self.catalog.version().to_string(),
                weights_version: self.weights_version.clone(),
                usage: Some(UsageOut {
                    input_tokens: usage_in,
                    output_tokens: usage_out,
                }),
                filtered: if p.filtered.is_empty() {
                    None
                } else {
                    Some(p.filtered.clone())
                },
                degraded_reason: None,
                partial_answers: if decoded.partial.is_empty() {
                    None
                } else {
                    Some(decoded.partial.clone())
                },
                request_id: request_id.to_string(),
            },
        })
    }

    /// 无候选:不调后端,meta.backend = 后端 id、usage 缺省(spec §8.2)。
    fn empty_response(&self, p: &Prepared, latency_ms: u64, request_id: &str) -> RecommendResponse {
        RecommendResponse {
            recommendations: vec![],
            profile: profile_view(p, None),
            degraded: false,
            debug: None,
            meta: Meta {
                backend: self.backend.id().to_string(),
                latency_ms,
                cache_hit: false,
                catalog_version: self.catalog.version().to_string(),
                weights_version: self.weights_version.clone(),
                usage: None,
                filtered: if p.filtered.is_empty() {
                    None
                } else {
                    Some(p.filtered.clone())
                },
                degraded_reason: None,
                partial_answers: None,
                request_id: request_id.to_string(),
            },
        }
    }

    /// 后端不可用:按粗排全量排序取 top_k,不调后端、不给语义理由(spec §8.2)。
    fn degraded_response(
        &self,
        p: &Prepared,
        err: &JudgeError,
        latency_ms: u64,
        request_id: &str,
        top_k: usize,
    ) -> RecommendResponse {
        let mut idx: Vec<usize> = (0..p.candidates.len()).collect();
        idx.sort_by(|&a, &b| {
            let (ma, mb) = (&p.candidates[a].model, &p.candidates[b].model);
            let (sa, sb) = (p.coarse_map[&ma.id], p.coarse_map[&mb.id]);
            fusion::cmp_rank(
                sa.total(),
                ma.price.in_per_mtok,
                &ma.id,
                sb.total(),
                mb.price.in_per_mtok,
                &mb.id,
            )
        });

        let recommendations: Vec<Recommendation> = idx
            .iter()
            .take(top_k)
            .enumerate()
            .map(|(rank0, i)| {
                let cand = &p.candidates[*i];
                let score = p.coarse_map[&cand.model.id];
                // 降级不应用软惩罚,也不渲染惩罚行(分数与理由口径一致)。
                let rctx = ReasonCtx {
                    rank: rank0 as u32 + 1,
                    model: &cand.model,
                    profile: &p.profile,
                    safety_factor: self.safety_factor,
                    rule_hits: &cand.rule_hits,
                    penalties: &[],
                    degraded: true,
                    verdict: None,
                    budget_present: p.constraints.budget.is_some(),
                    price_bands: self.questions.price_bands,
                };
                Recommendation {
                    model: cand.model.id.clone(),
                    rank: rank0 as u32 + 1,
                    score: fusion::round3(score.total()),
                    confidence: None,
                    low_confidence: false,
                    breakdown: Breakdown {
                        quality_fit: None,
                        cost_fit: None,
                        weights: None,
                        penalty: None,
                        rule_hits: cand.rule_hits.clone(),
                        coarse: Some(CoarseOut {
                            tag_overlap: fusion::round3(score.tag_overlap),
                            price_score: fusion::round3(score.price_score),
                        }),
                    },
                    reasons: self.renderer.render(&rctx, p.options.locale),
                }
            })
            .collect();

        RecommendResponse {
            recommendations,
            profile: profile_view(p, None),
            degraded: true,
            debug: None,
            meta: Meta {
                backend: self.backend.id().to_string(),
                latency_ms,
                cache_hit: false,
                catalog_version: self.catalog.version().to_string(),
                weights_version: self.weights_version.clone(),
                usage: None,
                filtered: if p.filtered.is_empty() {
                    None
                } else {
                    Some(p.filtered.clone())
                },
                degraded_reason: Some(err.degraded_reason().to_string()),
                partial_answers: None,
                request_id: request_id.to_string(),
            },
        }
    }
}

/// 分片局部问题 ID → 全局 ID:仅重编 `c{n}.<dim>` 形式(共享问题原样,由调用方 dedup)。
fn remap_partial_id(pid: &str, shard_start: usize, shard_len: usize) -> String {
    let Some(rest) = pid.strip_prefix('c') else {
        return pid.to_string();
    };
    let Some(dot) = rest.find('.') else {
        return pid.to_string();
    };
    let Ok(n) = rest[..dot].parse::<usize>() else {
        return pid.to_string();
    };
    if n >= shard_len {
        return pid.to_string();
    }
    format!("c{}{}", n + shard_start, &rest[dot..])
}

/// 概率最高的档位:(标签, 概率);无概率时退回 legend 首项(概率记 0)。
fn top_legend(a: Option<&DimAnswer>) -> Option<(String, f32)> {
    let a = a?;
    if let Some((k, p)) = a
        .probabilities
        .iter()
        .max_by(|x, y| x.1.partial_cmp(y.1).unwrap_or(std::cmp::Ordering::Equal))
    {
        let label = a.legend.get(k).cloned().unwrap_or_else(|| k.clone());
        return Some((label, *p));
    }
    a.legend.iter().next().map(|(_, v)| (v.clone(), 0.0))
}

/// 响应 profile:判断字段取 shared(降级/空结果为 None → 全 null),其余取画像。
fn profile_view(p: &Prepared, shared: Option<&SharedAnswers>) -> ProfileView {
    ProfileView {
        task_type: shared.and_then(|s| s.task_type.clone()),
        difficulty: shared.and_then(|s| s.difficulty),
        frontier_needed: shared.and_then(|s| s.frontier_needed),
        est_tokens: p.profile.est_tokens,
        has_images: p.profile.has_images,
        tools_count: p.profile.tools_count,
        json_mode: p.profile.json_mode,
        locale_hint: p.profile.locale_hint.clone(),
        task_hints: p.profile.task_hints.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::CatalogProvider;
    use crate::config::load_configs;
    use crate::explain::TemplateReasonRenderer;
    use crate::judgment::JudgeResponse;
    use crate::mock::MockBackend;
    use crate::model::{Capabilities, ModelEntry, ModelStatus, Price};
    use futures::future::BoxFuture;
    use serde_json::json;
    use std::path::Path;
    use std::sync::Mutex;

    // ---------- 测试基建 ----------

    fn configs() -> crate::config::LoadedConfigs {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../configs");
        load_configs(&dir).expect("仓库 configs/ 必须可加载")
    }

    fn renderer() -> Arc<dyn ReasonRenderer> {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../templates");
        Arc::new(TemplateReasonRenderer::from_dir(&dir).expect("仓库 templates/ 必须可加载"))
    }

    fn pipeline_with(backend: Arc<dyn JudgmentBackend>) -> Pipeline {
        let configs = configs();
        let catalog: Arc<dyn CatalogProvider> = Arc::new(configs.catalog.clone());
        Pipeline::new(&configs, catalog, backend, renderer(), "jev-test".into())
    }

    fn pipeline_with_catalog(
        backend: Arc<dyn JudgmentBackend>,
        catalog: Arc<dyn CatalogProvider>,
    ) -> Pipeline {
        let configs = configs();
        Pipeline::new(&configs, catalog, backend, renderer(), "jev-test".into())
    }

    fn req(v: serde_json::Value) -> RecommendRequest {
        serde_json::from_value(v).expect("测试请求必须可解析")
    }

    fn plain_request() -> RecommendRequest {
        req(json!({"request": {"messages": [{"role": "user", "content": "hello"}]}}))
    }

    fn mock_pipeline() -> Pipeline {
        pipeline_with(Arc::new(MockBackend::new("mock")))
    }

    struct TestCatalog {
        models: Vec<ModelEntry>,
        hash: String,
    }

    impl CatalogProvider for TestCatalog {
        fn models(&self) -> &[ModelEntry] {
            &self.models
        }
        fn version(&self) -> &str {
            &self.hash
        }
    }

    fn model_with(id: &str, price: f64, tags: &[&str]) -> ModelEntry {
        ModelEntry {
            id: id.into(),
            name: id.into(),
            vendor: "test".into(),
            description: "test model".into(),
            tags: tags.iter().map(|t| t.to_string()).collect(),
            capabilities: Capabilities::default(),
            context_window: 128_000,
            max_output: None,
            price: Price {
                in_per_mtok: price,
                out_per_mtok: price * 2.0,
            },
            groups: vec![],
            quality_notes: String::new(),
            status: ModelStatus::Enabled,
        }
    }

    /// n 个同质模型:m00..m{n-1},价格递增 0.01,无 tags(plain profile 全部通过硬规则)。
    fn catalog_of(n: usize) -> Arc<dyn CatalogProvider> {
        let models = (0..n)
            .map(|i| model_with(&format!("m{i:02}"), 1.0 + i as f64 * 0.01, &[]))
            .collect();
        Arc::new(TestCatalog {
            models,
            hash: format!("h{n}"),
        })
    }

    /// 记录收到的请求;可选地删掉某个局部问题 ID(模拟 partial 答案)。
    struct RecordingBackend {
        log: Mutex<Vec<JudgeRequest>>,
        drop_answer: Option<&'static str>,
        /// 仅当请求首候选 id 匹配时才丢弃答案(用于只影响某个分片);None = 每个分片都丢。
        drop_in_shard_starting_with: Option<&'static str>,
    }

    impl RecordingBackend {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                log: Mutex::new(vec![]),
                drop_answer: None,
                drop_in_shard_starting_with: None,
            })
        }
        fn dropping(id: &'static str) -> Arc<Self> {
            Arc::new(Self {
                log: Mutex::new(vec![]),
                drop_answer: Some(id),
                drop_in_shard_starting_with: None,
            })
        }
        fn dropping_in_shard(id: &'static str, first_candidate_id: &'static str) -> Arc<Self> {
            Arc::new(Self {
                log: Mutex::new(vec![]),
                drop_answer: Some(id),
                drop_in_shard_starting_with: Some(first_candidate_id),
            })
        }
        fn requests(&self) -> Vec<JudgeRequest> {
            self.log.lock().unwrap().clone()
        }
    }

    impl JudgmentBackend for RecordingBackend {
        fn id(&self) -> &str {
            "rec"
        }
        fn judge<'a>(
            &'a self,
            req: JudgeRequest,
        ) -> BoxFuture<'a, Result<JudgeResponse, JudgeError>> {
            self.log.lock().unwrap().push(req.clone());
            let in_target_shard = self
                .drop_in_shard_starting_with
                .is_none_or(|first| req.state["candidates"][0]["id"] == first);
            let drop_id = if in_target_shard {
                self.drop_answer
            } else {
                None
            };
            Box::pin(async move {
                let mock = MockBackend::new("rec");
                let mut resp = mock.judge(req).await?;
                if let Some(id) = drop_id {
                    resp.answers.remove(id);
                }
                Ok(resp)
            })
        }
    }

    struct FailBackend {
        error: JudgeError,
    }

    impl JudgmentBackend for FailBackend {
        fn id(&self) -> &str {
            "fail"
        }
        fn judge<'a>(
            &'a self,
            _req: JudgeRequest,
        ) -> BoxFuture<'a, Result<JudgeResponse, JudgeError>> {
            let e = self.error.clone();
            Box::pin(async move { Err(e) })
        }
    }

    fn fail_pipeline(error: JudgeError) -> Pipeline {
        pipeline_with(Arc::new(FailBackend { error }))
    }

    // ---------- 1. happy path ----------

    #[tokio::test]
    async fn happy_path_returns_ranked_recommendations() {
        let pipe = mock_pipeline();
        let p = pipe.prepare(&plain_request()).unwrap();
        let expected = p.candidates.len().min(pipe.limits.default_top_k as usize);

        let resp = pipe.recommend(&plain_request(), "req-1").await.unwrap();

        assert!(!resp.degraded);
        assert!(resp.debug.is_none());
        assert_eq!(resp.recommendations.len(), expected);
        for (i, r) in resp.recommendations.iter().enumerate() {
            assert_eq!(r.rank, i as u32 + 1, "rank 必须连续");
            assert!(r.confidence.is_some());
            assert!(!r.breakdown.rule_hits.is_empty());
            assert!(!r.reasons.is_empty());
            assert!(
                r.reasons.iter().any(|s| s.contains("硬约束")),
                "zh 理由: {:?}",
                r.reasons
            );
            let w = r.breakdown.weights.expect("weights");
            let sum = w.quality_fit + w.cost_fit;
            assert!((sum - 1.0).abs() < 1e-3, "weights 归一化: {w:?}");
            assert!(r.breakdown.quality_fit.is_some());
            assert!(r.breakdown.cost_fit.is_some());
            assert!(r.breakdown.coarse.is_none());
        }
        for pair in resp.recommendations.windows(2) {
            assert!(
                pair[0].score >= pair[1].score,
                "score 必须降序: {} < {}",
                pair[0].score,
                pair[1].score
            );
        }
        assert_eq!(resp.meta.request_id, "req-1");
        assert_eq!(resp.meta.backend, "mock-1");
        let usage = resp.meta.usage.expect("usage");
        assert!(usage.input_tokens > 0 && usage.output_tokens > 0);
        assert_eq!(resp.profile.task_type.as_deref(), Some("chat"));
    }

    // ---------- 2. 确定性 tie-break ----------

    #[tokio::test]
    async fn equal_scores_break_by_price_then_id() {
        let pipe = mock_pipeline();
        let mut request = plain_request();
        request.options.top_k = Some(50);
        let resp = pipe.recommend(&request, "req-tie").await.unwrap();

        let mut models = configs().catalog.models.clone();
        models.sort_by(|a, b| {
            a.price
                .in_per_mtok
                .partial_cmp(&b.price.in_per_mtok)
                .unwrap()
                .then(a.id.cmp(&b.id))
        });
        let expected: Vec<String> = models.into_iter().map(|m| m.id).collect();
        let got: Vec<String> = resp
            .recommendations
            .iter()
            .map(|r| r.model.clone())
            .collect();
        assert_eq!(got, expected);
        for r in &resp.recommendations {
            assert_eq!(r.score, 0.5, "Mock 同分");
        }
    }

    // ---------- 3. include_debug ----------

    #[tokio::test]
    async fn include_debug_exposes_questions_answers_and_state() {
        let pipe = mock_pipeline();
        let mut request = plain_request();
        request.options.include_debug = true;
        let resp = pipe.recommend(&request, "req-dbg").await.unwrap();

        let debug = resp.debug.expect("debug 必须存在");
        for key in ["questions_sent", "answers_raw", "state"] {
            assert!(debug.get(key).is_some(), "debug 缺少 {key}");
        }
        assert!(debug["questions_sent"].as_array().unwrap().len() == 1);
        assert!(debug["answers_raw"].as_array().unwrap().len() == 1);
        assert_eq!(debug["state"].as_array().unwrap().len(), 1);

        let plain = pipe.recommend(&plain_request(), "req-nodbg").await.unwrap();
        let v = serde_json::to_value(&plain).unwrap();
        assert!(v.get("debug").is_none(), "非 debug 响应不得含 debug 键");
    }

    // ---------- 4. 空结果 ----------

    #[tokio::test]
    async fn empty_candidates_skip_backend_and_report_filtered() {
        let pipe = mock_pipeline();
        let mut request = plain_request();
        request.constraints.require_tags = vec!["no_such_tag".into()];
        let resp = pipe.recommend(&request, "req-empty").await.unwrap();

        assert!(resp.recommendations.is_empty());
        assert!(!resp.degraded);
        assert!(resp.profile.task_type.is_none());
        assert!(resp.profile.difficulty.is_none());
        assert!(resp.profile.frontier_needed.is_none());
        assert!(resp.meta.usage.is_none());
        let filtered = resp.meta.filtered.expect("filtered");
        let tag = filtered
            .iter()
            .find(|f| f.rule == "tag_require")
            .expect("tag_require 计数");
        assert_eq!(tag.count as usize, pipe.catalog.models().len());
    }

    // ---------- 5. 降级 ----------

    #[tokio::test]
    async fn backend_4xx_degrades_to_coarse_ranking() {
        let pipe = fail_pipeline(JudgeError::Http {
            status: 401,
            body: "nope".into(),
        });
        let resp = pipe.recommend(&plain_request(), "req-deg").await.unwrap();

        assert!(resp.degraded);
        assert_eq!(resp.meta.degraded_reason.as_deref(), Some("backend_4xx"));
        assert_eq!(resp.meta.backend, "fail");
        assert!(resp.meta.usage.is_none());
        assert!(!resp.recommendations.is_empty());
        assert!(resp.profile.task_type.is_none());
        assert!(resp.profile.difficulty.is_none());
        assert!(resp.profile.frontier_needed.is_none());
        for r in &resp.recommendations {
            assert!(r.confidence.is_none());
            assert!(!r.low_confidence);
            assert!(r.breakdown.coarse.is_some());
            assert!(r.breakdown.quality_fit.is_none());
            assert!(r.breakdown.cost_fit.is_none());
            assert!(r.breakdown.weights.is_none());
            assert!(r.breakdown.penalty.is_none());
            assert!(
                r.reasons[0].contains("未经语义判断"),
                "降级提示: {:?}",
                r.reasons
            );
        }
        assert!(resp.debug.is_none());
    }

    #[tokio::test]
    async fn timeout_degrades_to_unavailable() {
        let pipe = fail_pipeline(JudgeError::Timeout);
        let resp = pipe.recommend(&plain_request(), "req-to").await.unwrap();
        assert!(resp.degraded);
        assert_eq!(
            resp.meta.degraded_reason.as_deref(),
            Some("backend_unavailable")
        );
    }

    // ---------- 6. include_debug + 后端错误 → Upstream ----------

    #[tokio::test]
    async fn debug_mode_surfaces_backend_error() {
        let pipe = fail_pipeline(JudgeError::Http {
            status: 401,
            body: "nope".into(),
        });
        let mut request = plain_request();
        request.options.include_debug = true;
        let err = pipe.recommend(&request, "req-err").await.unwrap_err();
        assert!(
            matches!(err, PipelineError::Upstream(_)),
            "期望 Upstream,得到 {err:?}"
        );
        assert!(err.to_string().contains("401"));
    }

    // ---------- 7. 分片 ----------

    #[tokio::test]
    async fn large_catalog_is_sharded_with_local_ids() {
        // 价格按 `i % 30` 递增:第 2 片的 m30..m44 与第 1 片的 m00..m14 同价交错,
        // 使全局顺序无法由「片 1 整体 + 片 2 整体」拼接得到。
        let models: Vec<ModelEntry> = (0..45)
            .map(|i| model_with(&format!("m{i:02}"), (i % 30) as f64 * 10.0, &[]))
            .collect();
        let mut ordered: Vec<&ModelEntry> = models.iter().collect();
        ordered.sort_by(|a, b| {
            a.price
                .in_per_mtok
                .partial_cmp(&b.price.in_per_mtok)
                .unwrap()
                .then(a.id.cmp(&b.id))
        });
        let expected: Vec<String> = ordered.into_iter().map(|m| m.id.clone()).collect();
        let catalog: Arc<dyn CatalogProvider> = Arc::new(TestCatalog {
            models,
            hash: "hi45".into(),
        });
        let backend = RecordingBackend::new();
        let pipe = pipeline_with_catalog(backend.clone(), catalog);
        let request = plain_request();

        let p = pipe.prepare(&request).unwrap();
        assert_eq!(p.shard_requests.len(), 2);
        assert_eq!(p.shard_ranges, vec![0..30, 30..45]);
        // 第二片局部重编号:从 c0 开始,只有 15 个候选的问题。
        let second = &p.shard_requests[1];
        assert!(second.questions.contains_key("c0.quality_fit"));
        assert!(second.questions.contains_key("c14.cost_fit"));
        assert!(!second.questions.keys().any(|k| k.starts_with("c15.")));
        assert_eq!(second.state["candidates"][0]["i"], 0);
        assert_eq!(second.state["candidates"][0]["id"], "m30");

        let mut request = request;
        request.options.top_k = Some(50);
        let resp = pipe.recommend(&request, "req-shard").await.unwrap();

        let sent = backend.requests();
        assert_eq!(sent.len(), 2);
        assert_eq!(sent[1], p.shard_requests[1]);

        assert_eq!(resp.recommendations.len(), 45);
        for (i, r) in resp.recommendations.iter().enumerate() {
            assert_eq!(r.rank, i as u32 + 1);
        }
        let got: Vec<String> = resp
            .recommendations
            .iter()
            .map(|r| r.model.clone())
            .collect();
        assert_eq!(got, expected, "跨片全局有序(同分 → 价格 → id)");
        // 真正的交错:m00/m30 同价、m01/m31 同价,片 2 候选夹在片 1 之间。
        assert_eq!(&got[..4], ["m00", "m30", "m01", "m31"]);
        assert_ne!(
            got,
            (0..45).map(|i| format!("m{i:02}")).collect::<Vec<_>>(),
            "顺序不得是片 1 整体 + 片 2 整体"
        );
        // 两片 usage 合并
        let usage = resp.meta.usage.expect("usage");
        assert_eq!(usage.input_tokens, 600);
        assert_eq!(usage.output_tokens, 20);
    }

    // ---------- 8. 截断 ----------

    #[tokio::test]
    async fn truncates_to_max_judged_by_coarse_rank() {
        let backend = RecordingBackend::new();
        let pipe = pipeline_with_catalog(backend.clone(), catalog_of(70));
        let request = plain_request();

        let p = pipe.prepare(&request).unwrap();
        assert_eq!(p.candidates.len(), 70);
        assert_eq!(p.judge_indices.len(), 60);
        assert_eq!(p.shard_requests.len(), 2);
        assert_eq!(p.shard_ranges, vec![0..30, 30..60]);
        let judged: usize = p.shard_ranges.iter().map(|r| r.len()).sum();
        assert_eq!(judged, 60);
        // 粗排按价格分位:最贵的两个被截掉。
        let judged_ids: Vec<&str> = p
            .judge_indices
            .iter()
            .map(|&i| p.candidates[i].model.id.as_str())
            .collect();
        assert!(!judged_ids.contains(&"m68"));
        assert!(!judged_ids.contains(&"m69"));

        let resp = pipe.recommend(&request, "req-trunc").await.unwrap();
        assert!(!resp.degraded);
        assert_eq!(resp.recommendations.len(), 5);
        assert_eq!(backend.requests().len(), 2);
        // 判断数 = 60 → 问题数 = 3 共享 + 60×2
        assert_eq!(p.shard_requests[0].questions.len(), 63);
        assert_eq!(p.shard_requests[1].questions.len(), 63);
    }

    // ---------- 9. partial ----------

    #[tokio::test]
    async fn missing_dimension_marks_low_confidence_and_partial() {
        let backend = RecordingBackend::dropping("c1.quality_fit");
        let pipe = pipeline_with(backend.clone());
        let mut request = plain_request();
        request.options.top_k = Some(50);

        let p = pipe.prepare(&request).unwrap();
        let target = p.candidates[p.judge_indices[1]].model.id.clone();
        let resp = pipe.recommend(&request, "req-partial").await.unwrap();

        let rec = resp
            .recommendations
            .iter()
            .find(|r| r.model == target)
            .expect("目标候选应在推荐内");
        assert!(rec.low_confidence, "缺维度 → min confidence = 0");
        assert_eq!(rec.confidence, Some(0.0));
        let partial = resp.meta.partial_answers.expect("partial_answers");
        assert!(
            partial.contains(&"c1.quality_fit".to_string()),
            "partial: {partial:?}"
        );
    }

    /// 第 2 片的局部 c1 必须重编为全局 c31(不是 c1),且只影响该片候选。
    #[tokio::test]
    async fn partial_id_in_second_shard_is_remapped_to_global() {
        let backend = RecordingBackend::dropping_in_shard("c1.quality_fit", "m30");
        let pipe = pipeline_with_catalog(backend.clone(), catalog_of(45));
        let mut request = plain_request();
        request.options.top_k = Some(50);

        let p = pipe.prepare(&request).unwrap();
        assert_eq!(p.shard_ranges, vec![0..30, 30..45]);
        let target = p.candidates[p.judge_indices[31]].model.id.clone();
        assert_eq!(target, "m31");

        let resp = pipe.recommend(&request, "req-partial2").await.unwrap();

        let rec = resp
            .recommendations
            .iter()
            .find(|r| r.model == target)
            .expect("目标候选应在推荐内");
        assert_eq!(rec.confidence, Some(0.0));
        assert!(rec.low_confidence);
        assert_eq!(
            resp.meta.partial_answers,
            Some(vec!["c31.quality_fit".to_string()]),
            "第二片的 c1 必须重编为全局 c31"
        );
        // 只丢了一次:第二片请求确实被看到,且第一片未受影响。
        let sent = backend.requests();
        assert_eq!(sent.len(), 2);
        assert!(sent[0].questions.contains_key("c1.quality_fit"));
        assert!(sent[1].questions.contains_key("c1.quality_fit"));
    }

    /// 共享答案跨片合并:第一片缺失、第二片给出 → 采用第二片,且不报 partial。
    #[tokio::test]
    async fn shared_answer_from_second_shard_is_merged() {
        let backend = RecordingBackend::dropping_in_shard("task_type", "m00");
        let pipe = pipeline_with_catalog(backend.clone(), catalog_of(45));
        let request = plain_request();

        let resp = pipe.recommend(&request, "req-shared").await.unwrap();

        assert_eq!(resp.profile.task_type.as_deref(), Some("chat"));
        assert_eq!(resp.profile.difficulty, Some(1.0));
        assert_eq!(resp.profile.frontier_needed, Some(0.3));
        let partial = resp.meta.partial_answers.unwrap_or_default();
        assert!(
            !partial.contains(&"task_type".to_string()),
            "已被第二片回答的共享问题不得报缺失: {partial:?}"
        );
        // 第一片确实缺了答案(两片都被请求到)。
        let sent = backend.requests();
        assert_eq!(sent.len(), 2);
        assert!(sent[0].questions.contains_key("task_type"));
    }

    /// Task 17 的编译期守卫:Pipeline 必须可跨线程/跨任务共享。
    #[test]
    fn pipeline_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<Pipeline>();
    }

    // ---------- 10. top_k / locale ----------

    #[tokio::test]
    async fn top_k_and_locale_are_honored() {
        let pipe = mock_pipeline();
        let mut request = plain_request();
        request.options.top_k = Some(2);
        let resp = pipe.recommend(&request, "req-k").await.unwrap();
        assert_eq!(resp.recommendations.len(), 2);

        let mut request = plain_request();
        request.options.locale = Locale::En;
        let resp = pipe.recommend(&request, "req-en").await.unwrap();
        assert!(
            resp.recommendations[0]
                .reasons
                .iter()
                .any(|s| s.contains("Quality fit")),
            "en 理由: {:?}",
            resp.recommendations[0].reasons
        );
    }

    // ---------- 11. 非法输入 ----------

    #[tokio::test]
    async fn invalid_inputs_map_to_pipeline_errors() {
        let pipe = mock_pipeline();

        let bad_top_k = req(json!({"request": {"messages": []}, "options": {"top_k": 0}}));
        let err = pipe.prepare(&bad_top_k).unwrap_err();
        assert!(
            matches!(err, PipelineError::InvalidConstraints(_)),
            "期望 InvalidConstraints,得到 {err:?}"
        );
        assert!(err.to_string().contains("top_k"));

        let empty_body = req(json!({"request": {}}));
        let err = pipe.prepare(&empty_body).unwrap_err();
        assert!(
            matches!(err, PipelineError::InvalidRequestBody(_)),
            "期望 InvalidRequestBody,得到 {err:?}"
        );
    }

    // ---------- 12. 粗排分位分母 = 候选集 ----------

    #[tokio::test]
    async fn coarse_percentiles_use_filtered_candidate_set() {
        let catalog: Arc<dyn CatalogProvider> = Arc::new(TestCatalog {
            models: vec![
                model_with("m-cheap", 0.5, &[]),
                model_with("m-mid", 1.0, &["foo"]),
                model_with("m-pricey", 2.0, &["foo"]),
            ],
            hash: "h3".into(),
        });
        let pipe = pipeline_with_catalog(Arc::new(MockBackend::new("mock")), catalog);
        let mut request = plain_request();
        request.constraints.require_tags = vec!["foo".into()];

        let p = pipe.prepare(&request).unwrap();
        assert_eq!(p.candidates.len(), 2);
        // 3 个模型里过滤掉最便宜的一个;分位分母必须是 2(不是 3):
        // 分母 3 会得到 1.0 / 0.5,分母 2 才是 1.0 / 0.0。
        assert_eq!(p.coarse_map["m-mid"].price_score, 1.0);
        assert_eq!(p.coarse_map["m-pricey"].price_score, 0.0);
        assert!(!p.coarse_map.contains_key("m-cheap"));
    }
}
