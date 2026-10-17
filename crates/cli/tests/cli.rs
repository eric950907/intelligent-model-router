//! CLI 黑盒测试:CARGO_BIN_EXE 定位二进制;不依赖 examples/(Task 20 才创建)。
use std::path::{Path, PathBuf};
use std::process::Command;

fn repo(path: &str) -> PathBuf {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    if path.is_empty() {
        root
    } else {
        root.join(path)
    }
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
    let out = cli()
        .args(["validate-catalog", "--configs", "configs"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "exit={:?}\nstdout={stdout}",
        out.status.code()
    );
    assert!(stdout.contains("收录 11"), "got: {stdout}");
    assert!(stdout.contains("排除 3"), "got: {stdout}");
    assert!(stdout.contains("mode=embedding×1"), "got: {stdout}");
    assert!(stdout.contains("依赖默认富化 0"), "got: {stdout}");
}

#[test]
fn validate_catalog_errors_exit_2() {
    let dir = std::env::temp_dir().join("mrc-cli-empty-configs");
    let _ = std::fs::create_dir_all(&dir);
    let out = cli()
        .args(["validate-catalog", "--configs"])
        .arg(&dir)
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(2),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(String::from_utf8_lossy(&out.stderr).contains("server.yaml"));
}

#[test]
fn dry_run_prints_all_questions() {
    let req = write_min_request();
    let out = cli()
        .args(["dry-run", "--configs", "configs"])
        .arg("--request")
        .arg(&req)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        stdout.contains("task_type") && stdout.contains("frontier_needed"),
        "got: {stdout}"
    );
    assert!(
        stdout.contains("c10.quality_fit"),
        "11 个候选应全部入片;got: {stdout}"
    );
    assert!(stdout.contains("request_excerpt"), "got: {stdout}");
}

#[test]
fn dry_run_raw_shard_is_curl_ready() {
    let req = write_min_request();
    let out = cli()
        .args(["dry-run", "--configs", "configs"])
        .arg("--request")
        .arg(&req)
        .args(["--raw", "--shard", "0"])
        .output()
        .unwrap();
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
    let out = cli()
        .args(["dry-run", "--configs", "configs"])
        .arg("--request")
        .arg(&req)
        .args(["--raw", "--shard", "9"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("分片 9 不存在"));
}
