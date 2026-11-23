#!/usr/bin/env python3
"""按路由汇总测试库 usage_records：sub2api 口径命中率、input 分布、积分转换率、计费比。

用法: analyze.py <database> [since]
例如: analyze.py kiro_minehigh_20261020 "2026-10-20 03:30:00+00"
"""
import json
import subprocess
import sys

DB = sys.argv[1] if len(sys.argv) > 1 else "kiro_minehigh_20261020"
SINCE = sys.argv[2] if len(sys.argv) > 2 else "2026-10-20 00:00:00+00"

HIT = ("case when cache_read_input_tokens > 0 then cache_read_input_tokens::float / "
       "nullif(compat_input_tokens + cache_read_input_tokens + cache_creation_input_tokens, 0) end")

SQL = f"""
\\pset format unaligned
\\pset tuples_only on
select json_agg(t) from (
  select split_part(endpoint, '/v1', 1) as route,
         sum((status = 'success')::int) as ok,
         sum(compat_input_tokens)::bigint as input,
         sum(cache_read_input_tokens)::bigint as read,
         sum(cache_creation_input_tokens)::bigint as creation,
         sum(estimated_cost_usd) as est,
         sum(original_cost_usd) as orig,
         sum(kiro_metering_usage) as credits,
         percentile_cont(0.1) within group (order by compat_input_tokens) as in_p10,
         percentile_cont(0.5) within group (order by compat_input_tokens) as in_p50,
         percentile_cont(0.9) within group (order by compat_input_tokens) as in_p90,
         max(compat_input_tokens) as in_max,
         sum((compat_input_tokens > 3300)::int) as in_over_3300,
         percentile_cont(0.1) within group (order by {HIT}) as hit_p10,
         percentile_cont(0.5) within group (order by {HIT}) as hit_p50,
         percentile_cont(0.9) within group (order by {HIT}) as hit_p90,
         sum((cache_read_input_tokens = 0)::int) as cold
  from usage_records
  where deleted_at is null and created_at >= '{SINCE}' and endpoint like '/dfcache/%'
    and status = 'success'
  group by 1 order by 1
) t;
"""

raw = subprocess.run(
    ["docker", "exec", "-i", "kiro-rs-postgres-local", "psql", "-U", "kiro_rs", "-d", DB],
    input=SQL, capture_output=True, text=True, check=True,
).stdout
# psql 会把较长的 JSON 折成多行，按首尾方括号截取整段再解析。
rows = json.loads(raw[raw.index("["):raw.rindex("]") + 1])

fmt = lambda v: f"{v:.3f}" if v is not None else "-"
print("路由 | 成功 | 冷 | 汇总命中率 | 单请求命中 p10/p50/p90 | input p10/p50/p90/max | >3300 | 积分 | 积分转换率 | 原始 | 计费比")
for r in rows:
    prompt = r["input"] + r["read"] + r["creation"]
    hit = r["read"] / prompt if prompt else 0
    credits = r["credits"] or 0
    rate = r["est"] / credits if credits else float("nan")
    orig_rate = r["orig"] / credits if credits else float("nan")
    ratio = r["est"] / r["orig"] if r["orig"] else float("nan")
    print(f"{r['route']} | {r['ok']} | {r['cold']} | {hit:.2%} | "
          f"{fmt(r['hit_p10'])}/{fmt(r['hit_p50'])}/{fmt(r['hit_p90'])} | "
          f"{int(r['in_p10'])}/{int(r['in_p50'])}/{int(r['in_p90'])}/{r['in_max']} | "
          f"{r['in_over_3300']} | {credits:.1f} | {rate:.3f} | {orig_rate:.3f} | {ratio:.2f}")
print()
print("积分转换率 = 下发 usage 估算费用 / Kiro 消耗积分（美元/积分）；原始 = 原始 usage 费用 / 积分；计费比 = 估算 / 原始")
