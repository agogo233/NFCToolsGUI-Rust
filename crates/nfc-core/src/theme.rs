//! 主题 token。
//!
//! 颜色逐字对标原版 `src/renderer/css/tokens.css`（`@media
//! (prefers-color-scheme: dark)` 对应 `dark()`）；圆角/字号/间距来自
//! `--radius-*` / `--fs-*` / `--space-*`。
//!
//! 范围：tokens.css 有 38 个 `--color-*`，本模块建模其中 33 个。
//! 未建模 8 类：原生滚动条（`--color-scrollbar*` ×4）、焦点环
//! （`--color-focus-ring`）、投影（`--shadow-*` ×3）——eframe/egui 不暴露
//! 原生滚动条样式。缺口登记见 docs/02 §6.3。
//! 设备状态色来自原版 JS 状态条（见 docs/01 §7.3），不在 tokens.css 内。

/// RGB 颜色，不透明。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rgb(pub [u8; 3]);

impl Rgb {
    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self([r, g, b])
    }

    pub fn r(self) -> u8 {
        self.0[0]
    }

    pub fn g(self) -> u8 {
        self.0[1]
    }

    pub fn b(self) -> u8 {
        self.0[2]
    }

    /// 解析 `#rrggbb`：长度≠7 或首字符非 `#` 时返回黑；单个十六进制位非法时按 0 计。
    pub fn parse(s: &str) -> Self {
        let b = s.as_bytes();
        if b.len() != 7 || b[0] != b'#' {
            return Rgb::new(0, 0, 0);
        }
        let d = |c: u8| -> u8 {
            match c {
                b'0'..=b'9' => c - b'0',
                b'a'..=b'f' => c - b'a' + 10,
                b'A'..=b'F' => c - b'A' + 10,
                _ => 0,
            }
        };
        Rgb::new(
            d(b[1]) * 16 + d(b[2]),
            d(b[3]) * 16 + d(b[4]),
            d(b[5]) * 16 + d(b[6]),
        )
    }

    /// 格式化为 `#rrggbb`（小写）。
    pub fn to_hex(self) -> String {
        format!("#{:02x}{:02x}{:02x}", self.r(), self.g(), self.b())
    }
}

/// 全套颜色 token。字段名与 tokens.css 的 CSS 变量一一对应。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Theme {
    pub primary: Rgb,
    pub primary_hover: Rgb,
    pub primary_active: Rgb,
    pub text_inverse: Rgb,
    pub selection_fg: Rgb,
    pub bg: Rgb,
    pub surface: Rgb,
    pub surface2: Rgb,
    pub toolbar: Rgb,
    pub text: Rgb,
    pub text_secondary: Rgb,
    pub border: Rgb,
    pub divider: Rgb,
    pub selected: Rgb,
    pub hover_overlay: Rgb,
    pub button_hover_bg: Rgb,
    pub button_hover_text: Rgb,
    pub disabled_bg: Rgb,
    pub disabled_fg: Rgb,
    pub input_border: Rgb,
    pub input_bg: Rgb,
    pub console_bg: Rgb,
    pub console_fg: Rgb,
    pub danger: Rgb,
    pub tooltip_bg: Rgb,
    pub tooltip_border: Rgb,
    pub tooltip_fg: Rgb,
    pub mark_bg: Rgb,
    pub mark_fg: Rgb,
    pub uid: Rgb,
    pub key_a: Rgb,
    pub key_b: Rgb,
    pub access: Rgb,
}

