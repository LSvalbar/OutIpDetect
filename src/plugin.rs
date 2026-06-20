//! TrafficMonitor 插件实现。
//!
//! 把「落地IP」作为 TrafficMonitor 显示项，并在宿主提供的 GDI 绘图上下文中单行自绘。
//! 插件可与 CPU、内存等项目一起排序并共用 TrafficMonitor 的字体和颜色设置。
//!
//! # ABI 对齐说明（关键）
//! TrafficMonitor 的插件接口（`include/PluginInterface.h`，API version 7）是两个纯虚 C++ 类
//! `ITMPlugin` 与 `IPluginItem`。在 MSVC x64、单继承、无虚基类的情况下：
//! - 对象内存布局的**第一个字段**是指向虚表（vtable）的指针；
//! - 虚表是一组函数指针，**顺序严格等于头文件中虚函数的声明顺序**；
//! - 成员函数调用约定在 x64 上与 `extern "system"` 一致，`this` 作为隐藏的第一个参数传入。
//!
//! 因此我们用 `#[repr(C)]` 结构体手写两张虚表，逐条对应头文件里的每个虚函数（包括我们不使用、
//! 但必须占位以保持顺序的函数），把每个虚函数建模为
//! `unsafe extern "system" fn(this: *mut Obj, 其余参数...) -> 返回类型`。
//! 任何顺序/签名错位都会让 TM 调到错误的函数指针而崩溃，所以这里逐字对照了官方头文件。
//!
//! 头文件中各默认实现的虚函数我们仍需在虚表中列出（用安全的桩函数返回与默认实现一致的值）。

use std::os::raw::c_int;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, Once, OnceLock};

use windows::Win32::Foundation::{RECT, SIZE};
use windows::Win32::Graphics::Gdi::{
    CreateFontIndirectW, DeleteObject, DrawTextW, GetCurrentObject, GetObjectW,
    GetTextExtentPoint32W, SelectObject, SetBkMode, DT_END_ELLIPSIS, DT_LEFT, DT_NOPREFIX,
    DT_SINGLELINE, DT_VCENTER, HDC, HFONT, HGDIOBJ, LOGFONTW, OBJ_FONT, TRANSPARENT,
};

use crate::config::Config;
use crate::probe;
use crate::proxy;

// ============================================================================
// UTF-16 工具
// ============================================================================

/// 把 Rust 字符串转成以 0 结尾的 UTF-16（宽字符）缓冲。
/// 返回的指针交给 TM 后必须在调用结束后仍然有效，因此调用方需把缓冲存活在对象字段里。
fn to_utf16_nul(s: &str) -> Vec<u16> {
    let mut v: Vec<u16> = s.encode_utf16().collect();
    v.push(0); // 追加 NUL 结尾
    v
}

// ============================================================================
// 全局共享状态：后台探测线程写、显示项读
// ============================================================================

/// 最新的展示文本（形如 "JP  1.2.3.4"）。后台线程刷新，显示项读取。
static LATEST_TEXT: OnceLock<Mutex<String>> = OnceLock::new();
/// 后台探测线程是否已启动（保证只启动一次）。
static PROBE_STARTED: Once = Once::new();
/// 后台线程运行标志（当前生命周期内一直为真；预留给将来停止用）。
static RUNNING: AtomicBool = AtomicBool::new(true);

/// 取得 LATEST_TEXT 的单例。
fn latest_text() -> &'static Mutex<String> {
    LATEST_TEXT.get_or_init(|| Mutex::new(String::from("…")))
}

/// 读取当前展示文本（拷贝一份返回）。
fn read_latest_text() -> String {
    latest_text()
        .lock()
        .map(|g| g.clone())
        .unwrap_or_else(|_| "—".to_string())
}

