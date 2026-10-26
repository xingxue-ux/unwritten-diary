//! Provider 的 HTTP 传输层：可注入的 `Transport` + 真实实现 + 测试替身。
//!
//! # 为什么要抽象这一层
//!
//! 1. **CI 不联网**：所有 Provider 交互都走 `Transport`，测试注入回放/记账替身，
//!    一条真实请求都不发（#93 的质量集同样靠它）；
//! 2. **默认不联网可证明**：注入一个「一旦被调用就 panic」的替身，跑一遍本地功能，
//!    不被调用就等于证明了没有出网路径（见 `PanicTransport`）；
//! 3. **诚实**：回放替身会记账每一次调用，所以「是否真的发生了模型调用」是数出来的，
//!    不是猜的。
//!
//! # 明文不进日志
//!
//! `HttpRequest` 带着 `Authorization` 头（密钥明文）。它的 `Debug` 是手写的：
//! 敏感头的值一律替换成 `***`，请求体只报字节数。谁 `{:?}` 了请求都不会泄漏。

use std::sync::Mutex;
use std::time::Duration;

/// 一次出站请求。**包含密钥明文**（在头部），所以只应该交给 `Transport`。
#[derive(Clone)]
pub struct HttpRequest {
    pub method: String,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    /// 本次请求的总超时（毫秒）。由适配器按用途给（连接测试短、生成长）。
    pub timeout_ms: u64,
}

impl HttpRequest {
    pub fn new(method: &str, url: impl Into<String>, timeout_ms: u64) -> Self {
        Self {
            method: method.to_owned(),
            url: url.into(),
            headers: Vec::new(),
            body: Vec::new(),
            timeout_ms,
        }
    }

    pub fn header(mut self, name: &str, value: impl Into<String>) -> Self {
        self.headers.push((name.to_owned(), value.into()));
        self
    }

    pub fn body(mut self, body: Vec<u8>) -> Self {
        self.body = body;
        self
    }

    pub fn get_header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    /// 脱敏摘要，给错误消息与排查用：**不含**任何头部的值。
    pub fn summary(&self) -> String {
        format!("{} {}（{} 字节请求体）", self.method, self.url, self.body.len())
    }
}

/// 手写 `Debug`：敏感头的值一律不打印。
impl std::fmt::Debug for HttpRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let headers: Vec<String> = self
            .headers
            .iter()
            .map(|(name, value)| {
                if is_sensitive_header(name) {
                    format!("{name}: ***")
                } else {
                    format!("{name}: {value}")
                }
            })
            .collect();
        f.debug_struct("HttpRequest")
            .field("method", &self.method)
            .field("url", &self.url)
            .field("headers", &headers)
            .field("body_bytes", &self.body.len())
            .field("timeout_ms", &self.timeout_ms)
            .finish()
    }
}

/// 这些头的值不进任何输出（密钥、Cookie）。
fn is_sensitive_header(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    name == "authorization" || name == "api-key" || name == "x-api-key" || name == "cookie"
}

/// 一次出站响应。
#[derive(Debug, Clone)]
pub struct HttpResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl HttpResponse {
    pub fn new(status: u16, body: impl Into<Vec<u8>>) -> Self {
        Self {
            status,
            headers: Vec::new(),
            body: body.into(),
        }
    }

    pub fn with_header(mut self, name: &str, value: impl Into<String>) -> Self {
        self.headers.push((name.to_owned(), value.into()));
        self
    }

    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    /// 响应体的有损 UTF-8 文本（解析 JSON 用；也用于错误消息的短摘录）。
    pub fn body_text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    /// JSON 响应体短摘录：**先擦密钥、再截断**成最长 200 个字符，换行折成空格。
    ///
    /// 顺序不能反：密钥可能比 200 个字符还长，先截断的话 `redact` 就匹配不到完整
    /// 密钥，明文前缀会留在摘录里。擦除与截断都在这里做，调用方不必也不能自己
    /// 「先截断再擦」。
    ///
    /// 只用于错误消息；响应体本身从不原样返回给上层。
    pub fn excerpt(&self, secret: Option<&str>) -> String {
        let text =
            super::normalize::redact(&self.body_text(), secret).replace(['\n', '\r'], " ");
        let mut excerpt: String = text.chars().take(200).collect();
        if text.chars().count() > 200 {
            excerpt.push('…');
        }
        excerpt
    }
}

