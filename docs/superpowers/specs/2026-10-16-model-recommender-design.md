# 模型推荐服务(model-recommender)设计文档

- **日期**: 2026-10-16
- **状态**: 待评审(设计已与发起人逐节确认)
- **项目路径**: `/Users/mapxus/model-recommender/`
- **实现语言**: Rust(2021 edition)

---

## 1. 背景与目标

**要解决的问题**: 网关(new-api 等)或任意调用方在一个用户请求到达时,需要从一份模型清单中选出最合适的若干模型。传统做法是写死的规则或人工配置;本项目的目标是用"决策模型"(System One 类,如 Jev/Laya/KEV)+ 少量确定性规则,对请求的**语义需求**做出判断,输出:

1. **Top-K(默认 5)模型推荐**,按综合分排序;
2. **每个推荐的人类可读判断原因**(模板化,支持中/英);
3. 结构化中间产物(需求画像、各维度分数、规则命中、置信度),供调用方决策与审计。

**设计原则**:
- **代码掌控流程,模型只做窄判断**: 硬约束过滤、归一化、加权融合、排序、理由拼装全部在代码里确定性地完成;决策模型只回答"这个候选和需求匹配吗、成本合适吗"这类窄问题。
- **可插拔**: 目录来源、规则、判断后端、理由渲染都是 trait + 配置驱动;v1 全实现默认版本,替换不动核心逻辑。
- **运维现实**: 决策后端是外部服务(无 SLA),必须有缓存、重试、降级、审计。

**参考项目**: new-api(QuantumNous/new-api,已单独做过逆向分析)。参考其:
- **适配器注册表模式**(`GetAdaptor` 工厂 + `plugins/tasks` 内嵌插件注册) → 本项目的 Rule/JudgmentBackend 注册表;
- **Pricing 数据结构的字段设计**(model_name/description/tags/vendor/ratio/endpoint types)→ 本项目 `catalog.yaml` 的字段参考;
- **渠道选择的"优先级分层+层内加权随机"思路**(`GetRandomSatisfiedChannel`)→ 本项目 v1.1 学习型粗排的预留参考,不在 v1 使用。

**不照搬 new-api 任何代码**;它只作为模式参考。

---

## 2. 范围

### v1 做
- 独立 HTTP 服务:输入原始请求体(OpenAI Chat Completions / Claude Messages 两种容错解析)+ 可选约束,输出 top-K + 理由。
- 本地 `catalog.yaml` 目录(通过 `CatalogProvider` trait 暴露)。
- 硬规则引擎(能力/上下文/预算/厂商约束,插件链)。
- TypeSafe `/v1/systemone` 契约客户端(默认 `jev-latest` 对应钉死的版本 ID;Laya/KEV 换 `base_url` 即可)。
- 一次批量语义调用(共享问题 + 每候选维度的 Score 问题),代码融合与模板理由(zh/en)。
- 内存缓存、后端重试与降级、结构化日志与指标、审计日志。
- CLI:`validate-catalog`、`dry-run`(打印将发送的完整问题,不联网)、`eval`(评估集跑分)。
- 测试调试页: 服务内嵌 `GET /debug` 单 HTML(零构建;预填样例、约束表单、结果卡片 + 原始 JSON;开发/试用定位,非产品 UI,见 9.7)。
- 本地部署:单二进制 + `configs/` 直接运行(见 9.6);默认对接云 Jev(需 `TYPESAFE_API_KEY`;区域限制环境需后端代理,见 7 节),无 key / 断网时自动走降级链路。

### v1 明确不做
- 不做转发/计费/鉴权体系/用户管理(那是网关职责)。
- 不做本地 Laya/KEV 模型部署(仅保证契约兼容;这是区域限制场景的治本路线,见 13 节)。
- 不做产品级 Web UI(仅提供 9.7 的内嵌调试页)。
- 不做学习型粗排与分类器蒸馏(留 v1.1,材料是审计日志)。
- 不做多租户、不做 Embedding/向量检索。

---

## 3. 关键决策记录(Decision Log)

| # | 决策 | 理由 |
|---|---|---|
| D1 | 形态 = **独立推荐服务**(HTTP),不做网关 | 可插拔性最强;new-api 等通过旁路/中间件调用;边界清晰 |
| D2 | 输入 = **原始请求体 + 可选约束** | 最贴近真实网关场景;服务自己提炼需求画像 |
| D3 | 模型清单 = **本地 YAML + CatalogProvider trait** | 字段完全可控;未来可加 new-api 同步 Provider |
| D4 | 分工 = **硬规则过滤 + Jev 软判断 + 代码融合** | 可控、可解释、省钱;决策模型不做确定性判断 |
| D5 | 后端 = **/v1/systemone 契约客户端,默认 Jev** | KEV/Laya 刻意兼容该契约;换 base_url 即可;无 Rust 官方 SDK,手写简单 |
| D6 | 理由 = **模板化**(结构化拼装,零额外 LLM 调用) | 决策模型不生成文本;i18n 与审计友好 |
| D7 | 语言 = **Rust**(发起人选定) | 单二进制、可控运行时;曾评估 TS(官方 SDK)/Go(new-api 生态)为更省力方案,发起人明确选 Rust |
| D8 | 架构 = **A 型(纯 Jev 打分)** + 预留缓存/降级/粗排接口缝 | 链路最短;缝是几十行的事,但决定将来扩展成本 |

---

## 4. 总体架构

### 4.1 请求流水线