/// 浅色主题。
pub fn light() -> Theme {
    Theme {
        primary: Rgb::new(0x41, 0x68, 0xcf),
        primary_hover: Rgb::new(0x3c, 0x5e, 0xb3),
        primary_active: Rgb::new(0x34, 0x53, 0x9f),
        text_inverse: Rgb::new(0xff, 0xff, 0xff),
        selection_fg: Rgb::new(0xff, 0xff, 0xff),
        bg: Rgb::new(0xf7, 0xf8, 0xfa),
        surface: Rgb::new(0xff, 0xff, 0xff),
        surface2: Rgb::new(0xf4, 0xf5, 0xfc),
        toolbar: Rgb::new(0xf7, 0xf8, 0xfa),
        text: Rgb::new(0x27, 0x28, 0x2d),
        text_secondary: Rgb::new(0x6b, 0x6f, 0x76),
        border: Rgb::new(0xeb, 0xec, 0xf0),
        divider: Rgb::new(0xd0, 0xd0, 0xd5),
        selected: Rgb::new(0xd2, 0xde, 0xf9),
        hover_overlay: Rgb::new(0xdd, 0xdf, 0xe3),
        button_hover_bg: Rgb::new(0x41, 0x68, 0xcf),
        button_hover_text: Rgb::new(0xff, 0xff, 0xff),
        disabled_bg: Rgb::new(0xe0, 0xe0, 0xe0),
        disabled_fg: Rgb::new(0xa0, 0xa0, 0xa0),
        input_border: Rgb::new(0xc7, 0xca, 0xd1),
        input_bg: Rgb::new(0xff, 0xff, 0xff),
        console_bg: Rgb::new(0x15, 0x17, 0x28),
        console_fg: Rgb::new(0x54, 0xad, 0x6c),
        danger: Rgb::new(0xeb, 0x71, 0x71),
        tooltip_bg: Rgb::new(0x27, 0x28, 0x2d),
        tooltip_border: Rgb::new(0xff, 0xff, 0xff),
        tooltip_fg: Rgb::new(0xff, 0xff, 0xff),
        mark_bg: Rgb::new(0xff, 0xd6, 0xd6),
        mark_fg: Rgb::new(0x00, 0x00, 0x00),
        uid: Rgb::new(0x77, 0x42, 0xb8),
        key_a: Rgb::new(0x3f, 0x91, 0x42),
        key_b: Rgb::new(0x4d, 0x80, 0x27),
        access: Rgb::new(0xc2, 0x41, 0x0c),
    }
}

/// 深色主题。
pub fn dark() -> Theme {
    Theme {
        primary: Rgb::new(0x43, 0x69, 0xc7),
        primary_hover: Rgb::new(0x3c, 0x5e, 0xb3),
        primary_active: Rgb::new(0x38, 0x56, 0xb0),
        text_inverse: Rgb::new(0xff, 0xff, 0xff),
        selection_fg: Rgb::new(0xcb, 0xcd, 0xce),
        bg: Rgb::new(0x1b, 0x20, 0x2b),
        surface: Rgb::new(0x2b, 0x2d, 0x30),
        surface2: Rgb::new(0x38, 0x41, 0x51),
        toolbar: Rgb::new(0x2b, 0x2d, 0x30),
        text: Rgb::new(0xcb, 0xcd, 0xce),
        text_secondary: Rgb::new(0x8f, 0x93, 0x9a),
        border: Rgb::new(0x1e, 0x1f, 0x22),
        divider: Rgb::new(0x4c, 0x54, 0x63),
        selected: Rgb::new(0x32, 0x42, 0x6b),
        hover_overlay: Rgb::new(0x4e, 0x50, 0x55),
        button_hover_bg: Rgb::new(0x4c, 0x54, 0x63),
        button_hover_text: Rgb::new(0xcb, 0xcd, 0xce),
        disabled_bg: Rgb::new(0x1a, 0x1a, 0x1a),
        disabled_fg: Rgb::new(0xa0, 0xa0, 0xa0),
        input_border: Rgb::new(0x4c, 0x54, 0x63),
        input_bg: Rgb::new(0x3f, 0x3f, 0x46),
        console_bg: Rgb::new(0x15, 0x17, 0x28),
        console_fg: Rgb::new(0x54, 0xad, 0x6c),
        danger: Rgb::new(0xeb, 0x71, 0x71),
        tooltip_bg: Rgb::new(0x29, 0x3b, 0x40),
        tooltip_border: Rgb::new(0x1e, 0x1f, 0x22),
        tooltip_fg: Rgb::new(0xcb, 0xcd, 0xce),
        mark_bg: Rgb::new(0x7c, 0x5d, 0x5d),
        mark_fg: Rgb::new(0xcb, 0xcd, 0xce),
        uid: Rgb::new(0xb1, 0x8c, 0xea),
        key_a: Rgb::new(0x96, 0xe1, 0x87),
        key_b: Rgb::new(0x74, 0xd0, 0x5c),
        access: Rgb::new(0xf0, 0x99, 0x6e),
    }
}

// ---- 几何 token（--radius-* / --fs-* / --space-*）----

/// 圆角：sm / md / lg / pill。
pub const RADIUS: [f32; 4] = [6.0, 10.0, 16.0, 999.0];

/// 字号：xs / sm / base / md / lg / title。
pub const FONT_SIZE: [f32; 6] = [12.0, 13.0, 14.0, 15.0, 16.0, 20.0];

/// 间距：space-1 .. space-6。
pub const SPACING: [f32; 6] = [4.0, 8.0, 12.0, 16.0, 20.0, 24.0];

