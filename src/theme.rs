//! Look and feel: fonts, colours, light/dark styles.

use egui::{Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Margin, Shadow, Stroke, TextStyle, Theme, Visuals};

pub const INTER: &[u8] = include_bytes!("../assets/fonts/Inter-Regular.ttf");
pub const INTER_SEMIBOLD: &[u8] = include_bytes!("../assets/fonts/Inter-SemiBold.ttf");

pub fn bold() -> FontFamily {
    FontFamily::Name("bold".into())
}

/// Colours used outside egui's own widgets.
#[derive(Clone, Copy)]
pub struct Palette {
    pub accent: Color32,
    pub canvas: Color32,
    pub sidebar: Color32,
    pub toolbar: Color32,
    pub separator: Color32,
    pub text: Color32,
    pub text_weak: Color32,
    pub hover: Color32,
    pub selected: Color32,
    pub page_shadow: Color32,
    pub dark: bool,
}

impl Palette {
    pub fn of(ctx: &egui::Context) -> Self {
        ctx.data(|d| d.get_temp::<Palette>(egui::Id::new("review-palette")))
            .filter(|p| p.dark == (ctx.theme() == Theme::Dark))
            .unwrap_or_else(|| Self::build(ctx.theme() == Theme::Dark, accent()))
    }

    fn build(dark: bool, accent: Color32) -> Self {
        if dark {
            Self {
                accent,
                canvas: Color32::from_rgb(0x1C, 0x1C, 0x1E),
                sidebar: Color32::from_rgb(0x26, 0x26, 0x29),
                toolbar: Color32::from_rgb(0x2C, 0x2C, 0x2F),
                separator: Color32::from_rgb(0x3A, 0x3A, 0x3E),
                text: Color32::from_rgb(0xEC, 0xEC, 0xEE),
                text_weak: Color32::from_rgb(0x9A, 0x9A, 0xA0),
                hover: Color32::from_white_alpha(14),
                selected: accent.gamma_multiply(0.35),
                page_shadow: Color32::from_black_alpha(140),
                dark,
            }
        } else {
            Self {
                accent,
                canvas: Color32::from_rgb(0xE9, 0xE9, 0xEB),
                sidebar: Color32::from_rgb(0xF4, 0xF4, 0xF6),
                toolbar: Color32::from_rgb(0xFB, 0xFB, 0xFC),
                separator: Color32::from_rgb(0xDA, 0xDA, 0xDE),
                text: Color32::from_rgb(0x1D, 0x1D, 0x1F),
                text_weak: Color32::from_rgb(0x6E, 0x6E, 0x73),
                hover: Color32::from_black_alpha(12),
                selected: accent.gamma_multiply(0.22),
                page_shadow: Color32::from_black_alpha(55),
                dark,
            }
        }
    }
}

/// The desktop accent colour: Windows' DWM accent, KDE's kdeglobals, else a calm blue.
pub fn accent() -> Color32 {
    let fallback = Color32::from_rgb(0x2F, 0x7C, 0xF6);
    #[cfg(windows)]
    {
        use windows::core::w;
        use windows::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_DWORD};
        let mut value: u32 = 0;
        let mut size = std::mem::size_of::<u32>() as u32;
        // SAFETY: reading a DWORD into a correctly sized buffer.
        let ok = unsafe {
            RegGetValueW(HKEY_CURRENT_USER, w!("Software\\Microsoft\\Windows\\DWM"), w!("AccentColor"), RRF_RT_REG_DWORD, None, Some(&mut value as *mut u32 as *mut _), Some(&mut size))
        };
        if ok.is_ok() {
            // Stored as 0xAABBGGRR.
            return Color32::from_rgb((value & 0xFF) as u8, (value >> 8 & 0xFF) as u8, (value >> 16 & 0xFF) as u8);
        }
        return fallback;
    }
    #[allow(unreachable_code)]
    let Some(cfg) = dirs::config_dir() else { return fallback };
    let Ok(text) = std::fs::read_to_string(cfg.join("kdeglobals")) else { return fallback };
    let mut section = "";
    let mut selection_bg = None;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            section = line;
            continue;
        }
        let Some((k, v)) = line.split_once('=') else { continue };
        let rgb: Vec<u8> = v.split(',').filter_map(|x| x.trim().parse().ok()).collect();
        if rgb.len() < 3 {
            continue;
        }
        let c = Color32::from_rgb(rgb[0], rgb[1], rgb[2]);
        if section == "[General]" && k.trim() == "AccentColor" {
            return c;
        }
        if section == "[Colors:Selection]" && k.trim() == "BackgroundNormal" {
            selection_bg = Some(c);
        }
    }
    selection_bg.unwrap_or(fallback)
}