```
POST /v1/recommend { request, constraints?, options? }
 │
 ├─① ProfileExtractor      纯代码,<1ms
 │   容错解析 OpenAI/Claude 请求体 → Profile{est_tokens, has_images, tools_count,
 │   json_mode, stream, max_tokens, text_chars, request_excerpt, locale_hint?}
 │
 ├─② HardRuleEngine        纯代码,μs 级;规则 = 有序插件链(Vec<Box<dyn Rule>>)
 │   每条规则对每个候选返回 Pass / Filter{reason} / Penalty{value, reason}
 │   产出: 候选集合 + 规则命中记录(供理由与审计)
 │   若候选为空 → 直接 200 + recommendations:[] + meta.filtered(见 8.1;合法结果)
 │
 ├─③ CoarseRank/分片规划    纯代码,确定性打分(标签重叠率 + 价格分位)
 │   ≤30 单次;30<N≤60 分片并行后合并;N>60 先粗排截断到 60 再分片
 │   粗排同时是降级路径的排序依据(任意 N,见 6.2)
 │
 ├─④ Judgment              一次(或分片并行的)systemone 批量调用
 │   state   = 请求画像摘要 + 精简候选目录(每候选: id/描述/tags/上下文/价格档)
 │   questions = 3 个共享问题 + 每候选 2 个 Score(quality_fit / cost_fit)
 │
 ├─⑤ Fusion                纯代码,μs 级
 │   归一化 norm = score/(档数-1);final = Σ w_i*norm_i − penalties;确定性 tie-break
 │   confidence = min(使用的维度 confidence);低于门槛 → 标记 low_confidence
 │
 └─⑥ Explain               模板渲染(zh/en),输出理由数组
     → RecommendationResponse
```

### 4.2 Rust workspace 布局

```
model-recommender/                      # git 仓库根
├─ Cargo.toml                           # workspace
├─ crates/
│  ├─ core/          recommender-core    # 纯逻辑: 画像/规则/提问构造/融合/理由。无网络,全单测
│  ├─ systemone/     recommender-systemone # /v1/systemone 契约客户端(reqwest)
│  ├─ server/        recommender-server  # axum 二进制(含内嵌调试页 assets/debug.html)
│  └─ cli/           recommender-cli     # 二进制: validate-catalog / dry-run / eval
├─ configs/
│  ├─ catalog.yaml
│  ├─ rules.yaml
│  ├─ questions.yaml
│  └─ server.yaml
├─ eval/
│  └─ scenarios/     # 评估集 YAML + request fixtures
├─ templates/                            # reason.zh.toml / reason.en.toml(理由模板)
├─ examples/                             # sample-request.json(本地试用样例)
├─ docs/superpowers/specs/               # 本文档
└─ .gitignore
```

依赖(锁定版本在实现计划阶段):`axum`, `tokio`, `reqwest`(rustls), `serde`/`serde_json`, `serde_yaml`, `moka`(缓存), `tracing`/`tracing-subscriber`, `tower`/`tower-http`(限流/追踪/请求体上限), `thiserror`/`anyhow`, `blake3`(hash), `uuid`, `prometheus`(或 `metrics` 系列)。dev: `wiremock`, `insta`, `rstest`。

### 4.3 可插拔 trait(注册表工厂 + 配置驱动)

```rust
/// 目录来源。默认实现: 从 catalog.yaml 加载。未来: new-api pricing 同步。
pub trait CatalogProvider: Send + Sync {
    fn models(&self) -> &[ModelEntry];
    fn version(&self) -> &str;            // 内容 hash,用于缓存 key 与审计
}

/// 规则上下文: 一次请求内所有规则共享的输入
pub struct RuleCtx<'a> {
    pub profile: &'a Profile,
    pub constraints: &'a Constraints,   // budget/vendor/tags 等调用方约束
    pub options: &'a Options,           // top_k/locale/include_debug
}

/// 硬规则。配置里按 name 实例化;注册表: fn build(name, params) -> Box<dyn Rule>
pub trait Rule: Send + Sync {
    fn name(&self) -> &str;
    fn eval(&self, ctx: &RuleCtx, model: &ModelEntry) -> RuleOutcome;
}

pub enum RuleOutcome {
    Pass,
    Filter { reason_key: ReasonKey, params: BTreeMap<String, String> },
    Penalty { value: f32, reason_key: ReasonKey, params: BTreeMap<String, String> },
}

/// 判断后端。/v1/systemone 契约是唯一 v1 实现(kind = "systemone")
pub trait JudgmentBackend: Send + Sync {
    fn id(&self) -> &str;                 // 配置 profile id
    fn judge<'a>(&'a self, req: JudgeRequest)
        -> BoxFuture<'a, Result<JudgeResponse, JudgeError>>;
}

/// 理由渲染。默认实现: 模板文件(zh/en)。可换。
pub trait ReasonRenderer: Send + Sync {
    fn render(&self, ctx: &ReasonCtx, locale: &Locale) -> Vec<String>;
}
```

---

## 5. 数据模型与配置

### 5.1 `catalog.yaml`

