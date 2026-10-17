# fixtures

- `jev_code_debug.json`:`/v1/systemone` 原始响应(录制或合成),供 `crates/core/tests/golden_e2e.rs` 回放。
- 重录/合成命令见 `docs/superpowers/plans/2026-10-16-model-recommender-core.md` Task 20。
- **绑定关系**:夹具的问题 ID 集合由 `configs/model_list.yaml`(候选集)+ `configs/questions.yaml` 决定;
  目录或问题变更后 golden 测试会以"夹具缺答案: cN.xxx"报错 —— 重跑 Step 2 重录/重合成。
- 合成夹具以 `"model": "synthetic-fixture"` 区分;真实录制为端点返回的 model 名。
