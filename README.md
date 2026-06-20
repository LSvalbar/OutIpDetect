# OutIpDetect

[![Platform](https://img.shields.io/badge/platform-Windows-0078D6?logo=windows)](https://www.microsoft.com/windows/)
[![TrafficMonitor](https://img.shields.io/badge/TrafficMonitor-plugin-39E66B)](https://github.com/zhongyang219/TrafficMonitor)
[![License](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

OutIpDetect 是一个适用于 [TrafficMonitor](https://github.com/zhongyang219/TrafficMonitor) 的 Windows 插件，用于在任务栏中显示当前网络连接实际使用的出口 IP（落地 IP）及国家/地区代码。

显示示例：

```text
出口IP  JP  43.251.186.2
```

## 项目背景

使用 Clash、sing-box、V2Ray 等代理工具时，本机网卡地址和普通局域网信息无法反映代理链路最终使用的公网出口。对于经常切换代理节点、规则或链式代理的用户，快速确认当前出口地区和 IP 是一个常见需求。

OutIpDetect 通过代理链路访问外部 IP 查询服务，并把结果作为 TrafficMonitor 显示项绘制到任务栏。项目不再创建独立任务栏窗口，也不包含托盘程序或常驻 EXE；显示位置、主题和任务栏集成均由 TrafficMonitor 管理。

## 功能

- 在 TrafficMonitor 任务栏窗口中显示国家/地区代码和出口 IP。
- 自动读取 Windows 系统代理。
- 支持 Clash 等工具的系统代理模式。
- 未设置系统代理时支持 TUN/透明代理环境下的直连探测。
- 支持在配置文件中手动指定 HTTP、HTTPS 或 SOCKS 代理。
- 内置多个 HTTPS 查询接口，当前接口失败时自动切换备用接口。
- 默认每 5 分钟刷新一次，避免频繁请求免费 API。
- 使用原生 Windows SChannel 处理 HTTPS，不依赖 OpenSSL。
- 使用 TrafficMonitor 当前字体和颜色进行单行自绘。

## 系统要求

- Windows 10 或 Windows 11（x64）。
- [TrafficMonitor](https://github.com/zhongyang219/TrafficMonitor)。
- TrafficMonitor 插件 API v7。

从源码构建还需要：

- Rust stable，目标工具链为 `x86_64-pc-windows-msvc`。
- Visual Studio Build Tools 2022，并安装“使用 C++ 的桌面开发”组件。

## 安装

### 使用预编译 DLL

1. 从本仓库的 Releases 页面下载 `ipdetect.dll`。
2. 退出 TrafficMonitor。
3. 将 DLL 复制到 TrafficMonitor 的插件目录：

   ```text
   TrafficMonitor\plugins\ipdetect.dll
   ```

   常见安装路径：

   ```text
   C:\Program Files\TrafficMonitor\plugins\ipdetect.dll
   ```

4. 重新启动 TrafficMonitor。
5. 打开 TrafficMonitor 设置，在任务栏显示项目中启用“落地IP”插件项。
6. 按需要调整显示顺序、字体和颜色。

更新插件时，应先完全退出 TrafficMonitor，再覆盖旧 DLL。

### 从源码构建

```powershell
git clone https://github.com/LSvalbar/OutIpDetect.git
cd OutIpDetect
cargo test
cargo build --release
```

构建产物：

```text
target\release\ipdetect.dll
```

将该文件复制到 TrafficMonitor 的 `plugins` 目录并重启 TrafficMonitor。

## 配置

配置文件位置：

```text
%APPDATA%\ipDetect\config.toml
```

配置文件不是必需的。文件不存在或内容无效时，插件使用默认配置。

示例：

```toml
# 留空时自动选择：Windows 系统代理 > 直连/TUN。
proxy_override = ""

# 刷新间隔，单位为秒；最低有效值为 300 秒。
refresh_secs = 300

# 留空时使用内置的多接口故障转移列表。
# 设置后会优先请求该地址，失败时仍会尝试内置接口。
probe_url = ""
```

手动代理示例：

```toml
proxy_override = "http://127.0.0.1:7897"
```

或：

```toml
proxy_override = "socks5://127.0.0.1:7898"
```

插件每轮刷新时重新读取配置，因此修改配置后通常不需要重启 TrafficMonitor；最迟在下一次刷新时生效。

## 出口 IP 探测

插件每轮仅请求一个可用接口。请求失败、超时或响应无法解析时，才会切换到下一个备用接口。

当前内置接口：

- `https://api.ip.sb/geoip/`
- `https://api.ipquery.io/?format=json`
- `https://api.myip.com/`
- `https://api.db-ip.com/v2/free/self`
- `https://free.freeipapi.com/api/json`
- `https://ipapi.co/json/`
- `https://1.1.1.1/cdn-cgi/trace`
- `https://www.cloudflare.com/cdn-cgi/trace`

接口列表参考了 [ihmily/ip-info-api](https://github.com/ihmily/ip-info-api)。第三方免费服务的可用性、限流和隐私政策由各服务提供方负责。

## 工作原理

```text
TrafficMonitor
    │
    ├─ 加载 ipdetect.dll
    │
    ├─ 插件后台线程读取配置和 Windows 系统代理
    │
    ├─ 经当前代理链路请求出口 IP API
    │
    └─ 插件在 TrafficMonitor 提供的 HDC 中绘制：
       出口IP  国家/地区代码  IP 地址
```

代理选择优先级：

1. `config.toml` 中的 `proxy_override`。
2. Windows 注册表中的系统代理。
3. 直连请求，适用于 TUN 或透明代理模式。

## 隐私与安全

- 插件不会收集、存储或上传密码、API 密钥、浏览记录等敏感数据。
- 每次探测会向一个第三方 IP 查询服务发送普通 HTTPS 请求；该服务会看到当前出口 IP。
- 项目不需要管理员权限运行。只有向 `Program Files` 中复制 DLL 时可能需要管理员权限。
- 不要把包含私人代理账号、密码或令牌的配置文件提交到公开仓库。

## 已知限制

- 仅支持 Windows 和 TrafficMonitor。
- 免费 IP 查询接口可能临时不可用或限制请求频率。
- 国家/地区信息来自第三方接口，可能存在延迟或识别误差。
- 插件当前只显示两位国家/地区代码，不显示城市、ISP 或真实国旗 Emoji。

## 开发与测试

```powershell
# 运行测试
cargo test

# 检查格式
cargo fmt -- --check

# 运行 Clippy
cargo clippy --all-targets -- -D warnings

# 构建发布 DLL
cargo build --release
```

核心模块：

```text
src/
├─ lib.rs       # DLL crate 入口
├─ plugin.rs    # TrafficMonitor API v7 接口和 GDI 自绘
├─ probe.rs     # 多接口出口 IP 探测和响应解析
├─ proxy.rs     # 系统代理、手动代理和 TUN/直连选择
└─ config.rs    # TOML 配置读取
```

## 贡献

欢迎提交 Issue 和 Pull Request。

提交代码前请确认：

1. `cargo fmt -- --check` 通过。
2. `cargo test` 通过。
3. `cargo clippy --all-targets -- -D warnings` 通过。
4. 没有提交密码、代理凭据、API 密钥或其他敏感数据。

## 许可证

本项目使用 [MIT License](LICENSE)。