/// 启动后台探测线程：循环「解析有效代理 + 探测一次」，把结果写入 LATEST_TEXT。
/// 刷新间隔读取配置（最低 5 分钟）。多次调用只会真正启动一次。
fn ensure_probe_thread() {
    PROBE_STARTED.call_once(|| {
        std::thread::spawn(|| {
            // 每轮都重新读取配置，便于用户改配置后无需重启 TM 即可生效。
            while RUNNING.load(Ordering::Relaxed) {
                let cfg = Config::load();
                // 手动代理覆盖：空字符串视为「不覆盖」。
                let manual = if cfg.proxy_override.trim().is_empty() {
                    None
                } else {
                    Some(cfg.proxy_override.clone())
                };
                let eff = proxy::resolve_effective_proxy(manual.as_deref());

                // 单个接口超时 5 秒，失败时由探测模块自动切换备用地址。
                let timeout = std::time::Duration::from_secs(5);
                match probe::probeWithFallback(&eff, Some(&cfg.probe_url), timeout) {
                    Ok(info) => {
                        if let Ok(mut g) = latest_text().lock() {
                            *g = info.display_text();
                        }
                    }
                    Err(e) => {
                        if let Ok(mut g) = latest_text().lock() {
                            // 探测失败时给出简短提示，避免界面空白。
                            *g = format!("探测失败: {e}");
                        }
                    }
                }

                // 免费接口最低每 5 分钟刷新一次，避免旧配置造成高频请求。
                let secs = crate::config::effectiveRefreshSecs(cfg.refresh_secs);
                std::thread::sleep(std::time::Duration::from_secs(secs));
            }
        });
    });
}

// ============================================================================
// IPluginItem 虚表（严格对照 PluginInterface.h 中 IPluginItem 的声明顺序）
// ============================================================================
//
// 声明顺序（API v7）：
//  0 GetItemName            const -> const wchar_t*
//  1 GetItemId              const -> const wchar_t*
//  2 GetItemLableText       const -> const wchar_t*
//  3 GetItemValueText       const -> const wchar_t*
//  4 GetItemValueSampleText const -> const wchar_t*
//  5 IsCustomDraw           const -> bool
//  6 GetItemWidth           const -> int
//  7 DrawItem               (void*hDC,int x,int y,int w,int h,bool dark) -> void
//  8 GetItemWidthEx         (void*hDC) const -> int
//  9 OnMouseEvent           (MouseEventType,int x,int y,void*hWnd,int flag) -> int
// 10 OnKeboardEvent         (int key,bool ctrl,bool shift,bool alt,void*hWnd,int flag) -> int
// 11 OnItemInfo             (ItemInfoType,void*p1,void*p2) -> void*
// 12 IsDrawResourceUsageGraph const -> int
// 13 GetResourceUsageGraphValue const -> float

/// IPluginItem 的虚表布局。每个字段都是 `extern "system"` 函数指针，
/// 顺序必须与头文件逐条一致。
#[repr(C)]
struct IPluginItemVtbl {
    get_item_name: unsafe extern "system" fn(*mut PluginItem) -> *const u16,
    get_item_id: unsafe extern "system" fn(*mut PluginItem) -> *const u16,
    get_item_lable_text: unsafe extern "system" fn(*mut PluginItem) -> *const u16,
    get_item_value_text: unsafe extern "system" fn(*mut PluginItem) -> *const u16,
    get_item_value_sample_text: unsafe extern "system" fn(*mut PluginItem) -> *const u16,
    is_custom_draw: unsafe extern "system" fn(*mut PluginItem) -> bool,
    get_item_width: unsafe extern "system" fn(*mut PluginItem) -> c_int,
    draw_item: unsafe extern "system" fn(
        *mut PluginItem,
        *mut core::ffi::c_void,
        c_int,
        c_int,
        c_int,
        c_int,
        bool,
    ),
    get_item_width_ex: unsafe extern "system" fn(*mut PluginItem, *mut core::ffi::c_void) -> c_int,
    on_mouse_event: unsafe extern "system" fn(
        *mut PluginItem,
        c_int,
        c_int,
        c_int,
        *mut core::ffi::c_void,
        c_int,
    ) -> c_int,
    on_keboard_event: unsafe extern "system" fn(
        *mut PluginItem,
        c_int,
        bool,
        bool,
        bool,
        *mut core::ffi::c_void,
        c_int,
    ) -> c_int,
    on_item_info: unsafe extern "system" fn(
        *mut PluginItem,
        c_int,
        *mut core::ffi::c_void,
        *mut core::ffi::c_void,
    ) -> *mut core::ffi::c_void,
    is_draw_resource_usage_graph: unsafe extern "system" fn(*mut PluginItem) -> c_int,
    get_resource_usage_graph_value: unsafe extern "system" fn(*mut PluginItem) -> f32,
}

