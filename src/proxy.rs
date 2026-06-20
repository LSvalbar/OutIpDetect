//! 系统代理探测模块。
//!
//! 决定落地IP探测请求应当通过哪个代理：手动覆盖 > 系统代理 > 直连(TUN)。
//! - 系统代理模式：Clash 等软件会把代理写入 Windows 注册表的 Internet Settings；
//! - TUN 模式：系统代理通常未设置，但流量已被虚拟网卡接管，直连请求同样会走代理链路。

use winreg::enums::HKEY_CURRENT_USER;
use winreg::RegKey;

/// 代理来源，用于在界面/日志中说明当前探测走的是哪条路径。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProxySource {
    /// 用户在设置中手动指定
    Manual,
    /// 从系统注册表读取
    System,
    /// 直连（无系统代理，通常为 TUN 模式）
    Direct,
}

/// 解析得到的「有效代理」。`url` 为 `None` 表示直连。
#[derive(Debug, Clone)]
pub struct EffectiveProxy {
    url: Option<String>,
    source: ProxySource,
}

impl EffectiveProxy {
    /// 返回代理 URL（形如 `http://127.0.0.1:7897`）；直连时为 `None`。
    pub fn url(&self) -> Option<String> {
        self.url.clone()
    }

    /// 返回代理来源。
    pub fn source(&self) -> ProxySource {
        self.source
    }
}

/// 解析有效代理：优先手动覆盖，其次系统代理，否则直连。
///
/// `manual_override` 为设置中用户填写的代理地址（可为空字符串表示不覆盖）。
pub fn resolve_effective_proxy(manual_override: Option<&str>) -> EffectiveProxy {
    if let Some(raw) = manual_override {
        let raw = raw.trim();
        if !raw.is_empty() {
            return EffectiveProxy {
                url: Some(normalize_proxy_url(raw)),
                source: ProxySource::Manual,
            };
        }
    }

    match read_system_proxy() {
        Some(url) => EffectiveProxy {
            url: Some(url),
            source: ProxySource::System,
        },
        None => EffectiveProxy {
            url: None,
            source: ProxySource::Direct,
        },
    }
}

/// 从注册表读取 Windows 系统代理设置；未启用或读取失败时返回 `None`。
pub fn read_system_proxy() -> Option<String> {
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let key = hkcu
        .open_subkey(r"Software\Microsoft\Windows\CurrentVersion\Internet Settings")
        .ok()?;

    // ProxyEnable 为 DWORD：1 表示启用系统代理
    let enable: u32 = key.get_value("ProxyEnable").ok()?;
    if enable == 0 {
        return None;
    }

    let server: String = key.get_value("ProxyServer").ok()?;
    parse_proxy_server(&server)
}

/// 解析注册表 `ProxyServer` 字段。
///
/// 可能是两种形式：
/// - `127.0.0.1:7897`（所有协议共用）
/// - `http=127.0.0.1:7897;https=127.0.0.1:7897;ftp=...;socks=...`（按协议分别配置）
///
/// 优先取 http/https 的地址，其次取任意一个，最后补上协议前缀。
pub fn parse_proxy_server(raw: &str) -> Option<String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }

    if raw.contains('=') {
        let mut http_addr: Option<String> = None;
        let mut any_addr: Option<String> = None;
        for part in raw.split(';') {
            let part = part.trim();
            if let Some((proto, addr)) = part.split_once('=') {
                let addr = addr.trim();
                if addr.is_empty() {
                    continue;
                }
                any_addr.get_or_insert_with(|| addr.to_string());
                if proto.eq_ignore_ascii_case("http") || proto.eq_ignore_ascii_case("https") {
                    http_addr = Some(addr.to_string());
                }
            }
        }
        http_addr.or(any_addr).map(|a| normalize_proxy_url(&a))
    } else {
        Some(normalize_proxy_url(raw))
    }
}

/// 给代理地址补上协议前缀（缺省按 http 处理）。
pub fn normalize_proxy_url(addr: &str) -> String {
    let addr = addr.trim();
    if addr.contains("://") {
        addr.to_string()
    } else {
        format!("http://{addr}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_simple_proxy() {
        assert_eq!(
            parse_proxy_server("127.0.0.1:7897"),
            Some("http://127.0.0.1:7897".to_string())
        );
    }

    #[test]
    fn parse_protocol_specific_prefers_http() {
        let raw = "ftp=10.0.0.1:1;https=127.0.0.1:7897;socks=127.0.0.1:7898";
        assert_eq!(
            parse_proxy_server(raw),
            Some("http://127.0.0.1:7897".to_string())
        );
    }

    #[test]
    fn parse_protocol_specific_fallback_any() {
        let raw = "socks=127.0.0.1:7898";
        assert_eq!(
            parse_proxy_server(raw),
            Some("http://127.0.0.1:7898".to_string())
        );
    }

    #[test]
    fn parse_empty_is_none() {
        assert_eq!(parse_proxy_server("  "), None);
    }

    #[test]
    fn normalize_keeps_existing_scheme() {
        assert_eq!(
            normalize_proxy_url("socks5://127.0.0.1:7898"),
            "socks5://127.0.0.1:7898"
        );
    }

    #[test]
    fn manual_override_wins() {
        let eff = resolve_effective_proxy(Some("127.0.0.1:1080"));
        assert_eq!(eff.source(), ProxySource::Manual);
        assert_eq!(eff.url(), Some("http://127.0.0.1:1080".to_string()));
    }
}
