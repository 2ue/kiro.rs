//! AWS Event Stream 流式解码器
//!
//! 使用状态机处理流式数据，支持断点续传和容错处理
//!
//! ## 状态机设计
//!
//! 参考 kiro-kt 项目的状态机设计，采用四态模型：
//!
//! ```text
//! ┌─────────────────┐
//! │      Ready      │  (初始态，就绪接收数据)
//! └────────┬────────┘
//!          │ feed() 提供数据
//!          ↓
//! ┌─────────────────┐
//! │     Parsing     │  decode() 尝试解析
//! └────────┬────────┘
//!          │
//!     ┌────┴────────────┐
//!     ↓                 ↓
//!  [成功]            [失败]
//!     │                 │
//!     ↓                 ├─> error_count++
//! ┌─────────┐           │
//! │  Ready  │           ├─> error_count < max_errors?
//! └─────────┘           │    YES → Recovering → Ready
//!                       │    NO  ↓
//!                  ┌────────────┐
//!                  │   Stopped  │ (终止态)
//!                  └────────────┘
//! ```

use super::error::{ParseError, ParseResult};
use super::frame::{Frame, PRELUDE_SIZE, parse_frame};
use bytes::{Buf, BytesMut};

/// 默认最大缓冲区大小 (16 MB)
pub const DEFAULT_MAX_BUFFER_SIZE: usize = 16 * 1024 * 1024;

/// 默认最大连续错误数
pub const DEFAULT_MAX_ERRORS: usize = 5;

/// 默认初始缓冲区容量
pub const DEFAULT_BUFFER_CAPACITY: usize = 8192;

/// 解码器状态
///
/// 采用四态模型，参考 kiro-kt 的设计：
/// - Ready: 就绪状态，可以接收数据
/// - Parsing: 正在解析帧
/// - Recovering: 恢复中（尝试跳过损坏数据）
/// - Stopped: 已停止（错误过多，终止态）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecoderState {
    /// 就绪，可以接收数据
    Ready,
    /// 正在解析帧
    Parsing,
    /// 恢复中（跳过损坏数据）
    Recovering,
    /// 已停止（错误过多）
    Stopped,
}

/// 流式事件解码器
///
/// 用于从字节流中解析 AWS Event Stream 消息帧
///
/// # Example
///
/// ```rust,ignore
/// use kiro_rs::kiro::parser::EventStreamDecoder;
///
/// let mut decoder = EventStreamDecoder::new();
///
/// // 提供流数据
/// decoder.feed(chunk)?;
///
/// // 解码所有可用帧
/// for result in decoder.decode_iter() {
///     match result {
///         Ok(frame) => println!("Got frame: {:?}", frame.event_type()),
///         Err(e) => eprintln!("Parse error: {}", e),
///     }
/// }
/// ```
pub struct EventStreamDecoder {
    /// 内部缓冲区
    buffer: BytesMut,
    /// 当前状态
    state: DecoderState,
    /// 已处理的帧数量
    frames_decoded: usize,
    /// 连续错误计数
    error_count: usize,
    /// 最大连续错误数
    max_errors: usize,
    /// 最大缓冲区大小
    max_buffer_size: usize,
    /// 跳过的字节数（用于调试）
    bytes_skipped: usize,
}

impl Default for EventStreamDecoder {
    fn default() -> Self {
        Self::new()
    }
}

impl EventStreamDecoder {
    /// 创建新的解码器
    pub fn new() -> Self {
        Self::with_capacity(DEFAULT_BUFFER_CAPACITY)
    }

