//! HTTP Client 构建模块
//!
//! 提供统一的 HTTP Client 构建功能，支持代理配置

use bytes::Bytes;
use reqwest::{Client, Proxy, RequestBuilder, Response};
use std::fmt;
use std::time::Duration;
use tokio::time::timeout;

use crate::model::config::TlsBackend;

/// 代理配置
#[derive(Clone, Default, PartialEq, Eq, Hash)]
pub struct ProxyConfig {
    /// 代理地址，支持 http/https/socks5/socks5h
    pub url: String,
    /// 代理认证用户名
    pub username: Option<String>,
    /// 代理认证密码
    pub password: Option<String>,
}

impl fmt::Debug for ProxyConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProxyConfig")
            .field("url_configured", &!self.url.trim().is_empty())
            .field("scheme", &self.scheme_for_log())
            .field("username_present", &self.username.is_some())
            .field("password_present", &self.password.is_some())
            .finish()
    }
}

pub fn maybe_compress_json_whitespace(body: String, enabled: bool) -> String {
    if !enabled {
        return body;
    }
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&body) else {
        return body;
    };
    serde_json::to_string(&value).unwrap_or(body)
}

#[derive(Debug)]
pub enum HttpSendError {
    Request(reqwest::Error),
    ResponseHeaderTimeout { timeout_secs: u64 },
    ResponseBodyTimeout { timeout_secs: u64 },
}

impl HttpSendError {
    #[cfg(test)]
    pub fn is_response_header_timeout(&self) -> bool {
        matches!(self, Self::ResponseHeaderTimeout { .. })
    }

    #[cfg(test)]
    pub fn is_response_body_timeout(&self) -> bool {
        matches!(self, Self::ResponseBodyTimeout { .. })
    }
}

impl fmt::Display for HttpSendError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Request(err) => write!(f, "{}", err),
            Self::ResponseHeaderTimeout { timeout_secs } => {
                write!(
                    f,
                    "upstream response header timeout after {}s",
                    timeout_secs
                )
            }
            Self::ResponseBodyTimeout { timeout_secs } => {
                write!(f, "upstream response body timeout after {}s", timeout_secs)
            }
        }
    }
}

impl std::error::Error for HttpSendError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Request(err) => Some(err),
            Self::ResponseHeaderTimeout { .. } => None,
            Self::ResponseBodyTimeout { .. } => None,
        }
    }
}

pub async fn send_with_response_header_timeout(
    request: RequestBuilder,
    timeout_secs: u64,
) -> Result<Response, HttpSendError> {
    if timeout_secs == 0 {
        return request.send().await.map_err(HttpSendError::Request);
    }

    match timeout(Duration::from_secs(timeout_secs), request.send()).await {
        Ok(result) => result.map_err(HttpSendError::Request),
        Err(_) => Err(HttpSendError::ResponseHeaderTimeout { timeout_secs }),
    }
}

pub async fn response_text_with_body_timeout(
    response: Response,
    timeout_secs: u64,
) -> Result<String, HttpSendError> {
    if timeout_secs == 0 {
        return response.text().await.map_err(HttpSendError::Request);
    }

    match timeout(Duration::from_secs(timeout_secs), response.text()).await {
        Ok(result) => result.map_err(HttpSendError::Request),
        Err(_) => Err(HttpSendError::ResponseBodyTimeout { timeout_secs }),
    }
}

pub async fn response_bytes_with_body_timeout(
    response: Response,
    timeout_secs: u64,
) -> Result<Bytes, HttpSendError> {
    if timeout_secs == 0 {
        return response.bytes().await.map_err(HttpSendError::Request);
    }

    match timeout(Duration::from_secs(timeout_secs), response.bytes()).await {
        Ok(result) => result.map_err(HttpSendError::Request),
        Err(_) => Err(HttpSendError::ResponseBodyTimeout { timeout_secs }),
    }
}

impl ProxyConfig {
    /// 从 url 创建代理配置
    pub fn new(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            username: None,
            password: None,
        }
    }

    /// 设置认证信息
    pub fn with_auth(mut self, username: impl Into<String>, password: impl Into<String>) -> Self {
        self.username = Some(username.into());
        self.password = Some(password.into());
        self
    }

    fn scheme_for_log(&self) -> &str {
        url::Url::parse(&self.url)
            .ok()
            .map(|url| match url.scheme() {
                "http" => "http",
                "https" => "https",
                "socks5" => "socks5",
                "socks5h" => "socks5h",
                _ => "other",
            })
            .unwrap_or("invalid")
    }
}