/// 传输失败。**只有「没拿到响应」的失败**才在这里；有响应的失败（4xx/5xx）
/// 走 `HttpResponse`，由 `normalize` 归一。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransportError {
    /// 连接/读写超时。
    Timeout { message: String },
    /// 连不上（DNS、拒绝连接、断网、TLS 握手失败）。
    Connect { message: String },
    /// 传输层的其它错误。
    Other { message: String },
}

impl std::fmt::Display for TransportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Timeout { message } => write!(f, "请求超时：{message}"),
            Self::Connect { message } => write!(f, "连接失败：{message}"),
            Self::Other { message } => write!(f, "传输错误：{message}"),
        }
    }
}

/// 出站请求的唯一出口。**核心只在两处调用它**：用户显式发起的连接测试，
/// 与真的被领取执行的 Provider 作业。没有「后台预热」「启动探测」之类的路径。
pub trait Transport: Send + Sync {
    fn send(&self, request: &HttpRequest) -> std::result::Result<HttpResponse, TransportError>;
}

/// 真实实现：`ureq` + rustls。
///
/// - **同步**：核心整体是同步的，不引 async runtime；
/// - **不跟随重定向**（`redirects(0)`）：跟随会把 `Authorization` 头带到另一个
///   主机上。3xx 当「端点配错了」处理（`model_unavailable`），不静默跟随；
/// - **超时**：每个请求自己的 `timeout_ms`（连接测试 20 秒、生成 120 秒），
///   不给 0（= 不超时）；
/// - **TLS 根证书**：ureq 的 `tls` 特性用打包进来的 Mozilla 根集合
///   （webpki-roots），**不读系统证书库**——企业自签代理的场景需要后续显式支持
///   （见 m3 文档「没有做到的」）。
pub struct UreqTransport {
    agent: ureq::Agent,
}

impl UreqTransport {
    pub fn new() -> Self {
        let agent = ureq::AgentBuilder::new()
            .redirects(0)
            .user_agent(concat!("BuXieRiJi/", env!("CARGO_PKG_VERSION")))
            .build();
        Self { agent }
    }
}

impl Default for UreqTransport {
    fn default() -> Self {
        Self::new()
    }
}

impl Transport for UreqTransport {
    fn send(&self, request: &HttpRequest) -> std::result::Result<HttpResponse, TransportError> {
        // Compile-time product policy, before DNS, sockets or request creation.
        if cfg!(feature = "offline-only") {
            return Err(TransportError::Other { message: "离线版不支持网络请求".to_owned() });
        }
        let mut outgoing = self
            .agent
            .request(&request.method, &request.url)
            .timeout(Duration::from_millis(request.timeout_ms.max(1)));
        for (name, value) in &request.headers {
            outgoing = outgoing.set(name, value);
        }
        let outcome = if request.body.is_empty() {
            outgoing.call()
        } else {
            outgoing.send(&request.body[..])
        };

        match outcome {
            Ok(response) => http_response(response),
            // ureq 把「有响应的错误状态」也走 Err 分支：先把响应捞出来，
            // 让 4xx/5xx 走归一化（那才是能给出可执行下一步的地方）。
            Err(ureq::Error::Status(_, response)) => http_response(response),
            Err(ureq::Error::Transport(transport)) => {
                let message = transport.to_string();
                let text = message.to_ascii_lowercase();
                if text.contains("timed out") || text.contains("timeout") {
                    Err(TransportError::Timeout { message })
                } else {
                    Err(TransportError::Connect { message })
                }
            }
        }
    }
}

fn http_response(response: ureq::Response) -> std::result::Result<HttpResponse, TransportError> {
    let status = response.status();
    let headers: Vec<(String, String)> = response
        .headers_names()
        .into_iter()
        .filter_map(|name| {
            response
                .header(&name)
                .map(|value| (name.clone(), value.to_owned()))
        })
        .collect();
    let declared = declared_body_length(&headers);
    let mut body = Vec::new();
    // 读失败**不能吞**：读超时是 `provider_timeout`（可重试），别的 IO 错误是
    // `network_unavailable`。把残体交给上层会变成「JSON 解析失败」→
    // `model_unavailable`（不可重试），语义正好反了。
    response
        .into_reader()
        .read_to_end(&mut body)
        .map_err(|error| {
            let message = format!("读取响应体失败：{error}");
            if is_timeout(&error) {
                TransportError::Timeout { message }
            } else {
                TransportError::Other { message }
            }
        })?;
    // `Content-Length` 是远端自己的声明：短读说明响应体被截断（连接断开、代理超时），
    // 绝不能当成合法的短体/空体。分块传输下这个头不权威，`declared_body_length`
    // 已经跳过了那种情况。
    if let Some(expected) = declared {
        let actual = body.len() as u64;
        if actual < expected {
            return Err(TransportError::Other {
                message: format!(
                    "响应体被截断：Content-Length 声明 {expected} 字节，实际只读到 {actual} 字节"
                ),
            });
        }
    }
    Ok(HttpResponse {
        status,
        headers,
        body,
    })
}