```yaml
version: 2026-10-16          # 人工维护的版本标记(内容仍会 hash)
defaults:
  status: enabled
models:
  - id: claude-sonnet-4.6
    name: Claude Sonnet 4.6
    vendor: anthropic
    description: >-
      高能力通用模型;代码与长上下文任务表现突出;支持工具调用与视觉。
    tags: [code, reasoning, long_context, vision, tools, zh_good]
    capabilities: { tools: true, vision: true, json_mode: true, audio_in: false }
    context_window: 200000
    max_output: 64000
    price: { in_per_mtok: 3.0, out_per_mtok: 15.0 }
    groups: [default]
    quality_notes: ""         # 可选: 给 Jev 的补充质量描述
  - id: deepseek-v3.2
    name: DeepSeek V3.2
    vendor: deepseek
    description: 高性价比通用模型;中文强;支持工具调用。
    tags: [chat, code, tools, zh_strong, cheap]
    capabilities: { tools: true, vision: false, json_mode: true, audio_in: false }
    context_window: 131072
    max_output: 32768
    price: { in_per_mtok: 0.28, out_per_mtok: 0.42 }
    groups: [default]
```

**必填**: `id`(唯一)、`name`、`vendor`、`tags`(非空)、`capabilities`(布尔齐全)、`context_window`(>0)、`price`(非负);**可选**: `description`(默认空)、`quality_notes`(默认空)、`max_output`(默认 = `context_window`)、`groups`(默认 `[default]`)、`status`(默认 enabled)。以上约束由 `validate-catalog` 校验。**v1 语义**: `status: disabled` 的模型在加载时直接排除;`groups` 与 `soft_penalties.vendor_preference` 为 v1.1 保留字段(加载时校验格式、暂不消费)。

### 5.2 `rules.yaml`

```yaml
rules:
  - kind: capability          # 内置 kind: capability / context_length / budget / vendor / tag_require
    enabled: true
  - kind: context_length
    enabled: true
    safety_factor: 1.15       # est_tokens*safety + max(0, max_tokens) ≤ context_window
  - kind: budget
    enabled: true
  - kind: vendor
    enabled: true
  - kind: tag_require
    enabled: true
soft_penalties:
  vendor_preference: { white: [], value_each: 0.05 }   # 非 must,按需
```

规则语义(确定性,可单测):
- `capability`: `tools_count>0 ⇒ capabilities.tools`;`has_images ⇒ capabilities.vision`;`json_mode ⇒ capabilities.json_mode`;`stream` 不产生约束。违反 → `Filter`。
- `context_length`: `ceil(est_tokens*safety_factor) + max_tokens_estimate ≤ context_window`。违反 → `Filter`。`est_tokens` 估算规则:v1 用 `ASCII 字符数/3.5 + CJK 字符数/1.5 + 每消息 4 token 开销`(保守系数,按评估集可调);决策文档与实现同源。
- `budget`: `constraints.budget.max_input_price_per_mtok` 存在且 `price.in_per_mtok >` 上限 → `Filter`;`constraints.budget.cost_sensitive == true` → 影响融合权重(见 6.4)。
- `vendor`: `constraints.vendor_deny` 命中 → `Filter`;`vendor_allow` 非空且不命中 → `Filter`。
- `tag_require`: `constraints.require_tags` 任一不在 `model.tags` → `Filter`。

### 5.3 `questions.yaml`

```yaml
shared:
  task_type:
    type: choice
    instructions: "用户请求属于哪类任务?以 `request_excerpt` 与 `profile` 为准。"
    criteria:
      code_debug: 排查/修复代码问题
      code_gen: 编写新代码或改造现有代码
      reasoning: 数学/逻辑/分析推理
      translation: 翻译
      creative: 创意写作
      summarization: 摘要/提炼
      extraction: 结构化抽取
      chat: 一般对话/问答
  difficulty:
    type: score
    instructions: "该请求的解决难度如何?"
    criteria: ["简单,常规直接处理", "中等,需要一定专业能力", "困难,需要顶级推理或领域深度"]
  frontier_needed:
    type: noul
    instructions: "该请求是否必须使用当前可得的最强能力模型才能可靠完成?"
    criteria: { "true": "是,容错低且难度高", "false": "否,常规能力足够" }
per_candidate:
  quality_fit:
    type: score
    instructions: >-
      `candidates[<i>]`(见 state.candidates)对本请求的质量匹配度如何?
      考虑: 能力是否覆盖任务类型(参考 task_type/difficulty/frontier_needed)、
      该模型在该领域的已知强项、上下文窗口是否满足、语言适配(参考 profile.locale_hint)。
    criteria:
      - { what: "明显不适合该需求", examples: ["需要视觉但无视觉能力"] }
      - { what: "勉强可用，有明显短板", examples: ["能力可用但该领域不是强项"] }
      - { what: "合适，能很好完成", examples: ["同类任务口碑良好"] }
      - { what: "最佳匹配，任务特征高度吻合", examples: ["该任务的典型首选"] }
  cost_fit:
    type: score
    instructions: >-
      `candidates[<i>]` 的成本与需求方的预算/敏感度匹配度?(考虑输入/输出单价与
      state.constraints;价格档位: cheap/mid/high 已标注在候选上;无约束时按中性判断)
    criteria: ["超出预算或性价比明显差", "成本可接受", "经济实惠"]
price_bands:                  # 代码据此把输入单价($/M tok)映射为 price_band
  cheap_max: 1.0
  mid_max: 5.0                # 超出即 high
weights:
  base: { quality_fit: 0.75, cost_fit: 0.25 }
  adjustments:                # 命中即调整,可叠加,最后对两维归一化
    - when: { difficulty_at_least: 1.5 }
      set: { quality_fit: "+0.10", cost_fit: "-0.10" }
    - when: { frontier_needed_above: 0.6 }
      set: { quality_fit: "+0.05", cost_fit: "-0.05" }
    - when: { budget_present: true }     # = constraints.budget 非空
      set: { cost_fit: "+0.10", quality_fit: "-0.10" }
    - when: { cost_sensitive: true }     # = constraints.budget.cost_sensitive == true
      set: { cost_fit: "+0.15", quality_fit: "-0.15" }
  clamp: { min: 0.05, max: 0.95 }
confidence:
  flag_below: 0.5             # 低于则 recommendation.low_confidence = true
```

