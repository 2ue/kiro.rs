"""与 src/anthropic/signature.rs 的 synthetic_thinking_signature 相同的合成签名。"""
import base64
import hashlib

_LEGACY = b"kiro-rs-compat-thinking-signature-v1\0"
_MARKER = hashlib.sha512(b"synthetic thinking signature marker v2").digest()[:len(_LEGACY)]


def synthetic_thinking_signature(thinking: str) -> str:
    return base64.b64encode(_MARKER + hashlib.sha256(thinking.encode()).digest()).decode()
