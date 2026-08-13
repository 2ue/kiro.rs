# Source excerpts

These snippets were extracted from running image revision:

`3bfab8c9dc138062cad3c3cd1682c410bd6a263b`

They are included so the RCA can be reviewed without access to the original
source tree at `/root/code/kiro.rs`.

Files:

- `storage_task_650_710.txt`
  - storage executor handle selection and `block_on_storage()`.
- `manager_redis_blocking_paths_3388_3575.txt`
  - synchronous Redis scheduler bridges using `block_on_storage()`.
- `manager_redis_hot_paths_4235_4455.txt`
  - Redis scheduler selection and in-flight lease hot path.
- `manager_persist_success_8735_8805.txt`
  - synchronous PgSQL success persistence.
- `manager_report_success_9440_9750.txt`
  - report success/session cleanup path.
- `provider_api_rate_limit_176_205.txt`
  - upstream/provider error kind to scheduler reason mapping.
- `provider_stream_completion_854_980.txt`
  - stream completion success/soft failure/drop behavior.
- `handlers_sse_builder_7025_7051.txt`
  - SSE body creation and ping/idle constants.
- `handlers_sse_unfold_7596_7651.txt`
  - SSE stream polling path.
- `handlers_stream_terminal_8060_8205.txt`
  - stream terminal success/error handling.
- `usage_writer_1605_1818.txt`
  - usage writer queue behavior.
- `main_server_health_selected.txt`
  - selected main server/health lines.