`criteria` 支持字符串或结构化对象(官方推荐:档位用"具体情境+示例"而不是程度副词)。`questions.yaml` 是**判断逻辑的唯一事实源**,调参不改代码。

### 5.4 `server.yaml`

```yaml
server: { listen: "0.0.0.0:8787", client_auth_token_env: RECOMMENDER_AUTH_TOKEN,
          admin_token_env: RECOMMENDER_ADMIN_TOKEN, metrics_enabled: true,
          debug_page_enabled: true,
          max_request_bytes: 4194304, request_timeout_ms: 15000 }
backends:
  - id: jev
    kind: systemone
    base_url: https://api.typesafe.ai
    model: jev-1.13.0          # 钉死版本 ID,不用漂移 alias
    api_key_env: TYPESAFE_API_KEY
    proxy: null                # 区域限制(451)时必需,例: http://127.0.0.1:7897;留空则沿用 HTTPS_PROXY/ALL_PROXY 环境变量
    timeout_ms: 5000           # 单次尝试
    total_budget_ms: 8000      # 重试/退避/信号量排队共享的总预算,超预算立即降级
    max_inflight: 16
    retry: { max_attempts: 2, base_delay_ms: 250, honor_retry_after: true }
    long_context_header: false # 候选过多分片,而非开 262k 长上下文
cache: { enabled: true, ttl_secs: 600, max_entries: 2048 }
limits: { max_candidates_in_call: 30, max_candidates_judged: 60, default_top_k: 5 }
```

secret 一律走环境变量(yaml 只存 env 名;backend `proxy` 为空时自动沿用 `HTTPS_PROXY`/`ALL_PROXY` 环境变量,见 7 节区域限制)。

---

## 6. 判断层设计(核心)

### 6.1 ProfileExtractor

- 输入: 原始请求体(JSON,两种格式容错解析:`messages[]` 通用;`system` 字段存在与否均可)。
- 提取: 拼接文本内容(字符串或 content 数组中的 `text` 部分)→ `est_tokens` 估算;统计 `images`(content 里的 `image_url`/`image` 块)、`tools`(顶层 `tools[]` 长度)、`json_mode`(`response_format.type == "json_object"` 或 `"json_schema"`)、`stream`、`max_tokens`。
- `request_excerpt`: 用户文本经清洗(去控制字符)后,取开头 2000 字符 + 末尾 500 字符,中间以 `…[截断]…` 标记;总长上限 3000 字符。**用于 Jev 的语义判断,单独放进 state 的专用字段,且不与 instructions 混排**(注入防护,见 9.4)。
- `locale_hint`: CJK 字符占比 > 20% → `zh`,否则 `other`(仅作为提示,不做语言识别断言)。
- `task_hints`(可选): 固定检测表 v1 三条——文本含代码块围栏(三反引号)→ "包含代码块";含报错关键词(Error/Exception/异常/堆栈)→ "提到报错堆栈";含 URL → "包含链接"。命中才加入,无则省略该字段。
- 解析失败容错: 非 JSON 或缺少 `messages` → 尝试按 `{"messages":[{"role":"user","content":"<原文>"}]}` 包一层;再失败 → `422 invalid_request_body`(外层信封 JSON 语法错误另由 `400 invalid_json` 处理,见 8.2)。

### 6.2 硬规则过滤(见 5.2)与 CoarseRank / 分片

**候选规模策略(v1 唯一口径)**:
- N ≤ `max_candidates_in_call`(默认 30):单次 systemone 调用。
- 30 < N ≤ `max_candidates_judged`(默认 60):分片为每片 ≤30,并行调用,按候选序号合并答案。
- N > 60:先 CoarseRank 截断到 60,再分片。
- **降级路径**: 后端不可用时,一律返回 CoarseRank 确定性排序(任意 N),`degraded: true`。

CoarseRank(v1 薄实现,确定性):
- `needs` 标签集(由画像推导): `has_images ⇒ vision`;`tools_count>0 ⇒ tools`;`json_mode ⇒ json_mode`;`est_tokens > 32k ⇒ long_context`;needs 为空时重叠率记 1.0。
- 标签重叠率 = |needs ∩ model.tags| / |needs|。
- 价格分位 = 1 − r/(N−1)(r = 输入单价升序名次,0 起点;最便宜 = 1.0,最贵 = 0.0;N=1 时记 1.0)。
- `coarse = 0.5*标签重叠率 + 0.5*价格分位`;tie-break = id 字典序(与 6.4 一致)。

它只服务"防超限(>60 截断)+ 降级兜底",不追求质量;语义排序完全交给 ④。

### 6.3 Jev 提问构造(6.3 是本文档最核心的可实现细节)

- **问题 ID 约定**: 共享问题用固定 ID(`task_type`/`difficulty`/`frontier_needed`);每候选问题用 `c{idx}.quality_fit` / `c{idx}.cost_fit`(`idx` 为候选在 state 中的序号)。
- **state 构造**:

```json
{
  "profile": {
    "est_tokens": 8200, "has_images": false, "tools_count": 3, "json_mode": false,
    "max_tokens": 4096, "locale_hint": "zh"
  },
  "request_excerpt": "……清洗截断后的用户请求文本……",
  "task_hints": ["包含代码块", "提到报错堆栈"],
  "constraints": { "budget_max_in_price": 5.0, "cost_sensitive": false },
  "candidates": [
    { "i": 0, "id": "claude-sonnet-4.6", "desc": "高能力通用模型;代码与长上下文突出",
      "tags": ["code","reasoning","long_context","tools","zh_good"],
      "ctx": 200000, "price_band": "mid", "in_price": 3.0, "out_price": 15.0 },
    { "i": 1, "id": "deepseek-v3.2", "desc": "高性价比通用;中文强", "tags": ["chat","code","tools","zh_strong","cheap"],
      "ctx": 131072, "price_band": "cheap", "in_price": 0.28, "out_price": 0.42 }
  ]
}
```

- **价格档位由代码计算**: `price_band` 按输入单价经 questions.yaml 的 `price_bands` 映射(cheap ≤ $1.0/M、mid ≤ $5.0/M、high > $5.0/M 输入 token);预算约束以显式数值进 `state.constraints`(未提供则不出现该字段,模型按中性判断)。
- **token 预算管理**: state ≈ 300 + 45×候选数;每问题 ≈ 60–90。候选 30 时总量 ≈ 5–7k token,远低于 64k 请求上限与 32k 单问题上限。>30 候选按 6.2 分片并行(每片 ≤30),融合阶段按候选序号合并答案。
- **单次调用**: 一次 `POST /v1/systemone`,全部问题并行回答;`model` 来自配置。

### 6.4 融合与置信度(确定性公式)

```
// 1) 归一化 (官方建议: 除以最大档位序号)
norm(s) = s.score / (len(s.criteria) - 1)          // 都在 [0,1]

// 2) 权重: 从 questions.yaml 取 base,按 adjustments 逐条叠加(命中条件读共享答案与约束),
//    条件阈值一律比较原始 answer 值(difficulty 原始 0–2 分,不归一化);
//    最后对 {quality_fit, cost_fit} 归一化到和为 1,并 clamp 到 [0.05,0.95]

// 3) 候选综合分
final(m) = w_q * norm(quality_fit_m) + w_c * norm(cost_fit_m)
           - Σ penalty(m)                     // 规则软惩罚, 默认 0
// tie-break: final 相同 → 输入价格升序 → id 字典序 (保证完全确定性)

// 4) 置信度
confidence(m) = min(quality_fit_m.confidence, cost_fit_m.confidence)
recommendation.low_confidence = confidence(m) < questions.yaml.confidence.flag_below
```

排序取 `top_k`;响应中每个推荐附带 `breakdown`(归一化维度分/权重/惩罚/规则命中)与理由。

### 6.5 理由模板(zh/en,`templates/reason.{zh,en}.toml`)

每个推荐生成 3–5 条理由,固定结构:

1. **硬约束**: `✅ 硬约束全部满足:上下文 200k ≥ 估算 8k;safety 系数 1.15;支持 tool_calls`(或注明被过滤原因——仅出现在 `no_candidates`/`filtered_by_rule` 审计信息中,推荐项不出现)
2. **质量匹配**: `🧠 质量匹配 0.92(概率集中在「最佳匹配」61%):与代码调试任务特征高度吻合`
3. **成本适配**: `💰 成本适配 0.50($3.0/M 输入,中档):在预算上限内但非最省`
4. **置信度提示**(低置信时): `⚠️ 置信度中等(0.61):质量判断在「合适/最佳匹配」间存在分歧,建议结合第 2 名对比`
5. **软惩罚说明**(如有)

模板参数即结构化值,渲染语言由 `options.locale` 决定(zh|en,默认 zh)。

---

## 7. TypeSafe systemone 契约摘要(实现依据)

- `POST {base_url}/v1/systemone`,`Authorization: Bearer <key>`,`Content-Type: application/json`。
- 请求: `{ "model": "<id>", "state": <string|object|array>, "questions": { "<id>": Question } }`。
  - Question 公共字段: `type` + `instructions`(string|object|array);`noul` 可选 `criteria:{true,false}`;`choice` 必填 `criteria: {option: rubric|null}`(≤255 选项);`score` 必填 `criteria: [level…]`(2–10 档,元素可为 string|object|array)。
- 响应: `{ "model": "<实际版本 ID>", "answers": { "<id>": Answer }, "usage": {"input_tokens", "output_tokens"} }`。
  - `noul` answer: `{type, noul: 0..1}`;`choice`: `{type, choice, probabilities{option: p}, confidence}`;`score`: `{type, score, legend{level: desc}, probabilities{level: p}, confidence}`。
  - `score` = Σ(档位序号 × 概率),可为小数;`confidence` 由概率分布集中度导出。