/// 「落地IP」显示项对象。
/// `#[repr(C)]` 且 vtbl 指针为**第一个字段**，以匹配 C++ 对象布局。
#[repr(C)]
struct PluginItem {
    /// 指向静态虚表的指针（C++ 对象的首字段）。
    vtbl: *const IPluginItemVtbl,
    /// 显示项名称缓存（UTF-16, NUL 结尾）。
    name_w: Vec<u16>,
    /// 唯一 ID 缓存。
    id_w: Vec<u16>,
    /// 标签文本缓存（如 "落地IP"）。
    label_w: Vec<u16>,
    /// 数值示例文本缓存（用于 TM 计算列宽）。
    sample_w: Vec<u16>,
    /// 数值文本缓存：每次 DataRequired 时用最新落地IP刷新，GetItemValueText 返回其指针。
    value_w: Vec<u16>,
}

// 落地IP的固定文案（中文标签 + 英文唯一ID）。
const ITEM_NAME: &str = "落地IP";
const ITEM_ID: &str = "ipdetect.landing_ip";
// 自绘模式下宿主标签必须为空，避免 TrafficMonitor 再次绘制标签。
const ITEM_LABEL: &str = "";
// 自绘区域按“左侧标签 + 右侧地域及 IP”的最长 IPv4 样例估算宽度。
const ITEM_SAMPLE: &str = "出口IP  JP  255.255.255.255";
// 96 DPI 下的兜底宽度；TrafficMonitor 会按当前 DPI 自动缩放。
const ITEM_FALLBACK_WIDTH: c_int = 190;
// 文本左右内边距，避免内容紧贴项目边界。
const ITEM_HORIZONTAL_PADDING: c_int = 4;

/// 清除宿主字体的旋转角度，确保中文和英文都按正常水平方向绘制。
#[allow(non_snake_case)]
fn normalizeFontOrientation(font: &mut LOGFONTW) {
    font.lfEscapement = 0;
    font.lfOrientation = 0;

    // Windows 使用“@字体名”表示竖排字体。仅清除角度不会恢复中文横排，
    // 因此还必须移除 @ 前缀，让 CreateFontIndirectW 选择普通横排字体。
    if font.lfFaceName.first().copied() == Some('@' as u16) {
        font.lfFaceName.copy_within(1.., 0);
        if let Some(last_character) = font.lfFaceName.last_mut() {
            *last_character = 0;
        }
    }
}

/// 基于 TrafficMonitor 当前字体创建一个方向固定为 0° 的临时字体。
#[allow(non_snake_case)]
unsafe fn createHorizontalFont(device_context: HDC) -> Option<HFONT> {
    let current_font = GetCurrentObject(device_context, OBJ_FONT);
    if current_font.0.is_null() {
        return None;
    }

    let mut font = LOGFONTW::default();
    let copied_size = GetObjectW(
        current_font,
        std::mem::size_of::<LOGFONTW>() as i32,
        Some((&mut font as *mut LOGFONTW).cast()),
    );
    if copied_size == 0 {
        return None;
    }

    normalizeFontOrientation(&mut font);
    let horizontal_font = CreateFontIndirectW(&font);
    if horizontal_font.0.is_null() {
        None
    } else {
        Some(horizontal_font)
    }
}

/// 选中临时水平字体，并返回字体及宿主原字体句柄用于恢复。
#[allow(non_snake_case)]
unsafe fn selectHorizontalFont(device_context: HDC) -> Option<(HFONT, HGDIOBJ)> {
    let horizontal_font = createHorizontalFont(device_context)?;
    let previous_font = SelectObject(device_context, horizontal_font);
    if previous_font.0.is_null() {
        let _ = DeleteObject(horizontal_font);
        None
    } else {
        Some((horizontal_font, previous_font))
    }
}

