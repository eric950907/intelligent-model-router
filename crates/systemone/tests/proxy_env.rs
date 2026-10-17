//! 代理环境变量回退(spec §5.4)。独立测试二进制:本文件只有一个测试,
//! 因此 set_var/remove_var 不会与其他测试并发。

use recommender_systemone::ClientConfig;

fn cfg(proxy: Option<String>) -> ClientConfig {
    ClientConfig {
        id: "jev".into(),
        base_url: "http://127.0.0.1:1".into(),
        model: "jev-1.13.0".into(),
        api_key: None,
        proxy,
        timeout_ms: 1000,
        long_context_header: false,
    }
}

#[test]
fn falls_back_to_env_and_prefers_explicit() {
    std::env::set_var("HTTPS_PROXY", "http://127.0.0.1:9");

    // 显式空白 → 回退环境变量
    assert_eq!(
        cfg(Some("   ".into())).effective_proxy().as_deref(),
        Some("http://127.0.0.1:9")
    );
    // 未配置 → 回退环境变量
    assert_eq!(
        cfg(None).effective_proxy().as_deref(),
        Some("http://127.0.0.1:9")
    );
    // 显式非空优先于环境变量
    assert_eq!(
        cfg(Some("http://explicit.proxy:8080".into()))
            .effective_proxy()
            .as_deref(),
        Some("http://explicit.proxy:8080")
    );

    std::env::remove_var("HTTPS_PROXY");
}