/// `Content-Length` 声明的响应体长度（没有、解析不了、或分块传输时为 `None`）。
///
/// 有 `Transfer-Encoding` 时按 RFC 9112 忽略 `Content-Length`：那种响应体的边界由
/// 分块格式决定，拿长度去校验会误报。
fn declared_body_length(headers: &[(String, String)]) -> Option<u64> {
    let chunked = headers
        .iter()
        .any(|(name, _)| name.eq_ignore_ascii_case("transfer-encoding"));
    if chunked {
        return None;
    }
    headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, value)| value.trim().parse().ok())
}

/// 这个 IO 错误是不是超时。ureq 会把 socket 的 `WouldBlock` 归一成 `TimedOut`，
/// 这里两种都认（不同平台/不同 reader 的实现会给出不同的 kind）。
fn is_timeout(error: &std::io::Error) -> bool {
    matches!(
        error.kind(),
        std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
    ) || error
        .to_string()
        .to_ascii_lowercase()
        .contains("timed out")
}

/// 回放替身：按脚本回响应，并把每一次请求记下来。
///
/// 用在 CI（#93 质量集、端到端路径）与所有 Provider 相关测试：
/// **没有网络也能跑，而且「发生了几次调用」是数出来的**。
pub struct ScriptedTransport {
    state: Mutex<ScriptedState>,
}

struct ScriptedState {
    responses: std::collections::VecDeque<std::result::Result<HttpResponse, TransportError>>,
    /// 脚本用完之后的行为：`true` 返回一个明确的错误（默认），`false` 就一直返回
    /// 最后一个响应（用于「重试直到成功」这类测试）。
    repeat_last: bool,
    requests: Vec<HttpRequest>,
}

impl ScriptedTransport {
    /// 按顺序回放的响应脚本。
    pub fn new(
        responses: Vec<std::result::Result<HttpResponse, TransportError>>,
    ) -> Self {
        Self {
            state: Mutex::new(ScriptedState {
                responses: responses.into(),
                repeat_last: false,
                requests: Vec::new(),
            }),
        }
    }

    /// 全部成功的脚本（最常用）。
    pub fn ok(responses: Vec<HttpResponse>) -> Self {
        Self::new(responses.into_iter().map(Ok).collect())
    }

    /// 脚本用完后重复最后一个响应，而不是报错。
    pub fn repeating(mut self) -> Self {
        self.state.get_mut().expect("刚建的锁不会中毒").repeat_last = true;
        self
    }

    /// 已经发出的请求（**含密钥明文**，只在测试里用；断言时请用 `call_count()`）。
    pub fn requests(&self) -> Vec<HttpRequest> {
        self.state
            .lock()
            .expect("回放替身的锁不该中毒")
            .requests
            .clone()
    }

    /// 一共发生了几次真实调用。连接测试报告里的 `actual_call_made` 靠它证明。
    pub fn call_count(&self) -> usize {
        self.state
            .lock()
            .expect("回放替身的锁不该中毒")
            .requests
            .len()
    }

    /// 第 n 次（从 0 开始）请求命中的 URL。
    pub fn url_at(&self, index: usize) -> Option<String> {
        self.state
            .lock()
            .expect("回放替身的锁不该中毒")
            .requests
            .get(index)
            .map(|request| request.url.clone())
    }
}

impl Transport for ScriptedTransport {
    fn send(&self, request: &HttpRequest) -> std::result::Result<HttpResponse, TransportError> {
        let mut state = self.state.lock().expect("回放替身的锁不该中毒");
        state.requests.push(request.clone());
        if state.repeat_last {
            if let Some(last) = state.responses.back() {
                return last.clone();
            }
        }
        match state.responses.pop_front() {
            Some(outcome) => outcome,
            None => Err(TransportError::Other {
                message: "回放脚本用完了（这次测试不该再发请求）".to_owned(),
            }),
        }
    }
}

/// 「一旦被调用就 panic」的替身：默认不联网的证明用它。
///
/// 断言「没有出网调用」不能只看记账数字（记账也可能漏记），让出网当场炸掉更硬。
#[derive(Default)]
pub struct PanicTransport {}