pub fn install(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    fonts.font_data.insert("inter".into(), FontData::from_static(INTER).into());
    fonts.font_data.insert("inter-semibold".into(), FontData::from_static(INTER_SEMIBOLD).into());
    fonts.families.entry(FontFamily::Proportional).or_default().insert(0, "inter".into());
    let mut bold_keys = fonts.families.get(&FontFamily::Proportional).cloned().unwrap_or_default();
    bold_keys.insert(0, "inter-semibold".into());
    fonts.families.insert(bold(), bold_keys);
    egui_phosphor::add_to_fonts(&mut fonts, egui_phosphor::Variant::Regular);
    ctx.set_fonts(fonts);

    let accent = accent();
    for theme in [Theme::Light, Theme::Dark] {
        let pal = Palette::build(theme == Theme::Dark, accent);
        ctx.style_mut_of(theme, |style| {
            style.text_styles = [
                (TextStyle::Heading, FontId::new(17.0, bold())),
                (TextStyle::Body, FontId::new(13.5, FontFamily::Proportional)),
                (TextStyle::Monospace, FontId::new(12.5, FontFamily::Monospace)),
                (TextStyle::Button, FontId::new(13.5, FontFamily::Proportional)),
                (TextStyle::Small, FontId::new(11.0, FontFamily::Proportional)),
            ]
            .into();
            let s = &mut style.spacing;
            s.item_spacing = egui::vec2(8.0, 6.0);
            s.button_padding = egui::vec2(8.0, 4.0);
            s.interact_size = egui::vec2(28.0, 26.0);
            s.menu_margin = Margin::same(6);
            s.window_margin = Margin::same(14);
            s.slider_width = 180.0;
            s.combo_width = 140.0;
            s.scroll.bar_width = 8.0;
            s.scroll.floating = true;
            style.animation_time = 0.12;
            style.visuals = visuals(&pal);
        });
    }
    ctx.data_mut(|d| d.insert_temp(egui::Id::new("review-accent"), accent));
}

fn visuals(p: &Palette) -> Visuals {
    let mut v = if p.dark { Visuals::dark() } else { Visuals::light() };
    let r = CornerRadius::same(6);
    v.panel_fill = p.toolbar;
    v.window_fill = if p.dark { Color32::from_rgb(0x2A, 0x2A, 0x2D) } else { Color32::from_rgb(0xFD, 0xFD, 0xFD) };
    v.window_stroke = Stroke::new(1.0, p.separator);
    v.window_corner_radius = CornerRadius::same(12);
    v.menu_corner_radius = CornerRadius::same(8);
    v.window_shadow = Shadow { offset: [0, 8], blur: 28, spread: 0, color: Color32::from_black_alpha(if p.dark { 110 } else { 40 }) };
    v.popup_shadow = Shadow { offset: [0, 4], blur: 16, spread: 0, color: Color32::from_black_alpha(if p.dark { 100 } else { 32 }) };
    v.selection.bg_fill = p.accent.gamma_multiply(if p.dark { 0.55 } else { 0.35 });
    v.selection.stroke = Stroke::new(1.0, p.accent);
    v.hyperlink_color = p.accent;
    v.extreme_bg_color = if p.dark { Color32::from_rgb(0x1E, 0x1E, 0x20) } else { Color32::WHITE };
    v.faint_bg_color = p.sidebar;
    v.override_text_color = None;
    v.slider_trailing_fill = true;
    v.handle_shape = egui::style::HandleShape::Circle;
    v.striped = false;

    let w = &mut v.widgets;
    w.noninteractive.bg_stroke = Stroke::new(1.0, p.separator);
    w.noninteractive.fg_stroke = Stroke::new(1.0, p.text);
    w.noninteractive.corner_radius = r;
    w.inactive.weak_bg_fill = Color32::TRANSPARENT;
    w.inactive.bg_fill = if p.dark { Color32::from_rgb(0x3A, 0x3A, 0x3E) } else { Color32::from_rgb(0xE4, 0xE4, 0xE8) };
    w.inactive.bg_stroke = Stroke::NONE;
    w.inactive.fg_stroke = Stroke::new(1.2, p.text);
    w.inactive.corner_radius = r;
    w.hovered.weak_bg_fill = p.hover.gamma_multiply(1.6);
    w.hovered.bg_fill = if p.dark { Color32::from_rgb(0x46, 0x46, 0x4B) } else { Color32::from_rgb(0xDA, 0xDA, 0xDF) };
    w.hovered.bg_stroke = Stroke::NONE;
    w.hovered.fg_stroke = Stroke::new(1.4, p.text);
    w.hovered.corner_radius = r;
    w.hovered.expansion = 0.0;
    w.active.weak_bg_fill = p.hover.gamma_multiply(2.4);
    w.active.bg_fill = p.accent;
    w.active.bg_stroke = Stroke::NONE;
    w.active.fg_stroke = Stroke::new(1.5, p.text);
    w.active.corner_radius = r;
    w.active.expansion = 0.0;
    w.open.weak_bg_fill = p.hover.gamma_multiply(2.0);
    w.open.corner_radius = r;
    v
}