- 限制: 请求体 ≤32MiB(支持 gzip/zstd 压缩层,单一层);嵌套 ≤64;数字必须有限;Jev 默认上下文 64k/请求、32k/问题(可选 `TypeSafe-Danger-Zone-Long-Context: true` 提到 262k,本项目默认不用);错误码 400/401/413/415/422/429/529(429/529 需按 `retry-after` 退避重试);上下文超限返回 `400 max_tokens_exceeded`。
- 模型: `jev-latest`(当前指向 jev-1.13.0)/`jev-preview`(1.14.0);价格 $0.042/M 输入 token,输出免费;延迟 ~100–500ms;各版本行为有差异,故**钉死版本 ID**。KEV(Qwen3.5+LoRA,兼容契约)与 Laya(开源 ModernBERT 系,兼容契约)只需更换 `base_url`/`model` 配置。
- 区域限制: 部分地区(实测大陆直连)返回 `451 not available in your region`;客户端需支持代理(见 5.4 `proxy`),或改用本地 Laya(见 13 节)。
- 契约实测(2026-10-16): `jev-latest` 解析为 `jev-1.13.0`;显式 `model: "jev-1.13.0"` 可用;`noul`/`score` 响应字段(score/legend/probabilities/confidence/usage)与本节摘要一致。

---

## 8. API 契约

### 8.1 `POST /v1/recommend`

请求:
```json
{
  "request":  { "···原始 OpenAI/Claude 请求体,原样透传···" },
  "constraints": {
    "budget": { "max_input_price_per_mtok": 5.0, "cost_sensitive": false },
    "require_tags": ["tools"],
    "vendor_allow": ["anthropic", "openai", "deepseek"],
    "vendor_deny": []
  },
  "options": { "top_k": 5, "locale": "zh", "include_debug": false }
}
```

响应(200):
```json
{
  "recommendations": [
    {
      "model": "claude-sonnet-4.6", "rank": 1, "score": 0.773, "confidence": 0.61,
      "low_confidence": false,
      "breakdown": {
        "quality_fit": 0.92, "cost_fit": 0.50, "weights": {"quality_fit": 0.65, "cost_fit": 0.35},
        "penalty": 0.0, "rule_hits": ["capability:tools✓", "context_length✓", "budget✓"]
      },
      "reasons": ["✅ …", "🧠 …", "💰 …"]
    }
  ],
  "profile": { "task_type": "code_debug", "difficulty": 1.2, "frontier_needed": 0.3, "est_tokens": 8200 },
  "degraded": false,
  "meta": { "backend": "jev-1.13.0", "latency_ms": 412, "cache_hit": false,
            "catalog_version": "a3f9…", "weights_version": "b21c…", "usage": {"input_tokens": 5210} }
}
```

- 示例体现 `budget_present` 调整:权重 0.65/0.35,`score = 0.65×0.92 + 0.35×0.50 = 0.773`(与 6.4 公式一致)。
- `include_debug: true` 时追加 `debug: {questions_sent, answers_raw, state}`(供 dry-run 联调)。
- 全部候选被过滤(或目录为空):200 + `recommendations: []` + `profile` + `meta.filtered: [{rule, count}]`。合法结果,不报错(4.1 与本契约一致)。

### 8.2 错误与降级语义

所有非 2xx 响应体统一为: `{"error":"<code>","message":"<说明>","request_id":"<id>"}`(200 降级响应仍走正常响应体,不走此信封)。

| 情况 | 行为 |
|---|---|
| 外层信封非法 JSON | `400 invalid_json` |
| 请求体超限 | `413 request_too_large` |
| `request` 字段无法解析为任一已知格式 | `422 invalid_request_body`(附尝试过的格式;与 6.1 一致) |
| 约束字段非法 | `422 invalid_constraints`(附字段路径) |
| 后端 4xx(非 429,如 400/401/413/422) | 不重试(确定性错误,多为配置/请求问题)→ 同样降级:`degraded: true` + `meta.degraded_reason: backend_4xx` + 指标计数(不把配置错误放大为服务中断) |
| `options.include_debug: true` 时后端出错 | 不降级,返回 `502 upstream_error` + 上游错误详情(联调时暴露真实问题) |
| 后端 429、5xx(含 529)、超时、连接失败 | 退避重试 1 次(尊重 `retry-after`,封顶 2s)→ 仍失败 → **降级**:返回 CoarseRank 排序(见 6.2)+ `degraded: true` + `meta.degraded_reason` + 理由注明"未经语义判断(后端不可用)" |
| 后端返回缺少问题答案 | 每候选 Score 维度按 0.5 中性值;共享问题缺失时权重调整条件视为未命中、响应 `profile` 对应字段为 null;记录警告并在 `meta.partial_answers` 列出 |
| 在途并发占满 | 等待 ≤2s(受后端总预算约束),仍满 → 降级路径 |
| 入站超时(体验兜底) | `408 request_timeout`(含 request id);正常路径不触发:后端总预算 8s + 降级 <10ms ≪ 入站 15s |
| 服务内部错误 | `500 internal_error`(带 request id) |

**降级/空结果/缺答案的响应字段口径**:
- **降级**(CoarseRank):`recommendations[].score` = 粗排分;`confidence = null`、`low_confidence = false`(降级态以顶层 `degraded` 为准);`breakdown` 的 `quality_fit/cost_fit/weights/penalty = null`,增加 `coarse: {tag_overlap, price_score}`;`rule_hits` 保留(硬规则照常执行);`profile` 判断字段(`task_type/difficulty/frontier_needed`)= null;`meta.usage = null`;理由注明"未经语义判断(后端不可用)"。
- **空结果**(全部被过滤/目录为空):`recommendations: []`;`profile` 判断字段 = null;`meta.usage = null`;`meta.filtered` 列出各规则过滤计数。
- **缺答案**(partial):缺失维度按**归一化 0.5** 参与融合,其 `confidence = 0.0`(因此该候选 `low_confidence = true`);`meta.partial_answers` 列出缺失问题 ID。