impl Transport for PanicTransport {
    fn send(&self, request: &HttpRequest) -> std::result::Result<HttpResponse, TransportError> {
        panic!(
            "默认不联网被破坏：没有任何已配置并启用的服务时，核心不该发起请求（{}）",
            request.summary()
        );
    }
}

/// 给「只想知道有没有被调用」的场景用：记账但什么都不做（不 panic、不发网络）。
#[derive(Default)]
pub struct CountingTransport {
    calls: Mutex<Vec<HttpRequest>>,
}

impl CountingTransport {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn call_count(&self) -> usize {
        self.calls.lock().expect("记账的锁不该中毒").len()
    }
}

impl Transport for CountingTransport {
    fn send(&self, request: &HttpRequest) -> std::result::Result<HttpResponse, TransportError> {
        self.calls
            .lock()
            .expect("记账的锁不该中毒")
            .push(request.clone());
        Err(TransportError::Connect {
            message: "记账替身不联网".to_owned(),
        })
    }
}

impl PanicTransport {
    pub fn new() -> Self {
        Self {}
    }
}

#[cfg(test)]
mod tests {
    use super::super::normalize::transport_error;
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    /// 起一个只服务一次的本地 HTTP 服务：读掉请求头，写回固定响应。
    ///
    /// `hold_ms > 0` 时写完不关连接——客户端会卡在「Content-Length 还没读满」
    /// 上直到自己的读超时；`hold_ms == 0` 时立刻关闭，模拟响应体被截断。
    fn serve_once(payload: &'static [u8], hold_ms: u64) -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").expect("绑定 loopback 端口");
        let port = listener.local_addr().expect("取本地地址").port();
        std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().expect("接受连接");
            let mut request = [0_u8; 1024];
            let _ = socket.read(&mut request);
            socket.write_all(payload).expect("写回响应头与残体");
            socket.flush().expect("flush 响应");
            if hold_ms > 0 {
                std::thread::sleep(Duration::from_millis(hold_ms));
            }
        });
        port
    }

    /// 读超时**不能**被吞成空体：否则上层把残体当 JSON 解析失败，
    /// 归成不可重试的 `model_unavailable`，与契约「读写超时 → provider_timeout，
    /// 可重试」正好相反。
    #[test]
    fn a_body_read_timeout_is_a_retryable_provider_timeout() {
        // 200 + Content-Length:1000，只写 9 字节，然后挂着连接不关。
        let port = serve_once(
            b"HTTP/1.1 200 OK\r\nContent-Length: 1000\r\nConnection: close\r\n\r\n123456789",
            2_000,
        );
        let request = HttpRequest::new(
            "GET",
            format!("http://127.0.0.1:{port}/v1/models"),
            300,
        );

        let error = match UreqTransport::new().send(&request) {
            Err(error) => error,
            Ok(response) => panic!(
                "读超时被吞成了响应（status {}，body {} 字节）",
                response.status,
                response.body.len()
            ),
        };
        assert!(
            matches!(error, TransportError::Timeout { .. }),
            "读超时应该归到 Timeout，实际 {error}"
        );
        let normalized = transport_error(&error, None);
        assert_eq!(
            normalized.code().wire(),
            "provider_timeout",
            "契约要求读写超时可重试：{normalized}"
        );
        assert!(normalized.retryable(), "读超时必须可重试：{normalized}");
        assert_ne!(normalized.code().wire(), "model_unavailable");
    }

    /// `Content-Length` 声明 1000、实际只读到 9 字节（连接提前关闭）：这是响应体
    /// 被截断，不是「合法的空体」，更不能当 JSON 解析失败。
    #[test]
    fn a_body_shorter_than_content_length_is_a_transport_error() {
        let port = serve_once(
            b"HTTP/1.1 200 OK\r\nContent-Length: 1000\r\nConnection: close\r\n\r\n123456789",
            0,
        );
        let request = HttpRequest::new(
            "GET",
            format!("http://127.0.0.1:{port}/v1/models"),
            5_000,
        );

        let error = match UreqTransport::new().send(&request) {
            Err(error) => error,
            Ok(response) => panic!(
                "短读被当成了合法响应（body {} 字节）",
                response.body.len()
            ),
        };
        assert!(
            matches!(error, TransportError::Other { .. }),
            "截断应该归到 Other，实际 {error}"
        );
        let normalized = transport_error(&error, None);
        assert_eq!(normalized.code().wire(), "network_unavailable");
        assert!(normalized.retryable(), "截断是暂时性的，应该可重试");
    }
}