/// 构建 HTTP Client
///
/// # Arguments
/// * `proxy` - 可选的代理配置
/// * `timeout_secs` - 超时时间（秒）
///
/// # Returns
/// 配置好的 reqwest::Client
pub fn build_client(
    proxy: Option<&ProxyConfig>,
    timeout_secs: u64,
    tls_backend: TlsBackend,
) -> anyhow::Result<Client> {
    let mut builder = Client::builder();
    if timeout_secs > 0 {
        builder = builder.timeout(Duration::from_secs(timeout_secs));
    }

    match tls_backend {
        TlsBackend::Rustls => {
            builder = builder.use_rustls_tls();
        }
        TlsBackend::NativeTls => {
            #[cfg(feature = "native-tls")]
            {
                builder = builder.use_native_tls();
            }
            #[cfg(not(feature = "native-tls"))]
            {
                anyhow::bail!("此构建版本未包含 native-tls 后端，请在配置中改用 rustls");
            }
        }
    }

    if let Some(proxy_config) = proxy {
        let mut proxy = Proxy::all(&proxy_config.url)?;

        // 设置代理认证
        if let (Some(username), Some(password)) = (&proxy_config.username, &proxy_config.password) {
            proxy = proxy.basic_auth(username, password);
        }

        builder = builder.proxy(proxy);
        tracing::debug!(
            scheme = proxy_config.scheme_for_log(),
            "HTTP Client 使用代理"
        );
    }

    Ok(builder.build()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn local_test_client() -> Client {
        Client::builder()
            .no_proxy()
            .build()
            .expect("local test client")
    }

    #[test]
    fn test_proxy_config_new() {
        let config = ProxyConfig::new("http://127.0.0.1:7890");
        assert_eq!(config.url, "http://127.0.0.1:7890");
        assert!(config.username.is_none());
        assert!(config.password.is_none());
    }

    #[test]
    fn test_proxy_config_with_auth() {
        let config = ProxyConfig::new("socks5://127.0.0.1:1080").with_auth("user", "pass");
        assert_eq!(config.url, "socks5://127.0.0.1:1080");
        assert_eq!(config.username, Some("user".to_string()));
        assert_eq!(config.password, Some("pass".to_string()));
    }

    #[test]
    fn proxy_config_debug_redacts_url_and_authentication() {
        let config = ProxyConfig::new("http://url-user:url-pass@proxy.example.invalid:8080/path")
            .with_auth("configured-user", "configured-pass");

        let output = format!("{config:?}");
        for secret in [
            config.url.as_str(),
            config.username.as_deref().unwrap(),
            config.password.as_deref().unwrap(),
            "url-user",
            "url-pass",
            "proxy.example.invalid",
        ] {
            assert!(!output.contains(secret), "Debug output leaked {secret:?}");
        }
        assert!(output.contains("scheme: \"http\""));
        assert!(output.contains("username_present: true"));
    }

    #[test]
    fn test_build_client_without_proxy() {
        let client = build_client(None, 30, TlsBackend::Rustls);
        assert!(client.is_ok());
    }

    #[test]
    fn test_build_client_without_total_timeout() {
        let client = build_client(None, 0, TlsBackend::Rustls);
        assert!(client.is_ok());
    }

    #[test]
    fn test_build_client_with_proxy() {
        let config = ProxyConfig::new("http://127.0.0.1:7890");
        let client = build_client(Some(&config), 30, TlsBackend::Rustls);
        assert!(client.is_ok());
    }

    #[tokio::test]
    async fn send_with_response_header_timeout_expires_before_response_headers() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let Ok((_stream, _peer)) = listener.accept().await else {
                return;
            };
            tokio::time::sleep(Duration::from_secs(5)).await;
        });

        let client = local_test_client();
        let err = send_with_response_header_timeout(client.get(format!("http://{}", addr)), 1)
            .await
            .expect_err("server accepts connection but never sends response headers");

        assert!(err.is_response_header_timeout());
        server.abort();
    }

    #[tokio::test]
    async fn response_text_with_body_timeout_expires_after_response_headers() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let Ok((mut stream, _peer)) = listener.accept().await else {
                return;
            };
            let mut request = [0_u8; 1024];
            let _ = stream.read(&mut request).await;
            let _ = stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\n")
                .await;
            tokio::time::sleep(Duration::from_secs(5)).await;
        });

        let client = local_test_client();
        let response = send_with_response_header_timeout(client.get(format!("http://{}", addr)), 5)
            .await
            .expect("response headers should arrive before timeout");
        let err = response_text_with_body_timeout(response, 1)
            .await
            .expect_err("server sends headers but stalls before declared body completes");

        assert!(err.is_response_body_timeout());
        server.abort();
    }
}
