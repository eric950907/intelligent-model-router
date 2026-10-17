//! /v1/systemone wiremock 契约测试:请求形状、错误映射、超时、坏响应体、缺 key、代理优先级。

use recommender_core::config::QuestionType;
use recommender_core::judgment::{JudgeError, JudgeRequest, JudgmentBackend, Question};
use recommender_systemone::{ClientConfig, SystemoneClient};
use serde_json::json;
use std::collections::BTreeMap;
use std::time::Duration;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const MODEL: &str = "jev-1.13.0";

/// 进程级锁:env 只影响客户端构造,锁住"清空代理 env + 构造客户端"即可无竞态。
static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn build_client(cfg: ClientConfig) -> SystemoneClient {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    for k in [
        "HTTPS_PROXY",
        "https_proxy",
        "ALL_PROXY",
        "all_proxy",
        "HTTP_PROXY",
        "http_proxy",
    ] {
        std::env::remove_var(k);
    }
    SystemoneClient::new(cfg).unwrap()
}

fn sample_request() -> JudgeRequest {
    let questions = BTreeMap::from([
        (
            "task_type".to_string(),
            Question {
                qtype: QuestionType::Choice,
                instructions: json!("判断任务类型"),
                criteria: Some(json!({"chat": null, "code": null})),
            },
        ),
        (
            "c0.quality_fit".to_string(),
            Question {
                qtype: QuestionType::Score,
                instructions: json!("质量适配度"),
                criteria: Some(json!(["低", "中", "高"])),
            },
        ),
        (
            "c0.cost_fit".to_string(),
            Question {
                qtype: QuestionType::Score,
                instructions: json!("成本适配度"),
                criteria: Some(json!(["低", "中", "高"])),
            },
        ),
    ]);
    JudgeRequest {
        model: String::new(),
        state: json!({"profile": {}, "candidates": []}),
        questions,
    }
}

fn sample_response() -> serde_json::Value {
    json!({
        "model": MODEL,
        "answers": {
            "task_type": {"type": "choice", "choice": "chat"},
            "c0.quality_fit": {"type": "score", "score": 2.0}
        },
        "usage": {"input_tokens": 12, "output_tokens": 3}
    })
}

fn client_for(server: &MockServer) -> SystemoneClient {
    build_client(ClientConfig {
        id: "jev".into(),
        base_url: server.uri(),
        model: MODEL.into(),
        api_key: Some("test-key".into()),
        proxy: Some(String::new()),
        timeout_ms: 5000,
        long_context_header: false,
    })
}

fn client_with(server: &MockServer, long_context_header: bool, timeout_ms: u64) -> SystemoneClient {
    build_client(ClientConfig {
        id: "jev".into(),
        base_url: server.uri(),
        model: MODEL.into(),
        api_key: Some("test-key".into()),
        proxy: Some(String::new()),
        timeout_ms,
        long_context_header,
    })
}

async fn mount_ok(server: &MockServer) {
    Mock::given(method("POST"))
        .and(path("/v1/systemone"))
        .respond_with(ResponseTemplate::new(200).set_body_json(sample_response()))
        .mount(server)
        .await;
}

#[tokio::test]
async fn sends_expected_request_shape_and_parses_response() {
    let server = MockServer::start().await;
    mount_ok(&server).await;

    let client = client_for(&server);
    assert_eq!(client.model(), MODEL);

    let resp = client.judge(sample_request()).await.expect("judge 失败");
    assert_eq!(resp.model, MODEL);
    assert_eq!(resp.answers["task_type"]["choice"], "chat");
    assert_eq!(resp.answers["c0.quality_fit"]["score"], 2.0);
    let usage = resp.usage.expect("usage 缺失");
    assert_eq!((usage.input_tokens, usage.output_tokens), (12, 3));

    let reqs = server.received_requests().await.unwrap();
    assert_eq!(reqs.len(), 1);
    let req = &reqs[0];

    assert_eq!(
        req.headers.get("authorization").unwrap().to_str().unwrap(),
        "Bearer test-key"
    );

    let body: serde_json::Value = serde_json::from_slice(&req.body).expect("请求体不是 JSON");
    // 客户端覆盖 pipeline 构造时空的 model。
    assert_eq!(body["model"], MODEL);
    assert!(body.get("state").is_some());
    assert_eq!(body["state"]["profile"], json!({}));
    assert_eq!(body["state"]["candidates"], json!([]));
    assert_eq!(body["questions"]["task_type"]["type"], "choice");
    assert!(body["questions"]["c0.quality_fit"].is_object());
    assert!(body["questions"]["c0.cost_fit"].is_object());
}

#[tokio::test]
async fn maps_http_error_status() {
    for status in [451u16, 429, 401] {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/systemone"))
            .respond_with(ResponseTemplate::new(status).set_body_string("nope"))
            .mount(&server)
            .await;

        let err = client_for(&server)
            .judge(sample_request())
            .await
            .expect_err("应返回 HTTP 错误");
        match err {
            JudgeError::Http { status: got, body } => {
                assert_eq!(got, status);
                assert_eq!(body, "nope");
            }
            other => panic!("期望 Http{{status:{status}}}，实际 {other:?}"),
        }
    }
}

#[tokio::test]
async fn truncates_error_body_to_500_chars() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/systemone"))
        .respond_with(ResponseTemplate::new(451).set_body_string("x".repeat(600)))
        .mount(&server)
        .await;

    let err = client_for(&server)
        .judge(sample_request())
        .await
        .expect_err("应返回 HTTP 错误");
    match err {
        JudgeError::Http { status, body } => {
            assert_eq!(status, 451);
            assert_eq!(body.chars().count(), 500);
        }
        other => panic!("期望 Http{{status:451}}，实际 {other:?}"),
    }
}

