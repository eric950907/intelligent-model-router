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
    fn id(&self) -> &str {
        "replay"
    }
    fn judge<'a>(
        &'a self,
        req: JudgeRequest,
    ) -> futures::future::BoxFuture<'a, Result<JudgeResponse, JudgeError>> {
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
    let fixture: JudgeResponse =
        serde_json::from_str(&fixture_text).expect("夹具必须是合法 JudgeResponse JSON");

    let backend = Arc::new(ReplayBackend {
        fixture: fixture.clone(),
        seen: Mutex::new(vec![]),
    });
    let renderer = TemplateReasonRenderer::from_dir(&root.join(&cfg.server.templates_dir))
        .expect("理由模板必须可加载");
    let catalog: Arc<dyn CatalogProvider> = Arc::new(cfg.catalog.clone());
    let pipeline = Pipeline::new(
        &cfg,
        catalog,
        backend.clone(),
        Arc::new(renderer),
        "jev-1.13.0".into(),
    );

    let mut req: RecommendRequest = serde_json::from_str(
        &std::fs::read_to_string(root.join("examples/sample-request.json")).expect("样例请求缺失"),
    )
    .expect("样例请求必须是合法信封");
    req.options.locale = locale;

    let resp = pipeline
        .recommend(&req, "golden-fixed-id")
        .await
        .expect("recommend 不应失败");

    // 夹具必须覆盖管线实际发出的全部问题 ID(目录/问题变更 → 在此暴露,重录夹具)
    let seen = backend.seen.lock().unwrap();
    assert_eq!(seen.len(), 1, "示例目录(11 候选)应单片发送");
    for id in seen[0].questions.keys() {
        assert!(
            fixture.answers.contains_key(id),
            "夹具缺答案:{id}(重录或重新合成)"
        );
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
