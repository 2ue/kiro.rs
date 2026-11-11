//! 发往上游前压缩过大的图片（运行配置 `imageProcessing.resizeEnabled`，默认关闭）。
//!
//! 只处理 base64 编码的 PNG、JPEG、WebP：长边超过上限或 base64 长度超过上限时，先等比缩放，再重新
//! 编码为 JPEG（透明区域铺白底）；仍然超限时逐步降低画质，最后再缩小尺寸。GIF 不处理，以免破坏
//! 动画。解码前限制尺寸和内存，失败或结果不比原图小时保留原图。

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use image::{DynamicImage, GenericImageView, ImageFormat, ImageReader, Limits};
use serde_json::Value;

use crate::model::config::ImageProcessingConfig;

/// 解码时允许的最大边长和内存，防止解压炸弹。
const DECODE_MAX_SIDE: u32 = 16_384;
const DECODE_MAX_ALLOC_BYTES: u64 = 512 * 1024 * 1024;
/// 降低画质时的下限，以及每轮缩小尺寸的比例和最小长边。
const MIN_JPEG_QUALITY: u8 = 35;
const SHRINK_STEP: f32 = 0.75;
const MIN_LONG_SIDE: u32 = 256;

#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub(crate) struct ImageResizeReport {
    pub(crate) resized: usize,
    pub(crate) original_bytes: usize,
    pub(crate) final_bytes: usize,
    pub(crate) failed: usize,
    /// 最近一张截图（最后一条带工具结果图片的消息里、最后一张工具结果图片）被缩放时的比例。
    pub(crate) latest_screenshot_scale: Option<ScreenshotScale>,
}

impl ImageResizeReport {
    fn merge_totals(&mut self, other: Self) {
        self.resized += other.resized;
        self.original_bytes += other.original_bytes;
        self.final_bytes += other.final_bytes;
        self.failed += other.failed;
    }
}

/// 截图原图与缩放后尺寸之比。模型看到的是缩放后的图，computer use 坐标在缩放后的像素空间里；
/// 乘以这个比例就回到客户端原始截图的像素空间。
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct ScreenshotScale {
    pub(crate) x: f64,
    pub(crate) y: f64,
}

impl ScreenshotScale {
    pub(crate) fn from_dimensions(original: (u32, u32), resized: (u32, u32)) -> Option<Self> {
        if resized.0 == 0 || resized.1 == 0 || resized == original {
            return None;
        }
        Some(Self {
            x: f64::from(original.0) / f64::from(resized.0),
            y: f64::from(original.1) / f64::from(resized.1),
        })
    }

    /// 把 computer use 工具参数里的 `coordinate` / `start_coordinate`（`[x, y]`）和 `region`
    /// （`[x0, y0, x1, y1]`）换算回原图像素并四舍五入为整数；形状不符的值保持不变。
    /// 返回是否有改动。
    pub(crate) fn rescale_computer_input(&self, input: &mut Value) -> bool {
        let Some(object) = input.as_object_mut() else {
            return false;
        };
        let mut changed = false;
        for (key, len) in [("coordinate", 2), ("start_coordinate", 2), ("region", 4)] {
            let Some(Value::Array(values)) = object.get_mut(key) else {
                continue;
            };
            if values.len() != len || !values.iter().all(Value::is_number) {
                continue;
            }
            for (index, value) in values.iter_mut().enumerate() {
                let factor = if index % 2 == 0 { self.x } else { self.y };
                let scaled = (value.as_f64().unwrap_or_default() * factor).round();
                if scaled.is_finite() && scaled.abs() < 1e15 {
                    *value = Value::from(scaled as i64);
                    changed = true;
                }
            }
        }
        changed
    }
}