// ---- 设备连接态颜色（两主题共用）----
// 仅用于设备状态条与设备下拉；任务状态条在原版用字面色 green/red/transparent，勿混用。

/// 已连接 / 成功（`#54ad6c`）
pub const STATUS_OK: Rgb = Rgb::new(0x54, 0xad, 0x6c);
/// 连接中 / 警告（`#b6b239`）
pub const STATUS_WARN: Rgb = Rgb::new(0xb6, 0xb2, 0x39);
/// 连接失败 / 错误（`#cf4152`）
pub const STATUS_ERR: Rgb = Rgb::new(0xcf, 0x41, 0x52);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn light_theme_matches_tokens_css() {
        let t = light();
        assert_eq!(t.primary.to_hex(), "#4168cf");
        assert_eq!(t.primary_hover.to_hex(), "#3c5eb3");
        assert_eq!(t.primary_active.to_hex(), "#34539f");
        assert_eq!(t.text_inverse.to_hex(), "#ffffff");
        assert_eq!(t.selection_fg.to_hex(), "#ffffff");
        assert_eq!(t.bg.to_hex(), "#f7f8fa");
        assert_eq!(t.surface.to_hex(), "#ffffff");
        assert_eq!(t.surface2.to_hex(), "#f4f5fc");
        assert_eq!(t.toolbar.to_hex(), "#f7f8fa");
        assert_eq!(t.text.to_hex(), "#27282d");
        assert_eq!(t.text_secondary.to_hex(), "#6b6f76");
        assert_eq!(t.border.to_hex(), "#ebecf0");
        assert_eq!(t.divider.to_hex(), "#d0d0d5");
        assert_eq!(t.selected.to_hex(), "#d2def9");
        assert_eq!(t.hover_overlay.to_hex(), "#dddfe3");
        assert_eq!(t.button_hover_bg.to_hex(), "#4168cf");
        assert_eq!(t.button_hover_text.to_hex(), "#ffffff");
        assert_eq!(t.disabled_bg.to_hex(), "#e0e0e0");
        assert_eq!(t.disabled_fg.to_hex(), "#a0a0a0");
        assert_eq!(t.input_border.to_hex(), "#c7cad1");
        assert_eq!(t.input_bg.to_hex(), "#ffffff");
        assert_eq!(t.console_bg.to_hex(), "#151728");
        assert_eq!(t.console_fg.to_hex(), "#54ad6c");
        assert_eq!(t.danger.to_hex(), "#eb7171");
        assert_eq!(t.tooltip_bg.to_hex(), "#27282d");
        assert_eq!(t.tooltip_border.to_hex(), "#ffffff");
        assert_eq!(t.tooltip_fg.to_hex(), "#ffffff");
        assert_eq!(t.mark_bg.to_hex(), "#ffd6d6");
        assert_eq!(t.mark_fg.to_hex(), "#000000");
        assert_eq!(t.uid.to_hex(), "#7742b8");
        assert_eq!(t.key_a.to_hex(), "#3f9142");
        assert_eq!(t.key_b.to_hex(), "#4d8027");
        assert_eq!(t.access.to_hex(), "#c2410c");
    }

    #[test]
    fn dark_theme_matches_tokens_css() {
        let t = dark();
        assert_eq!(t.primary.to_hex(), "#4369c7");
        assert_eq!(t.primary_hover.to_hex(), "#3c5eb3");
        assert_eq!(t.primary_active.to_hex(), "#3856b0");
        assert_eq!(t.text_inverse.to_hex(), "#ffffff");
        assert_eq!(t.selection_fg.to_hex(), "#cbcdce");
        assert_eq!(t.bg.to_hex(), "#1b202b");
        assert_eq!(t.surface.to_hex(), "#2b2d30");
        assert_eq!(t.surface2.to_hex(), "#384151");
        assert_eq!(t.toolbar.to_hex(), "#2b2d30");
        assert_eq!(t.text.to_hex(), "#cbcdce");
        assert_eq!(t.text_secondary.to_hex(), "#8f939a");
        assert_eq!(t.border.to_hex(), "#1e1f22");
        assert_eq!(t.divider.to_hex(), "#4c5463");
        assert_eq!(t.selected.to_hex(), "#32426b");
        assert_eq!(t.hover_overlay.to_hex(), "#4e5055");
        assert_eq!(t.button_hover_bg.to_hex(), "#4c5463");
        assert_eq!(t.button_hover_text.to_hex(), "#cbcdce");
        assert_eq!(t.disabled_bg.to_hex(), "#1a1a1a");
        assert_eq!(t.disabled_fg.to_hex(), "#a0a0a0");
        assert_eq!(t.input_border.to_hex(), "#4c5463");
        assert_eq!(t.input_bg.to_hex(), "#3f3f46");
        assert_eq!(t.console_bg.to_hex(), "#151728");
        assert_eq!(t.console_fg.to_hex(), "#54ad6c");
        assert_eq!(t.danger.to_hex(), "#eb7171");
        assert_eq!(t.tooltip_bg.to_hex(), "#293b40");
        assert_eq!(t.tooltip_border.to_hex(), "#1e1f22");
        assert_eq!(t.tooltip_fg.to_hex(), "#cbcdce");
        assert_eq!(t.mark_bg.to_hex(), "#7c5d5d");
        assert_eq!(t.mark_fg.to_hex(), "#cbcdce");
        assert_eq!(t.uid.to_hex(), "#b18cea");
        assert_eq!(t.key_a.to_hex(), "#96e187");
        assert_eq!(t.key_b.to_hex(), "#74d05c");
        assert_eq!(t.access.to_hex(), "#f0996e");
    }

    #[test]
    fn tokens_identical_across_themes() {
        // tokens.css 中明暗两态取同一值的 6 个 token
        let (l, d) = (light(), dark());
        assert_eq!(l.primary_hover, d.primary_hover);
        assert_eq!(l.text_inverse, d.text_inverse);
        assert_eq!(l.disabled_fg, d.disabled_fg);
        assert_eq!(l.console_bg, d.console_bg);
        assert_eq!(l.console_fg, d.console_fg);
        assert_eq!(l.danger, d.danger);
    }

    #[test]
    fn dark_flips_backgrounds_and_text() {
        let (l, d) = (light(), dark());
        let lum = |c: Rgb| c.r() as u32 + c.g() as u32 + c.b() as u32;

        // 背景变暗
        for (name, lv, dv) in [
            ("bg", l.bg, d.bg),
            ("surface", l.surface, d.surface),
            ("surface2", l.surface2, d.surface2),
        ] {
            assert!(lum(dv) < lum(lv), "{name}: dark 背景应比 light 更暗");
        }
        // 文本变亮（方向相反）
        for (name, lv, dv) in [
            ("text", l.text, d.text),
            ("text_secondary", l.text_secondary, d.text_secondary),
        ] {
            assert!(lum(dv) > lum(lv), "{name}: dark 文本应比 light 更亮");
        }
    }

    #[test]
    fn geometry_constants_are_monotonic() {
        // 值来源 tokens.css `--radius-*` / `--fs-*` / `--space-*`。
        // 不重复断言字面量（会与 const 定义自指），只校验单调递增这一真实不变量。
        for arr in [RADIUS.as_slice(), FONT_SIZE.as_slice(), SPACING.as_slice()] {
            assert!(arr.windows(2).all(|w| w[0] < w[1]), "{arr:?} 应严格递增");
        }
    }

    #[test]
    fn device_status_colors() {
        assert_eq!(STATUS_OK.to_hex(), "#54ad6c");
        assert_eq!(STATUS_WARN.to_hex(), "#b6b239");
        assert_eq!(STATUS_ERR.to_hex(), "#cf4152");
    }

    #[test]
    fn rgb_parse_roundtrip() {
        for hex in ["#4168cf", "#000000", "#ffffff", "#54ad6c", "#7742b8"] {
            assert_eq!(Rgb::parse(hex).to_hex(), hex);
            assert_eq!(Rgb::parse(hex.to_uppercase().as_str()).to_hex(), hex);
        }
    }

    #[test]
    fn rgb_parse_rejects_garbage() {
        assert_eq!(Rgb::parse(""), Rgb::new(0, 0, 0));
        assert_eq!(Rgb::parse("#zzz"), Rgb::new(0, 0, 0));
        assert_eq!(Rgb::parse("4168cf"), Rgb::new(0, 0, 0));
        assert_eq!(Rgb::parse("#4168"), Rgb::new(0, 0, 0));
        assert_eq!(Rgb::parse("#4168cff"), Rgb::new(0, 0, 0));
        assert_eq!(Rgb::parse("#416gcf"), Rgb::new(0x41, 0x60, 0xcf));
    }

    #[test]
    fn rgb_components() {
        let c = Rgb::new(0x12, 0x34, 0x56);
        assert_eq!(c.r(), 0x12);
        assert_eq!(c.g(), 0x34);
        assert_eq!(c.b(), 0x56);
        assert_eq!(c.0, [0x12, 0x34, 0x56]);
    }
}
