//! 把 `nfc_core::theme::Theme`（39 个 `--color-*` token 里的 34 个）桥接成 egui `Visuals`。
//!
//! 原版用 CSS 变量驱动，egui 没有等价物，故手工映射。刻意不调
//! `Visuals::dark()`/`light()` 的全量覆盖，只改能被 CSS 变量直接对应的项，
//! 其余沿用 egui 默认，避免丢掉 egui 自身的交互反馈色。
//!
//! 未建模的 5 个 token（scrollbar 4 个 + focus-ring 1 个）登记在 docs/02 §6。

use egui::{Color32, CornerRadius, Stroke, Visuals};

use nfc_core::theme::{Rgb, Theme};

/// `Theme` / `Rgb` 属 `nfc-core`，`Color32` 属 `egui`，两侧都是外部 crate——
/// 不能直接 `impl From<Rgb> for Color32`（违反孤儿规则）。故用本 crate 的
/// newtype 包一层，转换挂在它自己身上。
#[derive(Clone, Copy, Debug)]
pub struct Paint(pub Theme);

impl From<Theme> for Paint {
    fn from(t: Theme) -> Self {
        Paint(t)
    }
}

/// 解引用到 `Theme`，让调用点能直接写 `p.primary` 而非 `p.0.primary`。
/// 只读视图，故取 `Theme` 的引用。
impl std::ops::Deref for Paint {
    type Target = Theme;
    fn deref(&self) -> &Theme {
        &self.0
    }
}

/// `Rgb` → `Color32`。`Rgb` 在 core、`Color32` 在 egui，两侧都是外部 crate，
/// 孤儿规则禁止直接实现，故挂在 newtype 上并另给一个自由函数。
impl Paint {
    /// 取主题色的 egui 表示。
    pub fn color(self, r: Rgb) -> Color32 {
        Color32::from_rgb(r.r(), r.g(), r.b())
    }
}

impl Paint {
    /// 该调色板是深色还是浅色。`Theme` 本身不记录这个信息（原版靠
    /// `<html class="dark">` 切换），此处按 `bg` 亮度判定。
    pub fn is_dark(self) -> bool {
        // Rec. 601 亮度；阈值 128 与 CSS `prefers-color-scheme` 惯例一致
        let b = self.0.bg;
        let luma = 0.299 * b.r() as f32 + 0.587 * b.g() as f32 + 0.114 * b.b() as f32;
        luma < 128.0
    }

    /// 桥接到 egui 视觉风格。
    pub fn to_visuals(self) -> Visuals {
        let mut v = if self.is_dark() {
            Visuals::dark()
        } else {
            Visuals::light()
        };

        // 面板 / 表面
        v.panel_fill = self.color(self.bg);
        v.window_fill = self.color(self.surface);
        v.extreme_bg_color = self.color(self.console_bg);
        v.faint_bg_color = self.color(self.surface2);
        v.code_bg_color = self.color(self.console_bg);

        // 文本
        v.override_text_color = Some(self.color(self.text));
        v.weak_text_color = Some(self.color(self.text_secondary));
        v.hyperlink_color = self.color(self.primary);
        v.warn_fg_color = self.color(self.danger);
        v.error_fg_color = self.color(self.danger);

        // 边框
        v.window_stroke = Stroke::new(1.0, self.color(self.border));
        // 每个状态桶各自带 bg_stroke / corner_radius / expansion（`Widgets`
        // 本身只存 5 个状态，不存这些字段）。
        let radius = CornerRadius::same(4);
        for w in [
            &mut v.widgets.noninteractive,
            &mut v.widgets.inactive,
            &mut v.widgets.hovered,
            &mut v.widgets.active,
            &mut v.widgets.open,
        ] {
            w.corner_radius = radius;
            w.expansion = 0.0;
        }
        v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, self.color(self.border));

        // 按钮四态：CSS 里 hover 才反色（primary 底 + 白字），故 noninteractive 透明。
        v.widgets.noninteractive.bg_fill = self.color(self.surface);
        v.widgets.noninteractive.weak_bg_fill = Color32::TRANSPARENT;
        v.widgets.noninteractive.fg_stroke = Stroke::new(1.0, self.color(self.text));

        v.widgets.inactive.bg_fill = self.color(self.surface);
        v.widgets.inactive.weak_bg_fill = self.color(self.surface);
        v.widgets.inactive.fg_stroke = Stroke::new(1.0, self.color(self.text));

        v.widgets.hovered.bg_fill = self.color(self.button_hover_bg);
        v.widgets.hovered.weak_bg_fill = self.color(self.button_hover_bg);
        v.widgets.hovered.fg_stroke = Stroke::new(1.0, self.color(self.button_hover_text));

        v.widgets.active.bg_fill = self.color(self.primary_active);
        v.widgets.active.weak_bg_fill = self.color(self.primary_active);
        v.widgets.active.fg_stroke = Stroke::new(1.0, self.color(self.button_hover_text));

        v.widgets.open.bg_fill = self.color(self.hover_overlay);
        v.widgets.open.weak_bg_fill = self.color(self.hover_overlay);
        v.widgets.open.fg_stroke = Stroke::new(1.0, self.color(self.text));

        // 选中态：原版用 --color-selected 整行反白
        v.selection.bg_fill = self.color(self.selected);
        v.selection.stroke = Stroke::new(1.0, self.color(self.primary));

        // 输入框
        v.text_edit_bg_color = Some(self.color(self.input_bg));

        v
    }

    /// 设备状态条四色（`index.js` 里硬编码的四个值，非 token）。
    pub fn device_dot(self, state: nfc_core::state::DeviceState) -> Color32 {
        use nfc_core::state::DeviceState;
        match state {
            // index.js: device-status-indicator
            DeviceState::Connecting => Color32::from_rgb(0xb6, 0xb2, 0x39),
            DeviceState::Connected => Color32::from_rgb(0x54, 0xad, 0x6c),
            DeviceState::Failed => Color32::from_rgb(0xcf, 0x41, 0x52),
            DeviceState::None => Color32::from_gray(0x60),
        }
    }

    /// 任务状态三色指示（`index.js` 里 status-indicator）。
    pub fn status_dot(self, indicator: nfc_core::state::Indicator) -> Color32 {
        use nfc_core::state::Indicator;
        match indicator {
            // "free" → background-color: transparent
            Indicator::Free => Color32::TRANSPARENT,
            // "running" → green
            Indicator::Running => Color32::from_rgb(0x00, 0xb0, 0x00),
            // "error" → red
            Indicator::Error => Color32::from_rgb(0xff, 0x00, 0x00),
        }
    }
}

/// 事件条三种 kind 的着色（`event-line event-{type}`）。
pub fn event_color(theme: Theme, kind: nfc_core::state::EventKind) -> Color32 {
    use nfc_core::state::EventKind;
    match kind {
        EventKind::Key => Paint(theme).color(theme.key_a),
        EventKind::Success => Paint(theme).color(theme.console_fg),
        EventKind::Error => Paint(theme).color(theme.danger),
    }
}