/// 压缩消息里所有符合条件的 base64 图片（包括工具结果里的图片）。开关关闭时什么都不做。
pub(crate) fn resize_images_in_messages(
    messages: &mut [crate::anthropic::types::Message],
    config: ImageProcessingConfig,
) -> ImageResizeReport {
    let config = config.normalized();
    let mut report = ImageResizeReport::default();
    if !config.resize_enabled {
        return report;
    }
    for message in messages {
        let content = resize_images_in_content(&mut message.content, config, false);
        // 后面的截图取代前面的：只有最近一张决定坐标换算。
        if let Some(latest) = content.latest_screenshot_scale {
            report.latest_screenshot_scale = latest;
        }
        report.merge_totals(content.totals);
    }
    if report.resized > 0 || report.failed > 0 {
        tracing::info!(
        resized = report.resized,
               original_bytes = report.original_bytes,
        final_bytes = report.final_bytes,
        failed = report.failed,
        latest_screenshot_scale = ?report.latest_screenshot_scale,
        "发往上游前已压缩过大的图片"
           );
    }
    report
}

/// 一个 content 数组的汇总。`latest_screenshot_scale` 外层 `None` 表示其中没有工具结果图片，
/// `Some(None)` 表示最后一张工具结果图片没有被缩放。
#[derive(Default)]
struct ContentResizeReport {
    totals: ImageResizeReport,
    latest_screenshot_scale: Option<Option<ScreenshotScale>>,
}

