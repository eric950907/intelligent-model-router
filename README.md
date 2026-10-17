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
