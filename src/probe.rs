//! 落地IP探测模块。
//!
//! 通过代理向 Cloudflare 的 trace 端点发起 HTTPS 请求，解析出口IP与国家码。
//! 因为请求经由 Clash 的代理端口（HTTP CONNECT 隧道），Clash 会自动完成链式代理，
//! 我们读到的 `ip=` 即为最终出口的「落地IP」。

use std::net::IpAddr;
use std::str::FromStr;
use std::time::Duration;

use serde_json::Value;

use crate::proxy::EffectiveProxy;

/// 默认探测端点：Cloudflare trace，返回纯文本 `key=value`，含 `ip=` 与 `loc=`。
pub const CLOUDFLARE_TRACE_URL: &str = "https://www.cloudflare.com/cdn-cgi/trace";

/// 已验证可返回出口 IP 与国家码的 HTTPS 接口。
///
/// 每轮探测按顺序请求，成功后立即停止；只有当前接口失败或响应无法解析时才切换下一个。
pub const DEFAULT_PROBE_URLS: &[&str] = &[
    "https://api.ip.sb/geoip/",
    "https://api.ipquery.io/?format=json",
    "https://api.myip.com/",
    "https://api.db-ip.com/v2/free/self",
    "https://free.freeipapi.com/api/json",
    "https://ipapi.co/json/",
    "https://1.1.1.1/cdn-cgi/trace",
    CLOUDFLARE_TRACE_URL,
];

/// 一次探测得到的落地IP信息。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IpInfo {
    /// 出口（落地）IP 地址。
    pub ip: String,
    /// 两位 ISO 国家码（大写），如 `JP`；无法获取时为空字符串。
    pub country_code: String,
}

impl IpInfo {
    /// 任务栏展示文本，如 `JP  1.2.3.4`；无国家码时仅显示 IP。
    ///
    /// 注：Windows 故意把国旗 emoji 渲染成方块国家码而非真实国旗，
    /// 因此这里直接用两位国家码文本，避免方块与重复。
    pub fn display_text(&self) -> String {
        if self.country_code.is_empty() {
            self.ip.clone()
        } else {
            format!("{}  {}", self.country_code, self.ip)
        }
    }
}

/// 使用可选自定义接口和内置接口列表探测出口 IP，失败时自动切换下一个地址。
#[allow(non_snake_case)]
pub fn probeWithFallback(
    proxy: &EffectiveProxy,
    preferred_url: Option<&str>,
    timeout: Duration,
) -> Result<IpInfo, String> {
    let agent = build_agent(proxy, timeout)?;
    let preferred_url = preferred_url.map(str::trim).filter(|url| !url.is_empty());
    let mut errors: Vec<String> = Vec::new();

    // 用户明确配置的非内置地址优先执行，保留原有自定义能力。
    if let Some(url) = preferred_url {
        if !DEFAULT_PROBE_URLS.contains(&url) {
            match requestProbe(&agent, url) {
                Ok(info) => return Ok(info),
                Err(error) => errors.push(format!("{url}: {error}")),
            }
        }
    }

    // 内置接口按顺序故障转移，任意一个成功后立即返回。
    for url in DEFAULT_PROBE_URLS {
        match requestProbe(&agent, url) {
            Ok(info) => return Ok(info),
            Err(error) => errors.push(format!("{url}: {error}")),
        }
    }

    Err(format!("所有出口 IP 接口均失败: {}", errors.join(" | ")))
}

/// 根据有效代理构建 ureq Agent，并显式装配 native-tls（SChannel）连接器。
fn build_agent(proxy: &EffectiveProxy, timeout: Duration) -> Result<ureq::Agent, String> {
    let connector = native_tls::TlsConnector::new().map_err(|e| format!("初始化 TLS 失败: {e}"))?;

    let mut builder = ureq::AgentBuilder::new()
        .timeout(timeout)
        .tls_connector(std::sync::Arc::new(connector));

    if let Some(url) = proxy.url() {
        let p = ureq::Proxy::new(&url).map_err(|e| format!("代理地址非法 {url}: {e}"))?;
        builder = builder.proxy(p);
    }

    Ok(builder.build())
}

/// 请求单个接口，并把纯文本或 JSON 响应转换为统一的出口 IP 信息。
#[allow(non_snake_case)]
fn requestProbe(agent: &ureq::Agent, url: &str) -> Result<IpInfo, String> {
    let body = agent
        .get(url)
        .set("User-Agent", "ipDetect/0.1")
        .call()
        .map_err(|error| format!("请求失败: {error}"))?
        .into_string()
        .map_err(|error| format!("读取响应失败: {error}"))?;

    parseProbeResponse(&body).ok_or_else(|| "响应中没有有效的 IP 与国家码".to_string())
}