    /// 创建具有指定缓冲区大小的解码器
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            buffer: BytesMut::with_capacity(capacity),
            state: DecoderState::Ready,
            frames_decoded: 0,
            error_count: 0,
            max_errors: DEFAULT_MAX_ERRORS,
            max_buffer_size: DEFAULT_MAX_BUFFER_SIZE,
            bytes_skipped: 0,
        }
    }

    /// 向解码器提供数据
    ///
    /// # Returns
    /// - `Ok(())` - 数据已添加到缓冲区
    /// - `Err(BufferOverflow)` - 缓冲区已满
    pub fn feed(&mut self, data: &[u8]) -> ParseResult<()> {
        // 检查缓冲区大小限制
        let new_size = self.buffer.len() + data.len();
        if new_size > self.max_buffer_size {
            return Err(ParseError::BufferOverflow {
                size: new_size,
                max: self.max_buffer_size,
            });
        }

        self.buffer.extend_from_slice(data);

        // 从 Recovering 状态恢复到 Ready
        if self.state == DecoderState::Recovering {
            self.state = DecoderState::Ready;
        }

        Ok(())
    }

    /// 尝试解码下一个帧
    ///
    /// # Returns
    /// - `Ok(Some(frame))` - 成功解码一个帧
    /// - `Ok(None)` - 数据不足，需要更多数据
    /// - `Err(e)` - 解码错误
    pub fn decode(&mut self) -> ParseResult<Option<Frame>> {
        // 如果已停止，直接返回错误
        if self.state == DecoderState::Stopped {
            return Err(ParseError::TooManyErrors {
                count: self.error_count,
                last_error: "解码器已停止".to_string(),
            });
        }

        // 缓冲区为空，保持 Ready 状态
        if self.buffer.is_empty() {
            self.state = DecoderState::Ready;
            return Ok(None);
        }

        // 转移到 Parsing 状态
        self.state = DecoderState::Parsing;

        match parse_frame(&self.buffer) {
            Ok(Some((frame, consumed))) => {
                // 成功解析
                self.buffer.advance(consumed);
                self.state = DecoderState::Ready;
                self.frames_decoded += 1;
                self.error_count = 0; // 重置连续错误计数
                Ok(Some(frame))
            }
            Ok(None) => {
                // 数据不足，回到 Ready 状态等待更多数据
                self.state = DecoderState::Ready;
                Ok(None)
            }
            Err(e) => {
                self.error_count += 1;
                let error_msg = e.to_string();

                // 检查是否超过最大错误数
                if self.error_count >= self.max_errors {
                    self.state = DecoderState::Stopped;
                    tracing::error!(
                        "解码器停止: 连续 {} 次错误，最后错误: {}",
                        self.error_count,
                        error_msg
                    );
                    return Err(ParseError::TooManyErrors {
                        count: self.error_count,
                        last_error: error_msg,
                    });
                }

                // 根据错误类型采用不同的恢复策略
                self.try_recover(&e);
                self.state = DecoderState::Recovering;
                Err(e)
            }
        }
    }

    /// 创建解码迭代器
    pub fn decode_iter(&mut self) -> DecodeIter<'_> {
        DecodeIter { decoder: self }
    }

    /// Returns the number of bytes that have not formed a complete frame yet.
    ///
    /// Callers must check this when the transport reaches EOF. A non-zero value
    /// means the response ended in the middle of an EventStream frame and must
    /// not be treated as a successful completion.
    pub fn pending_bytes(&self) -> usize {
        self.buffer.len()
    }

    /// Returns whether EOF can be accepted without discarding a partial frame.
    pub fn is_clean_eof(&self) -> bool {
        self.buffer.is_empty()
    }

    /// Returns the number of complete frames decoded from this response.
    pub fn frames_decoded(&self) -> usize {
        self.frames_decoded
    }

    /// 尝试容错恢复
    ///
    /// 根据错误类型采用不同的恢复策略（参考 kiro-kt 的设计）：
    /// - Prelude 阶段错误（CRC 失败、长度异常）：跳过 1 字节，尝试找下一帧边界
    /// - Data 阶段错误（Message CRC 失败、Header 解析失败）：跳过整个损坏帧
    fn try_recover(&mut self, error: &ParseError) {
        if self.buffer.is_empty() {
            return;
        }

        match error {
            // Prelude 阶段错误：可能是帧边界错位，逐字节扫描找下一个有效边界
            ParseError::PreludeCrcMismatch { .. }
            | ParseError::MessageTooSmall { .. }
            | ParseError::MessageTooLarge { .. } => {
                let skipped_byte = self.buffer[0];
                self.buffer.advance(1);
                self.bytes_skipped += 1;
                tracing::warn!(
                    "Prelude 错误恢复: 跳过字节 0x{:02x} (累计跳过 {} 字节)",
                    skipped_byte,
                    self.bytes_skipped
                );
            }

            // Data 阶段错误：帧边界正确但数据损坏，跳过整个帧
            ParseError::MessageCrcMismatch { .. } | ParseError::HeaderParseFailed(_) => {
                // 尝试读取 total_length 来跳过整帧
                if self.buffer.len() >= PRELUDE_SIZE {
                    let total_length = u32::from_be_bytes([
                        self.buffer[0],
                        self.buffer[1],
                        self.buffer[2],
                        self.buffer[3],
                    ]) as usize;

                    // 确保 total_length 合理且缓冲区有足够数据
                    if total_length >= 16 && total_length <= self.buffer.len() {
                        tracing::warn!("Data 错误恢复: 跳过损坏帧 ({} 字节)", total_length);
                        self.buffer.advance(total_length);
                        self.bytes_skipped += total_length;
                        return;
                    }
                }

                // 无法确定帧长度，回退到逐字节跳过
                let skipped_byte = self.buffer[0];
                self.buffer.advance(1);
                self.bytes_skipped += 1;
                tracing::warn!(
                    "Data 错误恢复 (回退): 跳过字节 0x{:02x} (累计跳过 {} 字节)",
                    skipped_byte,
                    self.bytes_skipped
                );
            }

            // 其他错误：逐字节跳过
            _ => {
                let skipped_byte = self.buffer[0];
                self.buffer.advance(1);
                self.bytes_skipped += 1;
                tracing::warn!(
                    "通用错误恢复: 跳过字节 0x{:02x} (累计跳过 {} 字节)",
                    skipped_byte,
                    self.bytes_skipped
                );
            }
        }
    }
}