fn resize_images_in_content(
    content: &mut Value,
    config: ImageProcessingConfig,
    inside_tool_result: bool,
) -> ContentResizeReport {
    let mut report = ContentResizeReport::default();
    let Value::Array(items) = content else {
        return report;
    };
    for item in items {
        let Some(object) = item.as_object_mut() else {
            continue;
        };
        match object.get("type").and_then(Value::as_str) {
            Some("tool_result") => {
                if let Some(nested) = object.get_mut("content") {
                    let nested = resize_images_in_content(nested, config, true);
                    if nested.latest_screenshot_scale.is_some() {
                        report.latest_screenshot_scale = nested.latest_screenshot_scale;
                    }
                    report.totals.merge_totals(nested.totals);
                }
            }
            Some("image") => {
                if inside_tool_result {
                    report.latest_screenshot_scale = Some(None);
                }
                let Some(source) = object.get_mut("source").and_then(Value::as_object_mut) else {
                    continue;
                };
                if source.get("type").and_then(Value::as_str) != Some("base64") {
                    continue;
                }
                let Some(data) = source.get("data").and_then(Value::as_str) else {
                    continue;
                };
                match resize_base64_image(data, config) {
                    ResizeOutcome::Unchanged => {}
                    ResizeOutcome::Failed => report.totals.failed += 1,
                    ResizeOutcome::Resized {
                        data,
                        original_bytes,
                        original_dimensions,
                        resized_dimensions,
                    } => {
                        if inside_tool_result {
                            report.latest_screenshot_scale =
                                Some(ScreenshotScale::from_dimensions(
                                    original_dimensions,
                                    resized_dimensions,
                                ));
                        }
                        report.totals.resized += 1;
                        report.totals.original_bytes += original_bytes;
                        report.totals.final_bytes += data.len();
                        source.insert("data".to_string(), Value::String(data));
                        source.insert(
                            "media_type".to_string(),
                            Value::String("image/jpeg".to_string()),
                        );
                    }
                }
            }
            _ => {}
        }
    }
    report
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ResizeOutcome {
    Unchanged,
    Resized {
        data: String,
        original_bytes: usize,
        original_dimensions: (u32, u32),
        resized_dimensions: (u32, u32),
    },
    Failed,
}

pub(crate) fn resize_base64_image(data: &str, config: ImageProcessingConfig) -> ResizeOutcome {
    let encoded: String = data
        .chars()
        .filter(|ch| !ch.is_ascii_whitespace())
        .collect();
    let Ok(bytes) = BASE64.decode(encoded.as_bytes()) else {
        return ResizeOutcome::Unchanged;
    };
    let format = match image::guess_format(&bytes) {
        Ok(format @ (ImageFormat::Png | ImageFormat::Jpeg | ImageFormat::WebP)) => format,
        _ => return ResizeOutcome::Unchanged,
    };
    let Some((width, height)) = probe_dimensions(&bytes, format) else {
        return ResizeOutcome::Unchanged;
    };
    let too_large = encoded.len() > config.resize_max_bytes;
    let too_long = width.max(height) > config.resize_max_long_side;
    if !too_large && !too_long {
        return ResizeOutcome::Unchanged;
    }
    match shrink(&bytes, format, config) {
        Ok((jpeg, resized_dimensions)) => {
            let output = BASE64.encode(jpeg);
            if output.len() >= encoded.len() && !too_long {
                return ResizeOutcome::Unchanged;
            }
            ResizeOutcome::Resized {
                data: output,
                original_bytes: encoded.len(),
                original_dimensions: (width, height),
                resized_dimensions,
            }
        }
        Err(error) => {
            tracing::warn!(error = %error, original_bytes = encoded.len(), "图片压缩失败，保留原图");
            ResizeOutcome::Failed
        }
    }
}

fn reader(bytes: &[u8], format: ImageFormat) -> ImageReader<std::io::Cursor<&[u8]>> {
    let mut reader = ImageReader::with_format(std::io::Cursor::new(bytes), format);
    let mut limits = Limits::default();
    limits.max_image_width = Some(DECODE_MAX_SIDE);
    limits.max_image_height = Some(DECODE_MAX_SIDE);
    limits.max_alloc = Some(DECODE_MAX_ALLOC_BYTES);
    reader.limits(limits);
    reader
}

fn probe_dimensions(bytes: &[u8], format: ImageFormat) -> Option<(u32, u32)> {
    reader(bytes, format).into_dimensions().ok()
}

/// 返回 JPEG 字节和它的尺寸。
fn shrink(
    bytes: &[u8],
    format: ImageFormat,
    config: ImageProcessingConfig,
) -> anyhow::Result<(Vec<u8>, (u32, u32))> {
    let image = reader(bytes, format).decode()?;
    let mut long_side = image
        .width()
        .max(image.height())
        .min(config.resize_max_long_side);
    loop {
        let scaled = scale_to_long_side(&image, long_side);
        let flattened = flatten_to_rgb(&scaled);
        let mut quality = config.resize_jpeg_quality;
        loop {
            let jpeg = encode_jpeg(&flattened, quality)?;
            let encoded_len = jpeg.len().div_ceil(3) * 4;
            if encoded_len <= config.resize_max_bytes {
                return Ok((jpeg, flattened.dimensions()));
            }
            if quality <= MIN_JPEG_QUALITY {
                break;
            }
            quality = quality.saturating_sub(10).max(MIN_JPEG_QUALITY);
        }
        let next = ((long_side as f32) * SHRINK_STEP) as u32;
        if next < MIN_LONG_SIDE {
            // 已经缩到最小仍超过体积目标：返回最小尺寸的结果，尺寸上限至少已经满足。
            let smallest = flatten_to_rgb(&scale_to_long_side(&image, MIN_LONG_SIDE));
            return Ok((
                encode_jpeg(&smallest, MIN_JPEG_QUALITY)?,
                smallest.dimensions(),
            ));
        }
        long_side = next;
    }
}

fn scale_to_long_side(image: &DynamicImage, long_side: u32) -> DynamicImage {
    let (width, height) = image.dimensions();
    if width.max(height) <= long_side {
        return image.clone();
    }
    image.resize(long_side, long_side, image::imageops::FilterType::Triangle)
}

/// JPEG 不支持透明：透明像素按透明度与白底混合。
fn flatten_to_rgb(image: &DynamicImage) -> image::RgbImage {
    let rgba = image.to_rgba8();
    let mut rgb = image::RgbImage::new(rgba.width(), rgba.height());
    for (x, y, pixel) in rgba.enumerate_pixels() {
        let [r, g, b, a] = pixel.0;
        let alpha = a as u16;
        let blend = |channel: u8| ((channel as u16 * alpha + 255 * (255 - alpha)) / 255) as u8;
        rgb.put_pixel(x, y, image::Rgb([blend(r), blend(g), blend(b)]));
    }
    rgb
}

fn encode_jpeg(image: &image::RgbImage, quality: u8) -> anyhow::Result<Vec<u8>> {
    let mut output = Vec::new();
    let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut output, quality);
    image.write_with_encoder(encoder)?;
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn enabled() -> ImageProcessingConfig {
        ImageProcessingConfig {
            resize_enabled: true,
            ..ImageProcessingConfig::default()
        }
    }

    fn png(width: u32, height: u32, alpha: bool) -> String {
        let mut buffer = Vec::new();
        if alpha {
            let image = image::RgbaImage::from_fn(width, height, |x, y| {
                image::Rgba([
                    (x % 256) as u8,
                    (y % 256) as u8,
                    ((x + y) % 256) as u8,
                    (x % 2 * 200) as u8,
                ])
            });
            image
                .write_to(&mut std::io::Cursor::new(&mut buffer), ImageFormat::Png)
                .unwrap();
        } else {
            let image = image::RgbImage::from_fn(width, height, |x, y| {
                image::Rgb([
                    ((x * 7) % 256) as u8,
                    ((y * 13) % 256) as u8,
                    ((x * y) % 256) as u8,
                ])
            });
            image
                .write_to(&mut std::io::Cursor::new(&mut buffer), ImageFormat::Png)
                .unwrap();
        }
        BASE64.encode(buffer)
    }

    fn decoded_dimensions(data: &str) -> (u32, u32) {
        let bytes = BASE64.decode(data).unwrap();
        image::load_from_memory(&bytes).unwrap().dimensions()
    }

    #[test]
    fn large_image_is_scaled_and_reencoded_within_limits() {
        let original = png(2400, 1600, false);
        let ResizeOutcome::Resized {
            data,
            original_bytes,
            original_dimensions,
            resized_dimensions,
        } = resize_base64_image(&original, enabled())
        else {
            panic!("a 2400x1600 image must be resized");
        };
        assert_eq!(original_bytes, original.len());
        assert!(data.len() <= enabled().resize_max_bytes, "{}", data.len());
        let (width, height) = decoded_dimensions(&data);
        assert!(width.max(height) <= 1568, "{width}x{height}");
        assert_eq!(width * 1600 / 2400, height, "aspect ratio is kept");
        assert_eq!(original_dimensions, (2400, 1600));
        assert_eq!(resized_dimensions, (width, height));
        assert_eq!(
            image::guess_format(&BASE64.decode(&data).unwrap()).unwrap(),
            ImageFormat::Jpeg
        );
    }

    #[test]
    fn small_images_gifs_and_garbage_are_left_alone() {
        assert_eq!(
            resize_base64_image(&png(64, 64, false), enabled()),
            ResizeOutcome::Unchanged
        );
        let gif = BASE64.encode(b"GIF89a\x01\x00\x01\x00\x00\x00\x00;");
        assert_eq!(
            resize_base64_image(&gif, enabled()),
            ResizeOutcome::Unchanged
        );
        assert_eq!(
            resize_base64_image("not base64!!", enabled()),
            ResizeOutcome::Unchanged
        );
        let fake_png = BASE64.encode(b"\x89PNG\r\n\x1a\nbroken");
        assert_eq!(
            resize_base64_image(&fake_png, enabled()),
            ResizeOutcome::Unchanged
        );
    }

    #[test]
    fn transparent_png_is_flattened_onto_white() {
        let ResizeOutcome::Resized { data, .. } =
            resize_base64_image(&png(2000, 300, true), enabled())
        else {
            panic!("a 2000px wide image must be resized");
        };
        assert!(decoded_dimensions(&data).0 <= 1568);
    }

    /// The switch is off by default; when off, messages are never touched. Images inside tool
    /// results are processed too, and the media type follows the new encoding.
    #[test]
    fn messages_are_only_rewritten_when_the_switch_is_on() {
        let original = png(2400, 1600, false);
        let messages = || {
            vec![crate::anthropic::types::Message {
                role: "user".to_string(),
                content: serde_json::json!([
                   {"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": original}},
                   {"type": "tool_result", "tool_use_id": "t1", "content": [
                {"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": original}}
                   ]},
                   {"type": "text", "text": "look"}
                      ]),
            }]
        };
        let mut untouched = messages();
        let report = resize_images_in_messages(&mut untouched, ImageProcessingConfig::default());
        assert_eq!(report, ImageResizeReport::default());
        assert_eq!(untouched[0].content, messages()[0].content);

        let mut resized = messages();
        let report = resize_images_in_messages(&mut resized, enabled());
        assert_eq!(report.resized, 2);
        assert!(report.final_bytes < report.original_bytes);
        let content = &resized[0].content;
        assert_eq!(content[0]["source"]["media_type"], "image/jpeg");
        assert_eq!(
            content[1]["content"][0]["source"]["media_type"],
            "image/jpeg"
        );
        assert_eq!(content[2]["text"], "look");
    }

    fn screenshot_message(image: &str) -> crate::anthropic::types::Message {
        crate::anthropic::types::Message {
            role: "user".to_string(),
            content: serde_json::json!([
                {"type": "tool_result", "tool_use_id": "toolu_shot", "toolset_name": "computer", "content": [
                        {"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": image}}
                    ]}
            ]),
        }
    }

    /// Only the latest tool-result screenshot decides the coordinate scale: a later screenshot
    /// that needed no resize clears the scale of an earlier resized one.
    #[test]
    fn screenshot_scale_tracks_the_latest_tool_result_image() {
        let large = png(2400, 1600, false);
        let mut messages = vec![screenshot_message(&large)];
        let report = resize_images_in_messages(&mut messages, enabled());
        let scale = report
            .latest_screenshot_scale
            .expect("resized screenshot has a scale");
        let resized = messages[0].content[0]["content"][0]["source"]["data"]
            .as_str()
            .unwrap();
        let (width, height) = decoded_dimensions(resized);
        assert!(
            (scale.x - 2400.0 / f64::from(width)).abs() < 1e-9,
            "{scale:?}"
        );
        assert!(
            (scale.y - 1600.0 / f64::from(height)).abs() < 1e-9,
            "{scale:?}"
        );

        let mut messages = vec![
            screenshot_message(&large),
            crate::anthropic::types::Message {
                role: "assistant".to_string(),
                content: serde_json::json!([{"type": "text", "text": "ok"}]),
            },
            screenshot_message(&png(320, 240, false)),
        ];
        let report = resize_images_in_messages(&mut messages, enabled());
        assert_eq!(report.resized, 1);
        assert_eq!(report.latest_screenshot_scale, None);

        // Re-encoded only to meet the byte limit: same dimensions, so coordinates need no scale.
        let mut messages = vec![screenshot_message(&png(800, 600, false))];
        let report = resize_images_in_messages(&mut messages, enabled());
        assert_eq!(
            report.resized, 1,
            "the 800x600 fixture exceeds the byte limit"
        );
        let resized = messages[0].content[0]["content"][0]["source"]["data"]
            .as_str()
            .unwrap();
        assert_eq!(decoded_dimensions(resized), (800, 600));
        assert_eq!(report.latest_screenshot_scale, None);

        // Images outside tool results (for example a user attachment) never set the scale.
        let mut messages = vec![crate::anthropic::types::Message {
            role: "user".to_string(),
            content: serde_json::json!([
                {"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": large}}
            ]),
        }];
        let report = resize_images_in_messages(&mut messages, enabled());
        assert_eq!(report.resized, 1);
        assert_eq!(report.latest_screenshot_scale, None);
    }

    #[test]
    fn screenshot_scale_rescales_computer_coordinates_to_whole_original_pixels() {
        let scale = ScreenshotScale::from_dimensions((2400, 1600), (1200, 800)).unwrap();
        let mut input = serde_json::json!({
            "coordinate": [100, 51],
            "start_coordinate": [10.4, 20],
            "region": [0, 0, 600, 401],
            "text": "ctrl"
        });
        assert!(scale.rescale_computer_input(&mut input));
        assert_eq!(
            input,
            serde_json::json!({
                "coordinate": [200, 102],
                "start_coordinate": [21, 40],
                "region": [0, 0, 1200, 802],
                "text": "ctrl"
            })
        );

        // Malformed shapes and inputs without coordinates are left alone.
        let mut input = serde_json::json!({"coordinate": [1, 2, 3], "region": ["a", 1, 2, 3]});
        assert!(!scale.rescale_computer_input(&mut input));
        assert_eq!(
            input,
            serde_json::json!({"coordinate": [1, 2, 3], "region": ["a", 1, 2, 3]})
        );
        assert!(!scale.rescale_computer_input(&mut serde_json::json!({})));

        assert_eq!(
            ScreenshotScale::from_dimensions((800, 600), (800, 600)),
            None
        );
    }
}
