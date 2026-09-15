#!/bin/bash

# 流式保活测试脚本
# 用于验证 SSE keepalive 机制是否正常工作

set -e

# 配置
API_URL="${KIRO_API_URL:-http://localhost:3000}"
API_KEY="${KIRO_API_KEY:-test-key}"
TIMEOUT_SECONDS=180

# 颜色输出
GREEN='\033[0;32m'
RED='\033[0;31m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

echo -e "${GREEN}=== Kiro 流式保活测试 ===${NC}"
echo "API URL: $API_URL"
echo "超时设置: ${TIMEOUT_SECONDS}s"
echo ""

# 临时文件
RESPONSE_FILE=$(mktemp)
TIMING_FILE=$(mktemp)

cleanup() {
    rm -f "$RESPONSE_FILE" "$TIMING_FILE"
}
trap cleanup EXIT

echo -e "${YELLOW}[1/3] 测试短响应（应该正常完成）${NC}"
START_TIME=$(date +%s)
curl -s -N -X POST "$API_URL/v1/messages" \
  -H "Content-Type: application/json" \
  -H "x-api-key: $API_KEY" \
  -m 30 \
  -d '{
    "model": "claude-opus-5",
    "max_tokens": 100,
    "stream": true,
    "messages": [{
      "role": "user",
      "content": "说 hello"
    }]
  }' > "$RESPONSE_FILE" 2>&1 || true

END_TIME=$(date +%s)
ELAPSED=$((END_TIME - START_TIME))

if grep -q "message_stop" "$RESPONSE_FILE"; then
    echo -e "${GREEN}✓ 短响应测试通过 (${ELAPSED}s)${NC}"
else
    echo -e "${RED}✗ 短响应测试失败${NC}"
    cat "$RESPONSE_FILE"
    exit 1
fi

echo ""
echo -e "${YELLOW}[2/3] 测试长响应保活（模拟 Cloudflare 120s 场景）${NC}"
echo "请求一个需要长时间思考的任务..."

START_TIME=$(date +%s)
KEEPALIVE_COUNT=0
CONTENT_DELTA_COUNT=0
EMPTY_DELTA_COUNT=0

# 实时监控并统计
curl -s -N -X POST "$API_URL/v1/messages" \
  -H "Content-Type: application/json" \
  -H "x-api-key: $API_KEY" \
  -m $TIMEOUT_SECONDS \
  -d '{
    "model": "claude-opus-5",
    "max_tokens": 2048,
    "stream": true,
    "messages": [{
      "role": "user",
      "content": "请详细解释机器学习中的反向传播算法，包含数学推导过程。在回答前请仔细思考。"
    }]
  }' | while IFS= read -r line; do
    CURRENT_TIME=$(date +%s)
    ELAPSED=$((CURRENT_TIME - START_TIME))

    # 检查 ping 事件
    if echo "$line" | grep -q '"type":\s*"ping"'; then
        KEEPALIVE_COUNT=$((KEEPALIVE_COUNT + 1))
        echo -e "[${ELAPSED}s] ${YELLOW}Keepalive ping${NC}"
    fi

    # 检查空 delta
    if echo "$line" | grep -q 'content_block_delta'; then
        if echo "$line" | grep -qE '"(text|thinking|partial_json)":\s*""'; then
            EMPTY_DELTA_COUNT=$((EMPTY_DELTA_COUNT + 1))
            echo -e "[${ELAPSED}s] ${GREEN}空 Delta 保活${NC}"
        else
            CONTENT_DELTA_COUNT=$((CONTENT_DELTA_COUNT + 1))
        fi
    fi

    # 检查完成
    if echo "$line" | grep -q '"type":\s*"message_stop"'; then
        echo -e "[${ELAPSED}s] ${GREEN}✓ 消息完成${NC}"
        break
    fi

    # 超时检查
    if [ $ELAPSED -gt $((TIMEOUT_SECONDS - 5)) ]; then
        echo -e "${RED}⚠ 接近超时限制${NC}"
    fi
done > "$RESPONSE_FILE" 2>&1

END_TIME=$(date +%s)
ELAPSED=$((END_TIME - START_TIME))

echo ""
echo -e "${GREEN}=== 统计结果 ===${NC}"
echo "总耗时: ${ELAPSED}s"
echo "Ping 保活次数: $KEEPALIVE_COUNT"
echo "空 Delta 保活次数: $EMPTY_DELTA_COUNT"
echo "实际内容 Delta 次数: $CONTENT_DELTA_COUNT"

if [ $ELAPSED -gt 120 ]; then
    echo -e "${GREEN}✓ 成功突破 120 秒 Cloudflare 限制！${NC}"
fi

if grep -q "message_stop" "$RESPONSE_FILE"; then
    echo -e "${GREEN}✓ 长响应测试通过${NC}"
else
    echo -e "${RED}✗ 长响应测试失败（可能超时）${NC}"
    tail -20 "$RESPONSE_FILE"
    exit 1
fi

echo ""
echo -e "${YELLOW}[3/3] 检查保活间隔${NC}"

# 分析保活事件的时间间隔
if [ $EMPTY_DELTA_COUNT -gt 0 ] || [ $KEEPALIVE_COUNT -gt 0 ]; then
    TOTAL_KEEPALIVES=$((EMPTY_DELTA_COUNT + KEEPALIVE_COUNT))
    if [ $TOTAL_KEEPALIVES -gt 0 ]; then
        AVG_INTERVAL=$((ELAPSED / TOTAL_KEEPALIVES))
        echo "平均保活间隔: ${AVG_INTERVAL}s"

        if [ $AVG_INTERVAL -le 10 ]; then
            echo -e "${GREEN}✓ 保活间隔合理（≤10s）${NC}"
        elif [ $AVG_INTERVAL -le 30 ]; then
            echo -e "${YELLOW}⚠ 保活间隔偏长（10-30s），建议缩短${NC}"
        else
            echo -e "${RED}✗ 保活间隔过长（>30s），可能导致超时${NC}"
        fi
    fi
else
    echo -e "${RED}✗ 未检测到任何保活事件！${NC}"
    exit 1
fi

echo ""
echo -e "${GREEN}=== 所有测试通过 ===${NC}"
echo "您的流式保活机制工作正常，可以在 Cloudflare 上部署。"
