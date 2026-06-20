//! 配置模块。
//!
//! 负责读取 `%APPDATA%\ipDetect\config.toml` 中的插件探测设置。

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// 免费出口 IP 接口的最低刷新间隔：5 分钟。
pub const MIN_REFRESH_SECS: u64 = 300;

/// 应用配置。`#[serde(default)]` 确保旧配置缺字段时回退到默认值。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// 手动代理覆盖（留空 = 自动：系统代理 / 直连）。
    pub proxy_override: String,
    /// 刷新间隔（秒）。
    pub refresh_secs: u64,
    /// 探测端点 URL。
    pub probe_url: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            proxy_override: String::new(),
            refresh_secs: MIN_REFRESH_SECS,
            // 留空表示使用内置的多接口故障转移列表。
            probe_url: String::new(),
        }
    }
}

/// 把旧版本过短的刷新间隔提升到 5 分钟，同时允许用户配置更长间隔。
#[allow(non_snake_case)]
pub fn effectiveRefreshSecs(configured_secs: u64) -> u64 {
    configured_secs.max(MIN_REFRESH_SECS)
}

impl Config {
    /// 配置文件完整路径。
    pub fn config_path() -> PathBuf {
        let base = std::env::var("APPDATA").unwrap_or_else(|_| ".".to_string());
        PathBuf::from(base).join("ipDetect").join("config.toml")
    }

    /// 读取配置；文件不存在或解析失败时返回默认配置。
    pub fn load() -> Self {
        match std::fs::read_to_string(Self::config_path()) {
            Ok(content) => toml::from_str(&content).unwrap_or_default(),
            Err(_) => Self::default(),
        }
    }

    /// 保存配置到磁盘（自动创建目录）。
    pub fn save(&self) -> std::io::Result<()> {
        let path = Self::config_path();
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let content = toml::to_string_pretty(self).map_err(std::io::Error::other)?;
        std::fs::write(path, content)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_valid() {
        let c = Config::default();
        assert_eq!(c.refresh_secs, MIN_REFRESH_SECS);
        assert!(c.proxy_override.is_empty());
    }

    #[test]
    fn roundtrip_toml() {
        let c = Config::default();
        let s = toml::to_string_pretty(&c).unwrap();
        let back: Config = toml::from_str(&s).unwrap();
        assert_eq!(back.refresh_secs, c.refresh_secs);
        assert_eq!(back.proxy_override, c.proxy_override);
        assert_eq!(back.probe_url, c.probe_url);
    }

    /// 纯插件配置不得残留独立任务栏窗口的显示和开机自启字段。
    #[test]
    #[allow(non_snake_case)]
    fn serializedConfigContainsOnlyPluginSettings() {
        let serialized = toml::to_string_pretty(&Config::default()).unwrap();

        assert!(serialized.contains("proxy_override"));
        assert!(serialized.contains("refresh_secs"));
        assert!(serialized.contains("probe_url"));
        assert!(!serialized.contains("font_family"));
        assert!(!serialized.contains("background_color"));
        assert!(!serialized.contains("x_offset"));
        assert!(!serialized.contains("autostart"));
    }

    /// 旧版本默认的 5 秒配置必须自动迁移为最低 5 分钟，避免频繁请求免费接口。
    #[test]
    #[allow(non_snake_case)]
    fn refreshIntervalHasFiveMinuteMinimum() {
        assert_eq!(effectiveRefreshSecs(5), 300);
        assert_eq!(effectiveRefreshSecs(600), 600);
    }
}
