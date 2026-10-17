//! /v1/systemone 契约客户端(spec §7): 鉴权 / 代理 / 超时 / 错误映射。
//! 单次尝试;重试/总预算/信号量属 M2。

use futures::future::BoxFuture;
use recommender_core::judgment::{JudgeError, JudgeRequest, JudgeResponse, JudgmentBackend};
use std::time::Duration;

#[derive(Clone)]
pub struct ClientConfig {
    pub id: String,
    pub base_url: String,
    pub model: String,
    pub api_key: Option<String>,
    pub proxy: Option<String>,
    pub timeout_ms: u64,
    pub long_context_header: bool,
}

impl std::fmt::Debug for ClientConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClientConfig")
            .field("id", &self.id)
            .field("base_url", &self.base_url)
            .field("model", &self.model)
            .field("api_key", &self.api_key.as_ref().map(|_| "<redacted>"))
            // 代理 URL 常内嵌 user:pass,整体脱敏(实际值仍原样传给 reqwest)。
            .field("proxy", &self.proxy.as_ref().map(|_| "<configured>"))
            .field("timeout_ms", &self.timeout_ms)
            .field("long_context_header", &self.long_context_header)
            .finish()
    }
}

/// 拼接错误自身及其 `source` 链,避免只保留最外层的 reqwest 消息。
fn error_chain(err: &dyn std::error::Error) -> String {
    let mut out = err.to_string();
    let mut source = err.source();
    while let Some(e) = source {
        out.push_str(": ");
        out.push_str(&e.to_string());
        source = e.source();
    }
    out
}

impl ClientConfig {
    /// 显式配置优先;否则沿用环境变量(大小写各试)。spec §5.4。
    pub fn effective_proxy(&self) -> Option<String> {
        if let Some(p) = &self.proxy {
            if !p.trim().is_empty() {
                return Some(p.clone());
            }
        }
        for k in ["HTTPS_PROXY", "https_proxy", "ALL_PROXY", "all_proxy"] {
            if let Ok(v) = std::env::var(k) {
                if !v.trim().is_empty() {
                    return Some(v);
                }
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
        if cfg.timeout_ms == 0 {
            return Err(JudgeError::Connect("timeout_ms 必须 > 0".into()));
        }
        let mut builder = reqwest::Client::builder()
            .connect_timeout(Duration::from_millis(1000))
            .timeout(Duration::from_millis(cfg.timeout_ms))
            .user_agent(concat!("model-recommender/", env!("CARGO_PKG_VERSION")));
        if let Some(proxy) = cfg.effective_proxy() {
            builder =
                builder.proxy(reqwest::Proxy::all(&proxy).map_err(|e| {
                    JudgeError::Connect(format!("代理配置无效: {}", error_chain(&e)))
                })?);
        }
        let http = builder
            .build()
            .map_err(|e| JudgeError::Connect(error_chain(&e)))?;
        Ok(Self { cfg, http })
    }

    pub fn model(&self) -> &str {
        &self.cfg.model
    }
}

impl JudgmentBackend for SystemoneClient {
    fn id(&self) -> &str {
        &self.cfg.id
    }

    fn judge<'a>(
        &'a self,
        mut req: JudgeRequest,
    ) -> BoxFuture<'a, Result<JudgeResponse, JudgeError>> {
        Box::pin(async move {
            let key = self
                .cfg
                .api_key
                .as_deref()
                .filter(|k| !k.trim().is_empty())
                .ok_or(JudgeError::MissingKey)?;
            req.model = self.cfg.model.clone();

            let url = format!("{}/v1/systemone", self.cfg.base_url.trim_end_matches('/'));
            let mut rb = self.http.post(&url).bearer_auth(key).json(&req);
            if self.cfg.long_context_header {
                rb = rb.header("TypeSafe-Danger-Zone-Long-Context", "true");
            }

            let resp = rb.send().await.map_err(|e| {
                if e.is_timeout() {
                    JudgeError::Timeout
                } else {
                    JudgeError::Connect(error_chain(&e))
                }
            })?;
            let status = resp.status();
            if !status.is_success() {
                let body = match resp.text().await {
                    Ok(b) => b,
                    Err(e) if e.is_timeout() => return Err(JudgeError::Timeout),
                    Err(_) => String::new(),
                };
                return Err(JudgeError::Http {
                    status: status.as_u16(),
                    body: body.chars().take(500).collect(),
                });
            }
            let bytes = resp.bytes().await.map_err(|e| {
                if e.is_timeout() {
                    JudgeError::Timeout
                } else {
                    JudgeError::Decode(e.to_string())
                }
            })?;
            serde_json::from_slice::<JudgeResponse>(&bytes)
                .map_err(|e| JudgeError::Decode(e.to_string()))
        })
    }
}
