//! backend 注册表(spec §4.3 D5/§5.4): kind → JudgmentBackend;列表首个为默认。
//! 缺 key 不阻止启动:调用时返回 MissingKey → 走降级(spec §9.6)。
//! key/proxy 在启动时快照;轮换环境变量需重启进程。

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
    pub fn default_backend(&self) -> Arc<dyn JudgmentBackend> {
        self.backends[0].clone()
    }
    pub fn default_model_hint(&self) -> &str {
        &self.model_hints[0]
    }
}

pub fn build_backends(specs: &[BackendSpec]) -> Result<BuiltBackends, String> {
    let mut backends: Vec<Arc<dyn JudgmentBackend>> = vec![];
    let mut model_hints = vec![];
    for s in specs {
        match s.kind {
            BackendKind::Systemone => {
                let api_key = s.api_key_env.as_deref().and_then(|n| std::env::var(n).ok());
                if api_key
                    .as_deref()
                    .map(|k| k.trim().is_empty())
                    .unwrap_or(true)
                {
                    tracing::warn!(
                        backend = %s.id,
                        env = s.api_key_env.as_deref().unwrap_or(""),
                        "API key 未设置,该后端将始终走降级"
                    );
                }
                let client = SystemoneClient::new(ClientConfig {
                    id: s.id.clone(),
                    base_url: s.base_url.clone().unwrap_or_default(),
                    model: s.model.clone().unwrap_or_default(),
                    api_key,
                    proxy: s.proxy.clone(),
                    timeout_ms: s.timeout_ms,
                    long_context_header: s.long_context_header,
                })
                .map_err(|e| format!("backend `{}` 初始化失败: {e}", s.id))?;
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
    Ok(BuiltBackends {
        backends,
        model_hints,
    })
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
        assert_eq!(built.default_model_hint(), "mock-mock");
        let resp = built
            .default_backend()
            .judge(JudgeRequest {
                model: String::new(),
                state: serde_json::json!({}),
                questions: Default::default(),
            })
            .await
            .unwrap();
        assert_eq!(resp.model, "mock-1");
    }

    #[tokio::test]
    async fn systemone_without_key_still_builds_but_judge_errors() {
        let env_name = format!("TEST_KEY_UNSET_{}", uuid::Uuid::new_v4().simple());
        let built = build_backends(&[systemone_spec("jev", &env_name)]).unwrap();
        let err = built
            .default_backend()
            .judge(JudgeRequest {
                model: String::new(),
                state: serde_json::json!({}),
                questions: Default::default(),
            })
            .await
            .unwrap_err();
        assert!(matches!(err, JudgeError::MissingKey));
    }

    #[tokio::test]
    async fn empty_list_rejected() {
        assert!(build_backends(&[]).is_err());
    }

    #[test]
    fn first_backend_is_default_and_hints_align() {
        let env_name = format!("TEST_KEY_UNSET_{}", uuid::Uuid::new_v4().simple());
        let built = build_backends(&[mock_spec("mock"), systemone_spec("jev", &env_name)]).unwrap();
        assert_eq!(built.backends.len(), built.model_hints.len());
        assert_eq!(
            built.model_hints,
            vec!["mock-mock".to_string(), "jev-1.13.0".to_string()]
        );
        assert_eq!(built.default_backend().id(), "mock"); // 列表首个 = 默认
        assert_eq!(built.default_model_hint(), "mock-mock");
    }

    #[test]
    fn client_build_failure_is_an_error() {
        let mut spec = systemone_spec("jev", "IGNORED");
        spec.proxy = Some("::::".into()); // reqwest 无法解析的代理 URL
        assert!(build_backends(&[spec]).is_err());

        let mut spec = systemone_spec("jev", "IGNORED");
        spec.timeout_ms = 0;
        assert!(build_backends(&[spec]).is_err());
    }
}