impl PluginItem {
    fn new() -> Self {
        PluginItem {
            vtbl: &ITEM_VTBL,
            name_w: to_utf16_nul(ITEM_NAME),
            id_w: to_utf16_nul(ITEM_ID),
            label_w: to_utf16_nul(ITEM_LABEL),
            sample_w: to_utf16_nul(ITEM_SAMPLE),
            // 首次探测完成前也按最终单行布局显示占位内容。
            value_w: to_utf16_nul("出口IP  …"),
        }
    }

    /// 用最新展示文本刷新数值缓存（由 ITMPlugin::DataRequired 调用）。
    fn refresh_value(&mut self) {
        let text = read_latest_text();
        self.value_w = to_utf16_nul(&format!("出口IP  {text}"));
    }
}

// ---- IPluginItem 各虚函数实现 ----

unsafe extern "system" fn item_get_name(this: *mut PluginItem) -> *const u16 {
    (*this).name_w.as_ptr()
}
unsafe extern "system" fn item_get_id(this: *mut PluginItem) -> *const u16 {
    (*this).id_w.as_ptr()
}
unsafe extern "system" fn item_get_lable_text(this: *mut PluginItem) -> *const u16 {
    (*this).label_w.as_ptr()
}
unsafe extern "system" fn item_get_value_text(this: *mut PluginItem) -> *const u16 {
    // 返回缓存指针；内容由 DataRequired -> refresh_value 维护，保证调用后仍有效。
    (*this).value_w.as_ptr()
}
unsafe extern "system" fn item_get_value_sample_text(this: *mut PluginItem) -> *const u16 {
    (*this).sample_w.as_ptr()
}
unsafe extern "system" fn item_is_custom_draw(_this: *mut PluginItem) -> bool {
    // 返回 true：由插件强制按单行横向布局绘制标签和值。
    true
}
unsafe extern "system" fn item_get_width(_this: *mut PluginItem) -> c_int {
    // HDC 不可用时返回足以容纳最长 IPv4 样例的固定宽度。
    ITEM_FALLBACK_WIDTH
}
unsafe extern "system" fn item_draw_item(
    this: *mut PluginItem,
    hdc: *mut core::ffi::c_void,
    x: c_int,
    y: c_int,
    w: c_int,
    h: c_int,
    _dark: bool,
) {
    // 宿主没有提供有效绘图上下文或区域时不执行绘制。
    if this.is_null() || hdc.is_null() || w <= 0 || h <= 0 {
        return;
    }

    // 去掉缓存末尾的 NUL，并复制成 DrawTextW 所需的可变 UTF-16 缓冲区。
    let value_w = &(*this).value_w;
    let text_len = value_w.len().saturating_sub(1);
    let mut text_w = value_w[..text_len].to_vec();

    // 继承宿主字体的字号和字体族，但强制把字体方向恢复为正常的 0°。
    let device_context = HDC(hdc);
    let selected_font = selectHorizontalFont(device_context);
    let previous_background_mode = SetBkMode(device_context, TRANSPARENT);
    let mut text_rect = RECT {
        left: x + ITEM_HORIZONTAL_PADDING,
        top: y,
        right: x + w - ITEM_HORIZONTAL_PADDING,
        bottom: y + h,
    };
    let draw_format = DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX | DT_END_ELLIPSIS;
    let _ = DrawTextW(device_context, &mut text_w, &mut text_rect, draw_format);

    // 恢复宿主原有的背景模式，避免影响后续项目绘制。
    if previous_background_mode != 0 {
        let _ = SetBkMode(
            device_context,
            windows::Win32::Graphics::Gdi::BACKGROUND_MODE(previous_background_mode as u32),
        );
    }
    // 先恢复宿主字体，再释放临时字体，避免 GDI 对象仍被 HDC 占用。
    if let Some((horizontal_font, previous_font)) = selected_font {
        let _ = SelectObject(device_context, previous_font);
        let _ = DeleteObject(horizontal_font);
    }
}
unsafe extern "system" fn item_get_width_ex(
    this: *mut PluginItem,
    hdc: *mut core::ffi::c_void,
) -> c_int {
    // HDC 或对象不可用时退回固定宽度。
    if this.is_null() || hdc.is_null() {
        return ITEM_FALLBACK_WIDTH;
    }

    // 使用与实际绘制相同的水平字体测量，避免旋转字体导致宽度计算异常。
    let device_context = HDC(hdc);
    let selected_font = selectHorizontalFont(device_context);
    let sample_w = &(*this).sample_w;
    let sample_len = sample_w.len().saturating_sub(1);
    let mut size = SIZE::default();
    let measured =
        GetTextExtentPoint32W(device_context, &sample_w[..sample_len], &mut size).as_bool();

    // 测量完成后恢复并释放临时字体，不影响宿主继续绘制其他项目。
    if let Some((horizontal_font, previous_font)) = selected_font {
        let _ = SelectObject(device_context, previous_font);
        let _ = DeleteObject(horizontal_font);
    }

    if measured {
        size.cx + ITEM_HORIZONTAL_PADDING * 2
    } else {
        ITEM_FALLBACK_WIDTH
    }
}
unsafe extern "system" fn item_on_mouse_event(
    _this: *mut PluginItem,
    _type: c_int,
    _x: c_int,
    _y: c_int,
    _hwnd: *mut core::ffi::c_void,
    _flag: c_int,
) -> c_int {
    0 // 返回 0：让 TM 继续做默认处理（如右键菜单）
}
unsafe extern "system" fn item_on_keboard_event(
    _this: *mut PluginItem,
    _key: c_int,
    _ctrl: bool,
    _shift: bool,
    _alt: bool,
    _hwnd: *mut core::ffi::c_void,
    _flag: c_int,
) -> c_int {
    0
}
unsafe extern "system" fn item_on_item_info(
    _this: *mut PluginItem,
    _type: c_int,
    _p1: *mut core::ffi::c_void,
    _p2: *mut core::ffi::c_void,
) -> *mut core::ffi::c_void {
    core::ptr::null_mut()
}
unsafe extern "system" fn item_is_draw_resource_usage_graph(_this: *mut PluginItem) -> c_int {
    0 // 不绘制资源占用图
}
unsafe extern "system" fn item_get_resource_usage_graph_value(_this: *mut PluginItem) -> f32 {
    0.0
}