/// 解码迭代器
pub struct DecodeIter<'a> {
    decoder: &'a mut EventStreamDecoder,
}

impl<'a> Iterator for DecodeIter<'a> {
    type Item = ParseResult<Frame>;

    fn next(&mut self) -> Option<Self::Item> {
        // 如果处于 Stopped 或 Recovering 状态，停止迭代
        match self.decoder.state {
            DecoderState::Stopped => return None,
            DecoderState::Recovering => return None,
            _ => {}
        }

        match self.decoder.decode() {
            Ok(Some(frame)) => Some(Ok(frame)),
            Ok(None) => None,
            Err(e) => Some(Err(e)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kiro::parser::crc::crc32;

    fn string_header(name: &str, value: &str, output: &mut Vec<u8>) {
        output.push(name.len() as u8);
        output.extend_from_slice(name.as_bytes());
        output.push(7); // HeaderValueType::String
        output.extend_from_slice(&(value.len() as u16).to_be_bytes());
        output.extend_from_slice(value.as_bytes());
    }

    fn event_frame(event_type: &str, payload: &[u8]) -> Vec<u8> {
        let mut headers = Vec::new();
        string_header(":message-type", "event", &mut headers);
        string_header(":event-type", event_type, &mut headers);
        string_header(":content-type", "application/json", &mut headers);
        let total_length = PRELUDE_SIZE + headers.len() + payload.len() + 4;
        let mut frame = vec![0u8; total_length];
        frame[0..4].copy_from_slice(&(total_length as u32).to_be_bytes());
        frame[4..8].copy_from_slice(&(headers.len() as u32).to_be_bytes());
        let prelude_crc = crc32(&frame[..8]);
        frame[8..12].copy_from_slice(&prelude_crc.to_be_bytes());
        frame[PRELUDE_SIZE..PRELUDE_SIZE + headers.len()].copy_from_slice(&headers);
        let payload_start = PRELUDE_SIZE + headers.len();
        frame[payload_start..payload_start + payload.len()].copy_from_slice(payload);
        let message_crc = crc32(&frame[..total_length - 4]);
        frame[total_length - 4..].copy_from_slice(&message_crc.to_be_bytes());
        frame
    }

    #[test]
    fn test_decoder_feed() {
        let mut decoder = EventStreamDecoder::new();
        assert!(decoder.feed(&[1, 2, 3, 4]).is_ok());
    }

    #[test]
    fn test_decoder_insufficient_data() {
        let mut decoder = EventStreamDecoder::new();
        decoder.feed(&[0u8; 10]).unwrap();

        let result = decoder.decode();
        assert!(matches!(result, Ok(None)));
        assert_eq!(decoder.pending_bytes(), 10);
        assert!(!decoder.is_clean_eof());
    }

    #[test]
    fn decoder_accepts_fragmented_valid_frame() {
        let frame = event_frame("assistantResponseEvent", br#"{"content":"ok"}"#);
        let mut decoder = EventStreamDecoder::new();

        let split_at = frame.len() - 1;
        for chunk in frame[..split_at].chunks(3) {
            decoder.feed(chunk).expect("feed fragment");
            assert!(decoder.decode().expect("decode fragment").is_none());
        }

        decoder.feed(&frame[split_at..]).expect("feed final byte");
        let decoded = decoder.decode().expect("decode completed frame");
        assert!(decoded.is_some());
        assert!(decoder.is_clean_eof());
        assert_eq!(decoder.frames_decoded(), 1);
    }

    #[test]
    fn decoder_exposes_truncated_frame_as_dirty_eof() {
        let frame = event_frame("assistantResponseEvent", br#"{"content":"ok"}"#);
        let mut decoder = EventStreamDecoder::new();
        decoder.feed(&frame[..frame.len() - 1]).expect("feed");

        assert!(decoder.decode().expect("decode").is_none());
        assert_eq!(decoder.pending_bytes(), frame.len() - 1);
        assert!(!decoder.is_clean_eof());
    }

    #[test]
    fn decoder_recovers_from_bad_frame_and_decodes_following_frame() {
        let mut bad = event_frame("assistantResponseEvent", br#"{"content":"bad"}"#);
        let last = bad.len() - 1;
        bad[last] ^= 0xff;
        let good = event_frame("assistantResponseEvent", br#"{"content":"good"}"#);
        let mut decoder = EventStreamDecoder::new();
        decoder.feed(&[bad, good].concat()).expect("feed frames");

        assert!(matches!(
            decoder.decode(),
            Err(ParseError::MessageCrcMismatch { .. })
        ));
        decoder.feed(&[]).expect("resume after recovery");
        let recovered = decoder
            .decode()
            .expect("decode recovered frame")
            .expect("following valid frame");
        assert_eq!(recovered.event_type(), Some("assistantResponseEvent"));
        assert_eq!(recovered.payload_as_str(), r#"{"content":"good"}"#);
        assert_eq!(decoder.frames_decoded(), 1);
        assert!(decoder.is_clean_eof());
    }

    #[test]
    fn decoder_preserves_duplicate_frames_for_semantic_layer() {
        let frame = event_frame("assistantResponseEvent", br#"{"content":"same"}"#);
        let mut decoder = EventStreamDecoder::new();
        decoder
            .feed(&[frame.clone(), frame].concat())
            .expect("feed duplicate frames");

        let first = decoder
            .decode()
            .expect("first decode")
            .expect("first frame");
        let second = decoder
            .decode()
            .expect("second decode")
            .expect("second frame");
        assert_eq!(first.payload, second.payload);
        assert_eq!(decoder.frames_decoded(), 2);
        assert!(decoder.is_clean_eof());
    }
}
