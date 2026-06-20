//! 配置模块。
//!
//! 负责读取 `%APPDATA%\ipDetect\config.toml` 中的插件探测设置。

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// 应用配置。`#[serde(default)]` 确保旧配置缺字段时回退到默认值；
/// serde 默认忽略未知字段，因此旧版本里的 `refresh_secs` 仍能被正常解析（自动忽略）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// 手动代理覆盖（留空 = 自动：系统代理 / 直连）。
    pub proxy_override: String,
    /// 自定义探测端点 URL（留空 = 使用内置多接口故障转移列表）。
    pub probe_url: String,
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
        assert!(c.proxy_override.is_empty());
        assert!(c.probe_url.is_empty());
    }

    #[test]
    fn roundtrip_toml() {
        let c = Config::default();
        let s = toml::to_string_pretty(&c).unwrap();
        let back: Config = toml::from_str(&s).unwrap();
        assert_eq!(back.proxy_override, c.proxy_override);
        assert_eq!(back.probe_url, c.probe_url);
    }

    /// 纯插件配置只保留代理与探测端点，且不得残留定时刷新或独立窗口字段。
    #[test]
    #[allow(non_snake_case)]
    fn serializedConfigContainsOnlyPluginSettings() {
        let serialized = toml::to_string_pretty(&Config::default()).unwrap();

        assert!(serialized.contains("proxy_override"));
        assert!(serialized.contains("probe_url"));
        assert!(!serialized.contains("refresh_secs"));
        assert!(!serialized.contains("font_family"));
        assert!(!serialized.contains("autostart"));
    }

    /// 旧版本配置中的 refresh_secs 字段已废弃，但必须仍能被忽略而不报错（向后兼容）。
    #[test]
    #[allow(non_snake_case)]
    fn legacyRefreshSecsFieldIsIgnored() {
        let legacy = "proxy_override = \"\"\nrefresh_secs = 5\nprobe_url = \"\"\n";
        let cfg: Config = toml::from_str(legacy).unwrap();
        assert!(cfg.proxy_override.is_empty());
        assert!(cfg.probe_url.is_empty());
    }
}