#[tokio::test]
async fn maps_timeout() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/systemone"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_delay(Duration::from_millis(300))
                .set_body_json(sample_response()),
        )
        .mount(&server)
        .await;

    let err = client_with(&server, false, 100)
        .judge(sample_request())
        .await
        .expect_err("应超时");
    assert!(matches!(err, JudgeError::Timeout), "实际 {err:?}");
}

#[tokio::test]
async fn body_read_timeout_maps_to_timeout() {
    use std::io::{Read, Write};

    // 先读完请求再发响应头,随后挂起不发送 body:send() 已返回,超时发生在 body 读取阶段。
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind 失败");
    let addr = listener.local_addr().expect("local_addr 失败");
    let _server_thread = std::thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            let mut buf = [0u8; 4096];
            let _ = stream.read(&mut buf);
            let _ = stream.write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 100\r\n\r\n");
            let _ = stream.flush();
            std::thread::sleep(Duration::from_secs(2));
        }
    });

    let client = build_client(ClientConfig {
        id: "jev".into(),
        base_url: format!("http://{addr}"),
        model: MODEL.into(),
        api_key: Some("test-key".into()),
        proxy: Some(String::new()),
        timeout_ms: 150,
        long_context_header: false,
    });

    let err = client
        .judge(sample_request())
        .await
        .expect_err("body 读取应超时");
    assert!(matches!(err, JudgeError::Timeout), "实际 {err:?}");
}

#[tokio::test]
async fn connection_refused_maps_to_connect() {
    let client = build_client(ClientConfig {
        id: "jev".into(),
        base_url: "http://127.0.0.1:1".into(),
        model: MODEL.into(),
        api_key: Some("test-key".into()),
        proxy: Some(String::new()),
        timeout_ms: 200,
        long_context_header: false,
    });

    let err = client
        .judge(sample_request())
        .await
        .expect_err("应返回 Connect");
    assert!(matches!(err, JudgeError::Connect(_)), "实际 {err:?}");
}

#[tokio::test]
async fn maps_bad_response_body_to_decode() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/systemone"))
        .respond_with(ResponseTemplate::new(200).set_body_string("not json"))
        .mount(&server)
        .await;

    let err = client_for(&server)
        .judge(sample_request())
        .await
        .expect_err("应返回 Decode");
    assert!(matches!(err, JudgeError::Decode(_)), "实际 {err:?}");
}

#[tokio::test]
async fn missing_key_fails_before_sending() {
    let server = MockServer::start().await;
    mount_ok(&server).await;

    let client = build_client(ClientConfig {
        id: "jev".into(),
        base_url: server.uri(),
        model: MODEL.into(),
        api_key: None,
        proxy: Some(String::new()),
        timeout_ms: 5000,
        long_context_header: false,
    });

    let err = client
        .judge(sample_request())
        .await
        .expect_err("应返回 MissingKey");
    assert!(matches!(err, JudgeError::MissingKey), "实际 {err:?}");
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn blank_key_fails_before_sending() {
    let server = MockServer::start().await;
    mount_ok(&server).await;

    let client = build_client(ClientConfig {
        id: "jev".into(),
        base_url: server.uri(),
        model: MODEL.into(),
        api_key: Some("   ".into()),
        proxy: Some(String::new()),
        timeout_ms: 5000,
        long_context_header: false,
    });

    let err = client
        .judge(sample_request())
        .await
        .expect_err("应返回 MissingKey");
    assert!(matches!(err, JudgeError::MissingKey), "实际 {err:?}");
}

#[test]
fn explicit_proxy_wins() {
    let cfg = ClientConfig {
        id: "jev".into(),
        base_url: "http://127.0.0.1:1".into(),
        model: MODEL.into(),
        api_key: None,
        proxy: Some("http://explicit.proxy:8080".into()),
        timeout_ms: 1000,
        long_context_header: false,
    };
    assert_eq!(
        cfg.effective_proxy().as_deref(),
        Some("http://explicit.proxy:8080")
    );

    // 首尾空白不影响显式值(仅全空白才回退环境变量)。
    let padded = ClientConfig {
        proxy: Some("  http://explicit.proxy:8080  ".into()),
        ..cfg
    };
    assert_eq!(
        padded.effective_proxy().as_deref(),
        Some("  http://explicit.proxy:8080  ")
    );
}

#[tokio::test]
async fn sends_long_context_header_when_enabled() {
    let server = MockServer::start().await;
    mount_ok(&server).await;

    client_with(&server, true, 5000)
        .judge(sample_request())
        .await
        .expect("judge 失败");

    let reqs = server.received_requests().await.unwrap();
    assert_eq!(
        reqs[0]
            .headers
            .get("typesafe-danger-zone-long-context")
            .expect("缺少 long-context 头")
            .to_str()
            .unwrap(),
        "true"
    );
}

#[tokio::test]
async fn omits_long_context_header_by_default() {
    let server = MockServer::start().await;
    mount_ok(&server).await;

    client_for(&server)
        .judge(sample_request())
        .await
        .expect("judge 失败");

    let reqs = server.received_requests().await.unwrap();
    assert!(reqs[0]
        .headers
        .get("typesafe-danger-zone-long-context")
        .is_none());
}

#[tokio::test]
async fn trims_trailing_slash_from_base_url() {
    let server = MockServer::start().await;
    mount_ok(&server).await;

    let client = build_client(ClientConfig {
        id: "jev".into(),
        base_url: format!("{}/", server.uri()),
        model: MODEL.into(),
        api_key: Some("test-key".into()),
        proxy: Some(String::new()),
        timeout_ms: 5000,
        long_context_header: false,
    });

    client
        .judge(sample_request())
        .await
        .expect("带尾斜杠的 base_url 也应命中 /v1/systemone");
}