/// 自动解析 Cloudflare trace 和常见免费 IP API 的 JSON 响应。
#[allow(non_snake_case)]
pub fn parseProbeResponse(body: &str) -> Option<IpInfo> {
    if let Some(info) = parse_trace(body) {
        return Some(info);
    }

    let json: Value = serde_json::from_str(body).ok()?;
    let ip = findJsonString(&json, &["ip", "ipAddress", "query"])?;
    IpAddr::from_str(&ip).ok()?;

    let country_code = findJsonString(
        &json,
        &["country_code", "countryCode", "cc", "iso_code", "country"],
    )
    .filter(|value| {
        value.len() == 2
            && value
                .chars()
                .all(|character| character.is_ascii_alphabetic())
    })
    .unwrap_or_default()
    .to_uppercase();

    Some(IpInfo { ip, country_code })
}

/// 在任意层级的 JSON 对象中按候选字段名查找第一个非空字符串。
#[allow(non_snake_case)]
fn findJsonString(value: &Value, keys: &[&str]) -> Option<String> {
    match value {
        Value::Object(object) => {
            // 优先检查当前对象，避免嵌套对象中的辅助字段覆盖顶层出口 IP。
            for key in keys {
                if let Some(text) = object.get(*key).and_then(Value::as_str) {
                    let text = text.trim();
                    if !text.is_empty() {
                        return Some(text.to_string());
                    }
                }
            }

            // 当前层没有目标字段时，再递归检查嵌套对象。
            object
                .values()
                .find_map(|nested_value| findJsonString(nested_value, keys))
        }
        Value::Array(array) => array
            .iter()
            .find_map(|nested_value| findJsonString(nested_value, keys)),
        _ => None,
    }
}

/// 解析 Cloudflare trace 文本，提取 `ip` 与 `loc`。
pub fn parse_trace(body: &str) -> Option<IpInfo> {
    let mut ip: Option<String> = None;
    let mut loc: Option<String> = None;

    for line in body.lines() {
        if let Some(v) = line.strip_prefix("ip=") {
            ip = Some(v.trim().to_string());
        } else if let Some(v) = line.strip_prefix("loc=") {
            loc = Some(v.trim().to_uppercase());
        }
    }

    ip.map(|ip| IpInfo {
        ip,
        country_code: loc.unwrap_or_default(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_trace_extracts_ip_and_loc() {
        let body = "fl=123\nh=www.cloudflare.com\nip=1.2.3.4\nts=1\nloc=JP\ncolo=NRT\n";
        let info = parse_trace(body).unwrap();
        assert_eq!(info.ip, "1.2.3.4");
        assert_eq!(info.country_code, "JP");
    }

    #[test]
    fn parse_trace_without_loc() {
        let body = "ip=9.9.9.9\nfoo=bar\n";
        let info = parse_trace(body).unwrap();
        assert_eq!(info.ip, "9.9.9.9");
        assert_eq!(info.country_code, "");
    }

    #[test]
    fn parse_trace_no_ip_is_none() {
        assert!(parse_trace("loc=US\n").is_none());
    }

    #[test]
    fn display_text_format() {
        let info = IpInfo {
            ip: "1.2.3.4".to_string(),
            country_code: "JP".to_string(),
        };
        assert_eq!(info.display_text(), "JP  1.2.3.4");
    }

    #[test]
    fn display_text_without_country() {
        let info = IpInfo {
            ip: "1.2.3.4".to_string(),
            country_code: String::new(),
        };
        assert_eq!(info.display_text(), "1.2.3.4");
    }

    /// 地址⑤ api.ip.sb 的响应必须能解析出口 IP 与国家码。
    #[test]
    #[allow(non_snake_case)]
    fn parseApiIpSbResponse() {
        let body = r#"{"ip":"20.27.94.129","country_code":"JP","city":"Osaka"}"#;
        let info = parseProbeResponse(body).unwrap();
        assert_eq!(info.ip, "20.27.94.129");
        assert_eq!(info.country_code, "JP");
    }

    /// 不同免费接口采用不同字段名时仍应得到统一的出口 IP 信息。
    #[test]
    #[allow(non_snake_case)]
    fn parseAlternativeJsonResponses() {
        let db_ip = r#"{"ipAddress":"4.197.64.24","countryCode":"AU"}"#;
        let my_ip = r#"{"ip":"70.153.76.30","country":"Indonesia","cc":"ID"}"#;
        let ip_query =
            r#"{"ip":"20.210.211.204","location":{"country":"Japan","country_code":"JP"}}"#;

        assert_eq!(parseProbeResponse(db_ip).unwrap().country_code, "AU");
        assert_eq!(parseProbeResponse(my_ip).unwrap().country_code, "ID");
        assert_eq!(parseProbeResponse(ip_query).unwrap().country_code, "JP");
    }

    /// 默认故障转移列表只允许 HTTPS，并包含用户指定仓库中的地址⑤。
    #[test]
    #[allow(non_snake_case)]
    fn defaultEndpointsAreSecureAndContainAddressFive() {
        assert!(DEFAULT_PROBE_URLS
            .iter()
            .all(|url| url.starts_with("https://")));
        assert!(DEFAULT_PROBE_URLS.contains(&"https://api.ip.sb/geoip/"));
    }
}