降级路径 SLA: ≤10ms 返回可用排序(无网络)。

### 8.3 其他端点

- `GET /healthz` → 200 `{"ok":true}`
- `GET /readyz` → 目录已加载 + 至少一个可用 backend 配置 → 200;否则 503
- `GET /v1/catalog` → 当前目录 + 版本 hash(需 `RECOMMENDER_ADMIN_TOKEN`,未配置则仅本机可访问)
- `GET /metrics` → Prometheus 文本(可配开关)
- `GET /debug` → 内嵌单文件调试页(开发/试用用途,见 9.7;受 `debug_page_enabled` 开关控制)

---

## 9. 运维设计

### 9.1 缓存
- `moka` 内存 LRU;key = `blake3(canonical(profile_core) + canonical(constraints) + catalog_version + weights_version + top_k + locale)`。
- `profile_core` 只含影响判断的字段(est_tokens 分桶到 256 粒度、布尔量、tools_count、max_tokens、excerpt 的 blake3、候选 id 集合)——避免因微小文本差异全部 miss。
- 命中直接返回完整响应(`meta.cache_hit=true`);TTL 默认 600s;`max_entries` 2048。
- **`options.include_debug: true` 绕过缓存(不读不写)**: 调试响应(含原始答案/state)绝不入缓存,不会串给普通请求。
- 目录/权重/配置变更 → 版本 hash 变化 → 旧键自然失效(不主动清理)。

### 9.2 重试与超时
- 连接超时 1s、请求超时 `timeout_ms`(默认 5000);重试触发条件与 8.2 一致(429、5xx 含 529、连接失败、超时),最多 2 次尝试,指数退避 + 抖动,尊重 `retry-after`(秒或日期)封顶 2s。所有尝试(含退避与信号量排队)共享 `total_budget_ms`(默认 8000),超预算立即放弃并走降级;入站 `request_timeout_ms`(默认 15000)> 后端总预算(8000)+ 降级耗时,正常路径入站超时不会先触发。

### 9.3 并发与限流自我保护
- `tokio::Semaphore` 限制在途后端调用(`max_inflight`,默认 16);获取许可等待上限 2s。
- 服务自身入站并发用 `tower::limit::ConcurrencyLimit`(默认 64)+ `tower-http` 请求体上限。
- 背景事实: Jev 发布时限流约 1200 RPM,`max_inflight` 即为保护该配额。

### 9.4 安全与注入防护
- 调用方: 静态 Bearer(env),恒定时间比较;缺失配置则仅允许本机(127.0.0.1)。
- 后端 key: 仅 env;日志与 `meta` 不出现任何 secret。
- `request_excerpt` 清洗: 去控制字符(保留 `\n`/`\t`)、限长(见 6.1);放入 state 的专用字段;所有指令只存在于 `questions`,不在 state 中混排(官方明确: state 被视为数据;不把不可信文本拼进指令位)。
- `include_debug` 默认关闭且绕过缓存(见 9.1),防敏感文本外泄。

### 9.5 观测
- 日志: `tracing` 结构化,含 request_id、catalog_version、weights_version、backend、latency、cache_hit、degraded。
- 指标(可选开关): `recommend_requests_total{result}`, `backend_latency_seconds` (histogram), `cache_hit_ratio`, `degraded_total`, `backend_429_total`, `fusion_score` (histogram)。
- **审计日志**(独立文件/输出流,JSONL): `{ts, request_id, profile_hash, constraints, weights_version, catalog_version, top_k: [...], degraded, usage}`。是 v1.1 标注回路与 shadow 评估的原料。

### 9.6 本地部署与试用(默认场景)

- **形态**: `cargo build --release` 产物为单二进制;本地直接运行(容器化仅作可选补充):
  ```bash
  recommender-cli validate-catalog --configs configs/                   # 1. 校验配置
  recommender-cli dry-run --request examples/sample-request.json --configs configs/   # 2. 不联网:查看将发给 Jev 的完整问题
  TYPESAFE_API_KEY=xxx HTTPS_PROXY=http://127.0.0.1:7897 \
      recommender-server --configs configs/            # 3. 本地启动服务(区域限制时需代理,见下)
  curl -s http://127.0.0.1:8787/v1/recommend \
       -H 'content-type: application/json' -d @examples/sample-request.json  # 4. 端到端拿 top5 + 理由
  ```
- **浏览器调试台**: 服务启动后打开 `http://127.0.0.1:8787/debug` 可视化跑通推荐(见 9.7)。
- **无 key / 断网行为**: 服务照常启动并应答;Jev 调用失败(如缺 key → 401)按 8.2 降级为粗排 + `degraded: true`,规则/融合/理由/审计链路仍可完整观察。
- **区域限制(451)**: 大陆等地区直连 `api.typesafe.ai` 会被拒(451);给服务配置代理即可(server.yaml backend `proxy`,或启动时 `HTTPS_PROXY`),已实测经代理 HTTP 200。
- **数据出境提示**: 语义判断会把清洗、截断后的请求摘要与候选目录(不含 key)发送至 `api.typesafe.ai`;敏感数据试用请用降级模式,或走 13 节的纯本地 Laya 路线(v1.1)。

### 9.7 内嵌调试页(`GET /debug`)

