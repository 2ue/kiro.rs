#!/usr/bin/env python3
"""生成本地压测用的 940 个伪造账号（仿 59140 的形态：KIRO FREE、无自带模型目录、约 85 个禁用、
约 30% 账号并发覆盖为 2、5% 为 3，RPM 10），输出 seed-credentials.sql。Token 都是假的，只能配合假上游使用。"""
import hashlib
import json
import random

random.seed(20261117)
N = 940
with open("seed-credentials.sql", "w") as f:
    f.write("BEGIN;\n")
    for i in range(1, N + 1):
        disabled = i % 11 == 0
        rt = f"bench-rt-{i:04d}"
        data = {"id": i, "authMethod": "social", "provider": "Github", "accessToken": f"bench-at-{i:04d}",
                "refreshToken": rt, "expiresAt": "2027-06-01T00:00:00Z",
                "profileArn": "arn:aws:codewhisperer:us-east-1:000000000000:profile/BENCHFAKE",
                "apiRegion": "us-east-1", "machineId": hashlib.sha256(f"m{i}".encode()).hexdigest(),
                "machineIdSource": "stored_credential", "fingerprintVersion": "account-v2",
                "email": f"bench{i:04d}@example.invalid", "disabled": disabled, "subscriptionTitle": "KIRO FREE"}
        r = random.random()
        if r < 0.30:
            data.update({"maxConcurrentRequests": 2, "rpm": 10})
        elif r < 0.35:
            data.update({"maxConcurrentRequests": 3, "rpm": 10})
        payload = json.dumps(data).replace("'", "''")
        f.write(f"INSERT INTO credentials (id,priority,disabled,auth_kind,refresh_token_hash,data) VALUES "
                f"({i},0,{'true' if disabled else 'false'},'oauth','{hashlib.sha256(rt.encode()).hexdigest()}',"
                f"'{payload}'::jsonb);\n")
    f.write(f"SELECT setval('credentials_id_seq',{N});\nCOMMIT;\n")
