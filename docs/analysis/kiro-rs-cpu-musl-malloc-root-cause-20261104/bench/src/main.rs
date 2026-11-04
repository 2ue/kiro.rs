// 模拟 kiro-rs 每次上游发送对请求体做的处理：DOM 解析 + 序列化 + 多次整串复制。
#[cfg(feature = "mimalloc")]
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

use std::time::Instant;

fn make_body(target: usize) -> String {
    // 类似 conversationState.history：大量中小字符串对象
    let mut s = String::from(r#"{"conversationState":{"conversationId":"11111111-2222-3333-4444-555555555555","history":["#);
    let mut i = 0;
    while s.len() < target {
        if i > 0 { s.push(','); }
        s.push_str(&format!(r#"{{"userInputMessage":{{"content":"message {i} {}","toolResults":[{{"toolUseId":"t{i}","content":[{{"text":"{}"}}]}}]}}}}"#, "lorem ipsum ".repeat(20), "x".repeat(400)));
        i += 1;
    }
    s.push_str("]}}");
    s
}

fn per_send(body: &str) -> usize {
    // refresh_kiro_request_identity: DOM 解析 + to_string + replacen
    let v: serde_json::Value = serde_json::from_str(body).unwrap();
    let copy = body.to_string();
    let copy = copy.replacen("11111111-2222", "99999999-2222", 1);
    // transform_ide_api_body: 再解析 + 插入 profileArn + 序列化
    let mut v2: serde_json::Value = serde_json::from_str(&copy).unwrap();
    v2.as_object_mut().unwrap().insert("profileArn".into(), "arn:aws:x".into());
    let out = serde_json::to_string(&v2).unwrap();
    drop(v);
    out.len()
}

fn main() {
    let threads: usize = std::env::args().nth(1).map(|s| s.parse().unwrap()).unwrap_or(8);
    let mb: usize = std::env::args().nth(2).map(|s| s.parse().unwrap()).unwrap_or(4);
    let iters: usize = std::env::args().nth(3).map(|s| s.parse().unwrap()).unwrap_or(20);
    let body = std::sync::Arc::new(make_body(mb << 20));
    let t = Instant::now();
    let hs: Vec<_> = (0..threads).map(|_| { let b = body.clone(); std::thread::spawn(move || { let mut n = 0; for _ in 0..iters { n += per_send(&b); } n }) }).collect();
    let total: usize = hs.into_iter().map(|h| h.join().unwrap()).sum();
    let el = t.elapsed().as_secs_f64();
    println!("threads={threads} body={mb}MB sends={} wall={el:.2}s sends/s={:.1} out={}MB", threads*iters, (threads*iters) as f64/el, total>>20);
}