/// 静态 IPluginItem 虚表实例，函数指针顺序严格对应头文件。
static ITEM_VTBL: IPluginItemVtbl = IPluginItemVtbl {
    get_item_name: item_get_name,
    get_item_id: item_get_id,
    get_item_lable_text: item_get_lable_text,
    get_item_value_text: item_get_value_text,
    get_item_value_sample_text: item_get_value_sample_text,
    is_custom_draw: item_is_custom_draw,
    get_item_width: item_get_width,
    draw_item: item_draw_item,
    get_item_width_ex: item_get_width_ex,
    on_mouse_event: item_on_mouse_event,
    on_keboard_event: item_on_keboard_event,
    on_item_info: item_on_item_info,
    is_draw_resource_usage_graph: item_is_draw_resource_usage_graph,
    get_resource_usage_graph_value: item_get_resource_usage_graph_value,
};

// ============================================================================
// ITMPlugin 虚表（严格对照 PluginInterface.h 中 ITMPlugin 的声明顺序）
// ============================================================================
//
// 声明顺序（API v7）：
//  0 GetAPIVersion      const -> int            （必须返回 7）
//  1 GetItem            (int index) -> IPluginItem*
//  2 DataRequired       () -> void
//  3 ShowOptionsDialog  (void* hParent) -> OptionReturn(int)
//  4 GetInfo            (PluginInfoIndex) -> const wchar_t*
//  5 OnMonitorInfo      (const MonitorInfo&) -> void
//  6 GetTooltipInfo     () -> const wchar_t*
//  7 OnExtenedInfo      (ExtendedInfoIndex,const wchar_t*) -> void
//  8 GetPluginIcon      () -> void*
//  9 GetCommandCount    () -> int
// 10 GetCommandName     (int) -> const wchar_t*
// 11 GetCommandIcon     (int) -> void*
// 12 OnPluginCommand    (int,void*,void*) -> void
// 13 IsCommandChecked   (int) -> int
// 14 OnInitialize       (ITrafficMonitor*) -> void
//
// 注意：GetAPIVersion 在头文件里有 `const` 限定，其余无 const 的成员函数在 x64 ABI 下
// 调用约定一致（this 同样作为首参传入），const 不改变内存布局或调用约定。

