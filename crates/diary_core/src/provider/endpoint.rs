//! endpoint 的主机分类：**只有严格 loopback 才算「本机」**。
//!
//! 这一片只回答一个问题：这个 endpoint 是不是指向**运行核心的这台机器**？
//! 用途只有一个——本机服务通常不校验密钥，所以「没有凭据」不该在**发请求之前**
//! 被核心拦下（见 `provider::models::list_models` 与 `provider::test`）。
//!
//! # 只认 loopback，私有网段不算
//!
//! 算「本机」的只有三种写法：
//!
//! - IPv4 回环 `127.0.0.0/8`（`127.0.0.1`、`127.1.2.3`……）；
//! - IPv6 回环 `::1`（带方括号的 `[::1]` 也认）；
//! - 主机名 `localhost`（大小写不敏感）。
//!
//! **私有网段（`10.0.0.0/8`、`172.16.0.0/12`、`192.168.0.0/16`）刻意不算本机。**
//! 那是「局域网里的另一台机器」，不是这台机器：把「没有凭据也放行」扩到私有网段，
//! 等于让一台**别人的**服务器在我们没配置任何凭据时也收到请求——风险完全不同。
//! 那类服务该由用户填一条凭据（哪怕服务端会忽略它），或者由将来的产品决定显式
//! 支持「已知的无鉴权局域网服务」；本函数不做这个扩大。
//!
//! 判定只看字面主机名，**不解析 DNS**：`localhost.evil.example`、`127.0.0.1.nip.io`
//! 这类名字一律不算本机。带端口、带路径、带 `http://` / `https://` 前缀、带 userinfo
//! 都能正确取出主机；没有 scheme 的 `localhost:1234` 也认。
//!
//! # 为什么不用通用 URL 解析库
//!
//! 与 `provider::normalize_endpoint` 同一个理由：这里要的只是「协议/端口/路径之后
//! 的主机是什么」，多引一个依赖不划算。解析失败（空串、只有路径、主机为空）一律
//! 返回 `false`——**判不出来就不放行**是安全的方向。

/// `endpoint` 是否指向本机（严格 loopback）。判定清单见模块文档。
///
/// true 只覆盖 `127.0.0.0/8`、`::1` 与 `localhost`；**私有网段不算**。
pub fn is_loopback_endpoint(endpoint: &str) -> bool {
    let Some(host) = endpoint_host(endpoint) else {
        return false;
    };
    if host.eq_ignore_ascii_case("localhost") || host == "::1" {
        return true;
    }
    is_loopback_v4(&host)
}

/// 从 endpoint 里取出小写主机名。取不出来就是 `None`（调用方按「不是本机」处理）。
fn endpoint_host(endpoint: &str) -> Option<String> {
    let trimmed = endpoint.trim();
    if trimmed.is_empty() {
        return None;
    }
    // 有 scheme 就丢掉；没有 scheme 的 `localhost:1234` 也接受。
    let rest = trimmed
        .split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or(trimmed);
    // authority 在第一个 `/`、`?`、`#` 之前。
    let authority = rest.split(['/', '?', '#']).next()?.trim();
    // userinfo（`user:pass@host`）不属于主机。
    let authority = authority.rsplit('@').next()?;
    let host = if let Some(without_open) = authority.strip_prefix('[') {
        // IPv6 字面量：`[::1]:8080` → `::1`。
        without_open.split(']').next()?
    } else if authority.matches(':').count() > 1 {
        // 没有方括号的 IPv6 字面量（`::1`）：整段都是主机，没有端口可拆。
        authority
    } else {
        // `host:port` 或纯主机名。
        authority.split(':').next()?
    };
    // DNS 的绝对写法 `localhost.` 与 `localhost` 是同一个名字；只去掉一个结尾点。
    let host = host.trim().strip_suffix('.').unwrap_or(host.trim());
    if host.is_empty() {
        None
    } else {
        Some(host.to_ascii_lowercase())
    }
}

/// `127.0.0.0/8`：四段十进制、每段 0..=255，且第一段是 127。
///
/// 不接受 `127.1`（inet 的缩写形式）或整数形式 `2130706433`：那些写法在不同解析器
/// 里含义不一致，**判不出来就不放行**。
fn is_loopback_v4(host: &str) -> bool {
    let octets: Vec<&str> = host.split('.').collect();
    if octets.len() != 4 {
        return false;
    }
    let mut parsed = [0_u8; 4];
    for (index, octet) in octets.iter().enumerate() {
        if octet.is_empty() || !octet.bytes().all(|byte| byte.is_ascii_digit()) {
            return false;
        }
        match octet.parse::<u8>() {
            Ok(value) => parsed[index] = value,
            Err(_) => return false,
        }
    }
    parsed[0] == 127
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loopback_endpoints_are_recognised_in_every_spelling() {
        for endpoint in [
            "http://127.0.0.1:11434/v1",
            "127.0.0.1",
            "http://[::1]:8080",
            "localhost:1234",
            "HTTP://LOCALHOST/v1",
            "https://127.1.2.3/models",
            "http://user:pass@127.0.0.1:11434/v1",
            "http://localhost./v1",
            "  http://127.0.0.1:11434/v1  ",
        ] {
            assert!(is_loopback_endpoint(endpoint), "「{endpoint}」应当算本机");
        }
    }

    #[test]
    fn remote_and_private_network_endpoints_are_not_loopback() {
        for endpoint in [
            "https://api.deepseek.com",
            "http://192.168.1.10:11434",
            "http://10.0.0.5:8000",
            "http://172.20.0.1",
            "http://172.16.0.1",
            "http://0.0.0.0:11434",
            "http://localhost.evil.example",
            "http://127.0.0.1.nip.io",
            "http://2130706433",
            "http://[::2]:8080",
            "",
            "   ",
            "/v1",
        ] {
            assert!(!is_loopback_endpoint(endpoint), "「{endpoint}」不该算本机");
        }
    }

    /// 判定必须与端口、路径、scheme 的大小写无关；主机名以外的部分不参与。
    #[test]
    fn only_the_host_matters() {
        assert!(is_loopback_endpoint("http://LOCALHOST:9999/a/b?c=d#e"));
        assert!(is_loopback_endpoint("https://[::1]/v1/models"));
        assert!(!is_loopback_endpoint("https://api.deepseek.com/localhost"));
        assert!(!is_loopback_endpoint(
            "https://api.deepseek.com:443/v1?host=localhost"
        ));
    }
}