- **定位**: 开发/试用工具,非产品 UI(见 2 节);零构建、无外部依赖的单 HTML(原生 JS/CSS),构建时经 `include_str!` 内嵌进二进制(源码在 `crates/server/assets/debug.html`)。
- **功能**:
  - 预填样例请求(与 `examples/sample-request.json` 同构),可粘贴任意 OpenAI/Claude 风格请求体;
  - 约束表单: `top_k`、`locale`、预算上限、`cost_sensitive`、`require_tags`、`vendor_allow/deny`;
  - "推荐"按钮 → `POST /v1/recommend`;结果卡片(rank/score/confidence/low_confidence/reasons/breakdown)+ 原始 JSON 切换;
  - 顶部 meta 横幅(backend / degraded / cache_hit / latency),方便观察降级与缓存行为;
  - 鉴权: 若配置了调用方 token,页面提供输入框(仅存本机 localStorage)。
- **安全**: 同源请求,无需 CORS;不新增权限面(复用调用方鉴权规则,含 9.4 的本机限制);可由 `debug_page_enabled` 关闭。

---

## 10. 测试与评估

| 层 | 工具 | 内容 |
|---|---|---|
| 单元 | `rstest` + `insta` | 画像提取(OpenAI/Claude、图片/工具/json_mode/边界/非法体)、每条规则真值表、权重叠加与归一化 golden 数值、融合 tie-break、理由渲染快照(zh/en) |
| 契约 | `wiremock` | 发出请求形状断言(问题数/ID/档位结构/state 结构/token 预算),429→重试→成功、429→重试→降级、超时→降级、缺答案→中性值 |
| Golden E2E | 录制响应 fixtures | 离线全链路: 固定目录 + 固定响应 → 排序/理由/breakdown 快照 |
| 评估集 | `cli eval` | 场景 YAML: `{id, request(fixture), constraints, expected_top5[], must_not_include[], notes}`;指标: top-5 召回、MRR/nDCG、top1 错误率、降级率、p50/p95 延迟、单次 input_tokens 成本 |
| Shadow 回放 | `cli eval --shadow` | 用 new-api 导出的历史 usage 日志生成场景(请求体 + 实际使用模型),比较推荐 top1 与实际;输出一致率与不一致样本 |
| 校准 | 审计日志分析 | 置信度分桶 vs 实际认可率;验证 `flag_below` 门槛 |

**v1 验收标准**:
1. golden 场景 ≥30 个且 `eval` 可一键跑;
2. 标注集 top-5 召回 ≥90%,`must_not_include` 零违反;
3. 端到端 p95 ≤1.2s(含后端;降级路径 ≤10ms);
4. CI: 全部单测/契约测试/golden 通过;`validate-catalog` 对配置 0 错误。

**版本钉死与回归**: `server.yaml` 钉 `jev-1.13.0`;升级流程 = 改配置 → 跑 `eval` 对比 → 通过才发布。

---

## 11. 里程碑

| 阶段 | 内容 | 交付物 | 估计 |
|---|---|---|---|
| M0 骨架 | workspace、四份配置、catalog 加载+校验、规则引擎、API 薄壳、mock 后端、单测 | `dry-run` CLI 对样例请求打印完整待发问题(不联网) | 0.5–1 天 |
| M1 核心 | systemone 客户端、提问构造、融合、理由渲染、/debug 调试页、真实联调 | `curl` 或 /debug 调试页端到端出 top5+理由;golden fixtures 通过 | 1–2 天 |
| M2 运维 | 缓存、降级、重试、指标/日志、eval 工具、本地部署与试用引导(9.6)、Docker(可选) | 验收标准 1–4 全过;本地试用手册(四步 + curl)可用 | 1 天 |
| M3 集成 | new-api 旁路接入示例(文档+最小 demo)、shadow 回放跑通 | 集成说明 + 首轮 shadow 报告 | 按需 |

---

## 12. 风险与对策

| 风险 | 影响 | 对策 |
|---|---|---|
| Jev 无 SLA/限流漂移 | 服务不可用 | 重试+降级(确定性排序)+ 契约兼容备用后端(Laya/KEV 换 base_url);`max_inflight` 保护配额 |
| TypeSafe 区域限制(451) | 云 Jev 直连不可用 | 后端 `proxy` 配置(已实测可用);治本是 13 节的本地 Laya |
| 权重/档位未标定 | 排序不符合直觉 | `questions.yaml` 数据化;评估集 + shadow 回放闭环调参;门槛(flag_below)在自有数据上标定 |
| 用户文本注入 state | 判断被操纵 | state 只作为数据、指令只在 questions、清洗+截断+专用字段(9.4) |
| 候选过多超 token/上下文 | 请求失败 | 分片并行(>30,每片 ≤30)+ 硬上限截断(>60,先粗排截断);token 预算公式内建(6.3) |
| 目录漂移(价格/能力过期) | 错误推荐 | `validate-catalog` + 内容 hash 版本 + 审计可追溯 |
| 决策模型版本行为变化 | 排序漂移 | 钉死版本 ID;升级走 eval 回归 |

---

## 13. 后续(v1.1+,不在本计划内)

- 审计日志 → 标注回路 → 学习型粗排(参考 new-api "优先级分层+加权随机"的确定性调度思路)。
- 置信度门槛联动: 低置信度自动放宽 top_k / 返回两个候选对比说明。
- 本地 Laya backend profile(彻底摆脱区域限制,纯本地真实判断)+ 校准报告。
- new-api `CatalogProvider`(从 `/api/pricing` 同步)。
- 蒸馏: 把稳定高频判断训练为本地小分类器(feature = 画像字段)。