/// ITMPlugin 的虚表布局。
#[repr(C)]
struct ITMPluginVtbl {
    get_api_version: unsafe extern "system" fn(*mut Plugin) -> c_int,
    get_item: unsafe extern "system" fn(*mut Plugin, c_int) -> *mut PluginItem,
    data_required: unsafe extern "system" fn(*mut Plugin),
    show_options_dialog: unsafe extern "system" fn(*mut Plugin, *mut core::ffi::c_void) -> c_int,
    get_info: unsafe extern "system" fn(*mut Plugin, c_int) -> *const u16,
    on_monitor_info: unsafe extern "system" fn(*mut Plugin, *const core::ffi::c_void),
    get_tooltip_info: unsafe extern "system" fn(*mut Plugin) -> *const u16,
    on_extened_info: unsafe extern "system" fn(*mut Plugin, c_int, *const u16),
    get_plugin_icon: unsafe extern "system" fn(*mut Plugin) -> *mut core::ffi::c_void,
    get_command_count: unsafe extern "system" fn(*mut Plugin) -> c_int,
    get_command_name: unsafe extern "system" fn(*mut Plugin, c_int) -> *const u16,
    get_command_icon: unsafe extern "system" fn(*mut Plugin, c_int) -> *mut core::ffi::c_void,
    on_plugin_command: unsafe extern "system" fn(
        *mut Plugin,
        c_int,
        *mut core::ffi::c_void,
        *mut core::ffi::c_void,
    ),
    is_command_checked: unsafe extern "system" fn(*mut Plugin, c_int) -> c_int,
    on_initialize: unsafe extern "system" fn(*mut Plugin, *mut core::ffi::c_void),
}

/// 插件主对象（实现 ITMPlugin）。vtbl 为首字段以匹配 C++ 布局。
#[repr(C)]
struct Plugin {
    /// 指向静态虚表的指针。
    vtbl: *const ITMPluginVtbl,
    /// 唯一的显示项（落地IP）。
    item: PluginItem,
    /// 各 GetInfo 字段的 UTF-16 缓存，按 PluginInfoIndex 顺序排列。
    info_w: [Vec<u16>; 6],
}

// PluginInfoIndex 取值（与头文件一致）。
const TMI_NAME: c_int = 0;
const TMI_DESCRIPTION: c_int = 1;
const TMI_AUTHOR: c_int = 2;
const TMI_COPYRIGHT: c_int = 3;
const TMI_VERSION: c_int = 4;
const TMI_URL: c_int = 5;

impl Plugin {
    fn new() -> Self {
        Plugin {
            vtbl: &PLUGIN_VTBL,
            item: PluginItem::new(),
            info_w: [
                to_utf16_nul("OutIpDetect"),                             // TMI_NAME
                to_utf16_nul("显示当前经代理出口的IP与国家/地区代码"),   // TMI_DESCRIPTION
                to_utf16_nul("LSvalbar"),                                // TMI_AUTHOR
                to_utf16_nul("Copyright (C) 2026 LSvalbar"),             // TMI_COPYRIGHT
                to_utf16_nul("0.1.0"),                                   // TMI_VERSION
                to_utf16_nul("https://github.com/LSvalbar/OutIpDetect"), // TMI_URL
            ],
        }
    }
}

// ---- ITMPlugin 各虚函数实现 ----

