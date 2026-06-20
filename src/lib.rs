//! OutIpDetect TrafficMonitor 插件 crate 根。
//!
//! 项目只生成 `ipdetect.dll`，作为 TrafficMonitor API v7 插件加载。
//! 插件负责读取代理配置、探测出口 IP，并在 TrafficMonitor 任务栏窗口中绘制结果。

// 探测出口 IP，并在多个 HTTPS 接口之间自动故障转移。
pub mod probe;
// 解析有效代理（手动覆盖 / 系统代理 / 直连）。
pub mod proxy;
// 配置读写（刷新间隔、代理覆盖和自定义探测端点）。
pub mod config;
// TrafficMonitor 插件：手写 C++ 兼容虚表，导出 TMPluginGetInstance。
pub mod plugin;
