#!/usr/bin/env python3
"""从 59140 只读导出运行配置，脱敏改写成本地隔离压测用的 config.json（不连接任何线上存储）。

用法：python3 sanitize_config.py <输出路径> [--real-upstream]
默认把上游指向本地假 Kiro（kirobench-fake:19080）；--real-upstream 用于真实上游签名验证。
导出的 cachePolicy 等业务配置约 1.2MB，原样保留以复现 CPU 开销；所有密钥、存储地址被替换。
"""
import json
import subprocess
import sys


def runtime_config():
    out = subprocess.run(
        ["docker", "exec", "-i", "kiro-rs-forced-59140-postgres", "sh", "-c",
         'psql -v ON_ERROR_STOP=1 -U "$POSTGRES_USER" -d "$POSTGRES_DB" -t -A'],
        input="SET default_transaction_read_only = on;\nSELECT config FROM runtime_config WHERE id='default';",
        text=True, capture_output=True, check=True).stdout
    return json.loads([line for line in out.splitlines() if line.startswith("{")][0])


def main():
    target = sys.argv[1]
    real = "--real-upstream" in sys.argv
    c = runtime_config()
    c["apiKey"] = "sk-kirobench-local-0001"
    c["apiKeys"] = []
    c["adminApiKey"] = "sk-admin-kirobench-local"
    c["requestApiKeyPolicies"] = []
    for key in ["countTokensApiKey", "countTokensApiUrl", "proxyUrl", "proxyUsername", "proxyPassword", "machineId"]:
        c[key] = None
    db = "kirobench_real" if real else "kirobench"
    c["postgres"] = {"url": f"postgres://kirobench:kirobench@kirobench-pg:5432/{db}",
                     "maxConnections": 16, "usageMaxConnections": 8}
    c["redis"] = {"url": f"redis://kirobench-redis:6379/{1 if real else 0}", "keyPrefix": "kirobench"}
    c["observabilityRedis"] = {"keyPrefix": "kirobench:observability"}
    if real:
        c.pop("kiroUpstreamBaseUrl", None)
    else:
        c["kiroUpstreamBaseUrl"] = "http://kirobench-fake:19080/"
    c["host"], c["port"] = "0.0.0.0", 8990
    # 本地实例绝不刷新 Token：导入的线上账号没有 refresh token，后台刷新也关闭
    c["tokenRefreshBackgroundEnabled"] = False
    c["kiroVersionAutoSyncEnabled"] = False
    with open(target, "w") as fh:
        json.dump(c, fh, ensure_ascii=False)


if __name__ == "__main__":
    main()