unsafe extern "system" fn plugin_get_api_version(_this: *mut Plugin) -> c_int {
    7 // 必须与头文件默认实现一致
}
unsafe extern "system" fn plugin_get_item(this: *mut Plugin, index: c_int) -> *mut PluginItem {
    // 仅提供 1 个显示项：index==0 返回它，其它返回空指针（头文件要求越界返回 nullptr）。
    if index == 0 {
        &mut (*this).item as *mut PluginItem
    } else {
        core::ptr::null_mut()
    }
}
unsafe extern "system" fn plugin_data_required(this: *mut Plugin) {
    // 主程序定时调用：此处确保后台线程已启动，并把最新文本刷入显示项缓存。
    ensure_probe_thread();
    (*this).item.refresh_value();
}
unsafe extern "system" fn plugin_show_options_dialog(
    _this: *mut Plugin,
    _hparent: *mut core::ffi::c_void,
) -> c_int {
    // 2 == OR_OPTION_NOT_PROVIDED：未提供选项对话框（配置通过 config.toml 修改）。
    2
}
unsafe extern "system" fn plugin_get_info(this: *mut Plugin, index: c_int) -> *const u16 {
    match index {
        TMI_NAME | TMI_DESCRIPTION | TMI_AUTHOR | TMI_COPYRIGHT | TMI_VERSION | TMI_URL => {
            (*this).info_w[index as usize].as_ptr()
        }
        _ => core::ptr::null(), // 含 TMI_MAX 等越界值返回空指针
    }
}
unsafe extern "system" fn plugin_on_monitor_info(
    _this: *mut Plugin,
    _info: *const core::ffi::c_void,
) {
    // 不需要主程序的 CPU/内存等监控数据。
}
unsafe extern "system" fn plugin_get_tooltip_info(_this: *mut Plugin) -> *const u16 {
    EMPTY_W.as_ptr() // 返回空字符串（与头文件默认 L"" 一致）
}
unsafe extern "system" fn plugin_on_extened_info(
    _this: *mut Plugin,
    _index: c_int,
    _data: *const u16,
) {
    // 暂不处理主程序传来的扩展信息（如配置目录、颜色等）。
}
unsafe extern "system" fn plugin_get_plugin_icon(_this: *mut Plugin) -> *mut core::ffi::c_void {
    core::ptr::null_mut() // 不提供图标
}
unsafe extern "system" fn plugin_get_command_count(_this: *mut Plugin) -> c_int {
    0
}
unsafe extern "system" fn plugin_get_command_name(_this: *mut Plugin, _i: c_int) -> *const u16 {
    core::ptr::null()
}
unsafe extern "system" fn plugin_get_command_icon(
    _this: *mut Plugin,
    _i: c_int,
) -> *mut core::ffi::c_void {
    core::ptr::null_mut()
}
unsafe extern "system" fn plugin_on_plugin_command(
    _this: *mut Plugin,
    _i: c_int,
    _hwnd: *mut core::ffi::c_void,
    _para: *mut core::ffi::c_void,
) {
}
unsafe extern "system" fn plugin_is_command_checked(_this: *mut Plugin, _i: c_int) -> c_int {
    0
}
unsafe extern "system" fn plugin_on_initialize(_this: *mut Plugin, _app: *mut core::ffi::c_void) {
    // 插件加载时被调用：在此启动后台探测线程，尽早开始拿落地IP。
    ensure_probe_thread();
}

/// 静态 ITMPlugin 虚表实例，函数指针顺序严格对应头文件。
static PLUGIN_VTBL: ITMPluginVtbl = ITMPluginVtbl {
    get_api_version: plugin_get_api_version,
    get_item: plugin_get_item,
    data_required: plugin_data_required,
    show_options_dialog: plugin_show_options_dialog,
    get_info: plugin_get_info,
    on_monitor_info: plugin_on_monitor_info,
    get_tooltip_info: plugin_get_tooltip_info,
    on_extened_info: plugin_on_extened_info,
    get_plugin_icon: plugin_get_plugin_icon,
    get_command_count: plugin_get_command_count,
    get_command_name: plugin_get_command_name,
    get_command_icon: plugin_get_command_icon,
    on_plugin_command: plugin_on_plugin_command,
    is_command_checked: plugin_is_command_checked,
    on_initialize: plugin_on_initialize,
};

// 空宽字符串常量（供 GetTooltipInfo 返回）。
static EMPTY_W: [u16; 1] = [0];

// ============================================================================
// 单例与导出工厂函数
// ============================================================================

/// 对 `Plugin` 的稳定地址做线程安全包装。
///
/// `Plugin` 内含原始指针（`*const Vtbl`），因此默认既非 `Send` 也非 `Sync`，无法直接放进
/// `static`。但实际并发模型是安全的：
/// - 跨线程真正共享的可变状态只有 `LATEST_TEXT`（已用 `Mutex` 保护）；
/// - `Plugin`/`PluginItem` 自身的可变操作（`refresh_value`）只发生在 TM 调用 `DataRequired`
///   的线程内，TM 不会并发地对同一对象调用这些方法。
///
/// 因此这里手动断言 `Send + Sync` 是合理的。包装持有 `Box<Plugin>` 的堆地址，
/// 该地址在进程生命周期内保持不变且不会被释放。
struct SyncPlugin(*mut Plugin);
// 安全性见上方说明：共享可变状态已由 Mutex 保护，对象本身仅被 TM 单线程改写。
unsafe impl Send for SyncPlugin {}
unsafe impl Sync for SyncPlugin {}

/// 全局唯一的插件对象。头文件要求 TMPluginGetInstance 返回的对象在程序结束前不被释放，
/// 因此用 OnceLock 持有一个进程级单例（不会被 drop），并固定其地址。
static PLUGIN_INSTANCE: OnceLock<SyncPlugin> = OnceLock::new();

/// 导出给 TrafficMonitor 的工厂函数。
///
/// TM 通过 `GetProcAddress(hModule, "TMPluginGetInstance")` 取得此函数并调用，
/// 期望返回一个 `ITMPlugin*`（即指向以 vtbl 指针开头的对象的指针）。
/// 由于 `Plugin` 的首字段就是 `*const ITMPluginVtbl`，其地址即可当作 `ITMPlugin*` 使用。
///
/// `#[no_mangle]` 保证导出符号名不被修饰；`extern "system"` 与 C++ 调用约定一致。
#[no_mangle]
pub extern "system" fn TMPluginGetInstance() -> *mut core::ffi::c_void {
    let inst = PLUGIN_INSTANCE.get_or_init(|| {
        // 把 Plugin 放到堆上并泄漏出稳定地址，确保进程结束前都不被释放。
        let boxed = Box::new(Plugin::new());
        SyncPlugin(Box::into_raw(boxed))
    });
    // inst.0 即 Plugin 的堆地址；其首字段为 vtbl 指针，可直接当作 ITMPlugin* 返回。
    inst.0 as *mut core::ffi::c_void
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 插件项必须由自身绘制，避免 TrafficMonitor 把标签和值拆成上下两行。
    #[test]
    #[allow(non_snake_case)]
    fn pluginItemUsesCustomSingleLineDrawing() {
        assert!(unsafe { item_is_custom_draw(core::ptr::null_mut()) });
        assert_eq!(ITEM_LABEL, "");
        assert_eq!(ITEM_SAMPLE, "出口IP  JP  255.255.255.255");

        // 首次探测完成前也必须保留左侧标签，避免加载瞬间布局不一致。
        let item = PluginItem::new();
        let initial_text = String::from_utf16(&item.value_w[..item.value_w.len() - 1]).unwrap();
        assert_eq!(initial_text, "出口IP  …");
    }

    /// 自绘字体必须强制恢复为正常水平书写方向。
    #[test]
    #[allow(non_snake_case)]
    fn horizontalFontClearsRotation() {
        let mut font = windows::Win32::Graphics::Gdi::LOGFONTW {
            lfEscapement: 900,
            lfOrientation: 900,
            ..Default::default()
        };

        normalizeFontOrientation(&mut font);

        assert_eq!(font.lfEscapement, 0);
        assert_eq!(font.lfOrientation, 0);
    }

    /// Windows 竖排字体使用 @ 前缀；必须移除它，否则中文会横倒而英文保持正常。
    #[test]
    #[allow(non_snake_case)]
    fn horizontalFontRemovesVerticalFacePrefix() {
        let mut font = windows::Win32::Graphics::Gdi::LOGFONTW::default();
        let vertical_face: Vec<u16> = "@Microsoft YaHei UI".encode_utf16().collect();
        font.lfFaceName[..vertical_face.len()].copy_from_slice(&vertical_face);

        normalizeFontOrientation(&mut font);

        let face_length = font
            .lfFaceName
            .iter()
            .position(|character| *character == 0)
            .unwrap_or(font.lfFaceName.len());
        let normalized_face = String::from_utf16(&font.lfFaceName[..face_length]).unwrap();
        assert_eq!(normalized_face, "Microsoft YaHei UI");
    }
}
