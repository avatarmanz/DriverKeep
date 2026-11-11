//! Colours, fonts and the small building blocks every page shares.

use crate::system;
use egui::{
    Align, Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Frame, Layout,
    Margin, Pos2, RichText, Sense, Stroke, Vec2,
};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateIcon, FindWindowW, HICON, IsIconic, SW_RESTORE, SetForegroundWindow, ShowWindow,
};
use windows::core::{HSTRING, PCWSTR};

// Status colours read from the original's theme file; the teal() is from the person's pictures.
pub const SUCCESS: Color32 = Color32::from_rgb(0x3B, 0xBF, 0x88);
pub const CAUTION: Color32 = Color32::from_rgb(0xFF, 0x87, 0x3B);
pub const CRITICAL: Color32 = Color32::from_rgb(0xFF, 0x57, 0x57);
pub const BLUE: Color32 = Color32::from_rgb(0x4A, 0x6A, 0xE0);
pub const SKY: Color32 = Color32::from_rgb(0x5C, 0x9D, 0xF5);

/// The accent colour the person chose in Settings. It starts as the original teal.
pub const DEFAULT_ACCENT: [u8; 3] = [0x2A, 0xAE, 0x9B];

/// Ready-made accent colours, shown as swatches in Settings.
pub const ACCENTS: [(&str, [u8; 3]); 8] = [
    ("Teal", DEFAULT_ACCENT),
    ("Blue", [0x4A, 0x6A, 0xE0]),
    ("Sky", [0x2F, 0x8F, 0xE8]),
    ("Purple", [0x8B, 0x5C, 0xF6]),
    ("Pink", [0xD9, 0x46, 0x9B]),
    ("Red", [0xE0, 0x4F, 0x4F]),
    ("Orange", [0xE8, 0x7A, 0x2B]),
    ("Green", [0x3B, 0xA8, 0x5C]),
];

static ACCENT: AtomicU32 = AtomicU32::new(0x2A_AE_9B);

/// Gives true when the accent changed, so the colours that were set up once can be redone.
pub fn set_accent(rgb: [u8; 3]) -> bool {
    let value = u32::from_be_bytes([0, rgb[0], rgb[1], rgb[2]]);
    ACCENT.swap(value, Ordering::Relaxed) != value
}

pub fn teal() -> Color32 {
    let [_, r, g, b] = ACCENT.load(Ordering::Relaxed).to_be_bytes();
    Color32::from_rgb(r, g, b)
}

#[derive(Clone, Copy)]
pub struct Pal {
    pub bg: Color32,
    pub rail: Color32,
    pub card: Color32,
    pub field: Color32,
    pub line: Color32,
    pub text: Color32,
    pub soft: Color32,
    pub hover: Color32,
    pub chosen: Color32,
    pub faint: Color32,
}

const DARK: Pal = Pal {
    bg: Color32::from_rgb(0x1C, 0x1C, 0x1F),
    rail: Color32::from_rgb(0x17, 0x17, 0x1A),
    card: Color32::from_rgb(0x28, 0x28, 0x2C),
    field: Color32::from_rgb(0x1F, 0x1F, 0x22),
    line: Color32::from_rgb(0x36, 0x36, 0x3B),
    text: Color32::from_rgb(0xF2, 0xF2, 0xF4),
    soft: Color32::from_rgb(0xA6, 0xA8, 0xB2),
    hover: Color32::from_rgb(0x32, 0x32, 0x37),
    chosen: Color32::from_rgb(0x38, 0x38, 0x3E),
    faint: Color32::from_rgb(0x5A, 0x5B, 0x63),
};

const LIGHT: Pal = Pal {
    bg: Color32::from_rgb(0xF5, 0xF6, 0xF8),
    rail: Color32::from_rgb(0xEC, 0xEE, 0xF2),
    card: Color32::WHITE,
    field: Color32::from_rgb(0xF1, 0xF2, 0xF5),
    line: Color32::from_rgb(0xE3, 0xE5, 0xEA),
    text: Color32::from_rgb(0x1C, 0x1D, 0x22),
    soft: Color32::from_rgb(0x66, 0x69, 0x79),
    hover: Color32::from_rgb(0xEE, 0xEF, 0xF3),
    chosen: Color32::from_rgb(0xDD, 0xE1, 0xEA),
    faint: Color32::from_rgb(0xB5, 0xB8, 0xC4),
};

/// Dark with true black, so an OLED screen can switch those pixels off.
const OLED: Pal = Pal {
    bg: Color32::from_rgb(0x00, 0x00, 0x00),
    rail: Color32::from_rgb(0x00, 0x00, 0x00),
    card: Color32::from_rgb(0x0F, 0x0F, 0x11),
    field: Color32::from_rgb(0x08, 0x08, 0x09),
    line: Color32::from_rgb(0x26, 0x26, 0x2B),
    text: Color32::from_rgb(0xF2, 0xF2, 0xF4),
    soft: Color32::from_rgb(0xA0, 0xA2, 0xAC),
    hover: Color32::from_rgb(0x1A, 0x1A, 0x1D),
    chosen: Color32::from_rgb(0x24, 0x24, 0x28),
    faint: Color32::from_rgb(0x4A, 0x4B, 0x52),
};

static IS_DARK: AtomicBool = AtomicBool::new(true);
static IS_OLED: AtomicBool = AtomicBool::new(false);

pub fn set_dark(dark: bool) {
    IS_DARK.store(dark, Ordering::Relaxed);
}

/// Gives true when the choice changed, so the colours that were set up once can be redone.
pub fn set_oled(oled: bool) -> bool {
    IS_OLED.swap(oled, Ordering::Relaxed) != oled
}

pub fn pal() -> Pal {
    if !IS_DARK.load(Ordering::Relaxed) {
        LIGHT
    } else if IS_OLED.load(Ordering::Relaxed) {
        OLED
    } else {
        DARK
    }
}

pub const ICON_FONT: &str = "icons";
pub const ICON_SCAN: &str = "\u{E721}";
pub const ICON_SYSTEM: &str = "\u{E7F4}";
pub const ICON_BACKUP: &str = "\u{E74E}";
pub const ICON_SETTINGS: &str = "\u{E713}";
pub const ICON_DRIVERS: &str = "\u{E895}";
pub const ICON_CHECK: &str = "\u{E73E}";
pub const ICON_THEME: &str = "\u{E793}";
pub const ICON_POWER: &str = "\u{E7E8}";
pub const ICON_HIDE: &str = "\u{ED1A}";
pub const ICON_TRAY: &str = "\u{E73F}";
pub const ICON_CLOCK: &str = "\u{E823}";
pub const ICON_BELL: &str = "\u{EA8F}";
pub const ICON_SILENT: &str = "\u{E896}";
pub const ICON_RESTART: &str = "\u{E72C}";
pub const ICON_SHIELD: &str = "\u{EA18}";
pub const ICON_FOLDER: &str = "\u{E8B7}";
pub const ICON_GLOBE: &str = "\u{E774}";
pub const ICON_RESET: &str = "\u{E777}";
pub const ICON_ABOUT: &str = "\u{E946}";

pub fn plural(count: usize) -> &'static str {
    if count == 1 { "" } else { "s" }
}

pub fn size_text(bytes: u64) -> String {
    const MB: f64 = 1024.0 * 1024.0;
    let value = bytes as f64;
    if value >= 1024.0 * MB {
        format!("{:.1} GB", value / (1024.0 * MB))
    } else if value >= MB {
        format!("{:.1} MB", value / MB)
    } else {
        format!("{:.0} KB", (value / 1024.0).max(1.0))
    }
}

pub fn ago(unix: i64) -> String {
    let minutes = (chrono::Local::now().timestamp() - unix).max(0) / 60;
    match minutes {
        0 => "just now".into(),
        1..=59 => format!("{minutes} minute{} ago", plural(minutes as usize)),
        60..=1439 => format!("{} hour{} ago", minutes / 60, plural((minutes / 60) as usize)),
        _ => format!("{} day{} ago", minutes / 1440, plural((minutes / 1440) as usize)),
    }
}

pub fn hour_text(hour: u32) -> String {
    match hour {
        0 => "12 AM".into(),
        1..=11 => format!("{hour} AM"),
        12 => "12 PM".into(),
        _ => format!("{} PM", hour - 12),
    }
}

/// Opens the window that's already running, if any. True means "carry on starting".
pub fn bring_to_front() -> bool {
    let title = HSTRING::from(system::window_title());
    let Ok(found) = (unsafe { FindWindowW(PCWSTR::null(), &title) }) else {
        return true;
    };
    if found.is_invalid() {
        return true;
    }
    unsafe {
        if IsIconic(found).as_bool() {
            let _ = ShowWindow(found, SW_RESTORE);
        }
        let _ = SetForegroundWindow(found);
    }
    false
}

/// A rounded blue-to-teal() square with a white tick.
fn icon_pixel(x: u32, y: u32, size: u32) -> [u8; 4] {
    let s = size as f32;
    let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
    let r = s * 0.22;
    let dx = (r - px).max(px - (s - r)).max(0.0);
    let dy = (r - py).max(py - (s - r)).max(0.0);
    if dx * dx + dy * dy > r * r {
        return [0, 0, 0, 0];
    }
    let t = (x + y) as f32 / (2.0 * s);
    let mix = |a: f32, b: f32| (a * (1.0 - t) + b * t) as u8;
    let segment = |ax: f32, ay: f32, bx: f32, by: f32| {
        let (vx, vy) = (bx - ax, by - ay);
        let k = (((px - ax) * vx + (py - ay) * vy) / (vx * vx + vy * vy)).clamp(0.0, 1.0);
        let (qx, qy) = (ax + vx * k - px, ay + vy * k - py);
        (qx * qx + qy * qy).sqrt()
    };
    let distance = segment(s * 0.27, s * 0.52, s * 0.44, s * 0.69)
        .min(segment(s * 0.44, s * 0.69, s * 0.74, s * 0.33));
    if distance < s * 0.075 {
        [255, 255, 255, 255]
    } else {
        [mix(0x4A as f32, 0x2A as f32), mix(0x6A as f32, 0xAE as f32), mix(0xE0 as f32, 0x9B as f32), 255]
    }
}

pub fn window_icon() -> egui::IconData {
    const SIZE: u32 = 64;
    let rgba = (0..SIZE)
        .flat_map(|y| (0..SIZE).flat_map(move |x| icon_pixel(x, y, SIZE)))
        .collect();
    egui::IconData { rgba, width: SIZE, height: SIZE }
}

/// Writes the app icon as an .ico file, for packaging.
pub fn write_icon(path: &std::path::Path) -> Result<(), String> {
    const SIZE: u32 = 256;
    let rgba: Vec<u8> =
        (0..SIZE).flat_map(|y| (0..SIZE).flat_map(move |x| icon_pixel(x, y, SIZE))).collect();
    let picture = image::RgbaImage::from_raw(SIZE, SIZE, rgba).ok_or("Bad icon size")?;
    let mut png = std::io::Cursor::new(Vec::new());
    picture.write_to(&mut png, image::ImageFormat::Png).map_err(|e| e.to_string())?;
    let png = png.into_inner();
    let mut file = vec![0, 0, 1, 0, 1, 0, 0, 0, 0, 0, 1, 0, 32, 0];
    file.extend((png.len() as u32).to_le_bytes());
    file.extend(22u32.to_le_bytes());
    file.extend(png);
    std::fs::write(path, file).map_err(|e| e.to_string())
}

pub fn tray_icon() -> Option<HICON> {
    const SIZE: u32 = 32;
    let mut color = Vec::with_capacity((SIZE * SIZE * 4) as usize);
    for y in 0..SIZE {
        for x in 0..SIZE {
            let [r, g, b, a] = icon_pixel(x, y, SIZE);
            color.extend_from_slice(&[b, g, r, a]);
        }
    }
    let mask = vec![0u8; (SIZE * SIZE / 8) as usize];
    unsafe {
        let module = windows::Win32::System::LibraryLoader::GetModuleHandleW(None).ok()?;
        CreateIcon(Some(module.into()), SIZE as i32, SIZE as i32, 1, 32, mask.as_ptr(), color.as_ptr()).ok()
    }
}

pub fn install_look(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    if let Ok(bytes) = std::fs::read("C:\\Windows\\Fonts\\segoeui.ttf") {
        fonts.font_data.insert("segoe".into(), Arc::new(FontData::from_owned(bytes)));
        if let Some(list) = fonts.families.get_mut(&FontFamily::Proportional) {
            list.insert(0, "segoe".into());
        }
    }
    let regular = fonts.families.get(&FontFamily::Proportional).cloned().unwrap_or_default();
    if let Ok(bytes) = std::fs::read("C:\\Windows\\Fonts\\seguisb.ttf") {
        fonts.font_data.insert("segoe-bold".into(), Arc::new(FontData::from_owned(bytes)));
        let mut list = vec!["segoe-bold".to_string()];
        list.extend(regular);
        fonts.families.insert(FontFamily::Name("bold".into()), list);
    } else {
        fonts.families.insert(FontFamily::Name("bold".into()), regular);
    }
    if let Ok(bytes) = std::fs::read("C:\\Windows\\Fonts\\segmdl2.ttf") {
        fonts.font_data.insert(ICON_FONT.into(), Arc::new(FontData::from_owned(bytes)));
        fonts.families.insert(FontFamily::Name(ICON_FONT.into()), vec![ICON_FONT.into()]);
    }
    ctx.set_fonts(fonts);
    // egui moves 40 points per wheel notch, which feels slow next to other Windows programs.
    ctx.options_mut(|options| options.input_options.line_scroll_speed = 110.0);
    apply_style(ctx);
}

/// Colours and spacing for both themes. Run again when the accent colour changes.
pub fn apply_style(ctx: &egui::Context) {
    let dark = if IS_OLED.load(Ordering::Relaxed) { OLED } else { DARK };
    for (theme, colors) in [(egui::Theme::Dark, dark), (egui::Theme::Light, LIGHT)] {
        ctx.style_mut_of(theme, |style| {
            let v = &mut style.visuals;
            v.panel_fill = colors.bg;
            v.window_fill = colors.card;
            v.window_stroke = Stroke::new(1.0, colors.line);
            v.extreme_bg_color = colors.field;
            v.override_text_color = Some(colors.text);
            v.selection.bg_fill = colors.chosen;
            v.selection.stroke = Stroke::new(1.0, teal());
            v.hyperlink_color = SKY;
            for w in [
                &mut v.widgets.noninteractive,
                &mut v.widgets.inactive,
                &mut v.widgets.hovered,
                &mut v.widgets.active,
                &mut v.widgets.open,
            ] {
                w.corner_radius = CornerRadius::same(6);
            }
            v.widgets.inactive.weak_bg_fill = colors.field;
            v.widgets.inactive.bg_fill = colors.field;
            v.widgets.hovered.weak_bg_fill = colors.hover;
            v.widgets.hovered.bg_fill = colors.hover;
            v.widgets.active.weak_bg_fill = colors.chosen;
            v.widgets.active.bg_fill = teal();
            v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, colors.line);
            style.spacing.item_spacing = Vec2::new(8.0, 6.0);
            style.spacing.button_padding = Vec2::new(12.0, 5.0);
            style.spacing.interact_size.y = 28.0;
        });
    }
}

pub fn bold(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name("bold".into()))
}

pub fn icon_font(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(ICON_FONT.into()))
}

pub fn icon(glyph: &str, size: f32, color: Color32) -> RichText {
    RichText::new(glyph).font(icon_font(size)).color(color)
}

pub fn card() -> Frame {
    Frame::new()
        .fill(pal().card)
        .corner_radius(CornerRadius::same(10))
        .inner_margin(Margin::symmetric(16, 12))
}

pub fn soft(ui: &mut egui::Ui, text: impl Into<String>) {
    ui.label(RichText::new(text.into()).color(pal().soft));
}

pub fn small(text: impl Into<String>) -> RichText {
    RichText::new(text.into()).size(12.0).color(pal().soft)
}

pub fn heading(ui: &mut egui::Ui, text: &str) {
    ui.label(RichText::new(text).font(bold(16.0)));
}

pub fn primary(ui: &mut egui::Ui, text: &str, enabled: bool) -> bool {
    let fill = if enabled { teal() } else { pal().faint };
    let button = egui::Button::new(RichText::new(text).color(Color32::WHITE).font(bold(14.0)))
        .fill(fill)
        .corner_radius(CornerRadius::same(8))
        .min_size(Vec2::new(116.0, 38.0));
    ui.add_enabled(enabled, button).clicked()
}

pub fn secondary(ui: &mut egui::Ui, text: &str, enabled: bool) -> bool {
    let button = egui::Button::new(text)
        .fill(pal().hover)
        .corner_radius(CornerRadius::same(7))
        .min_size(Vec2::new(0.0, 32.0));
    ui.add_enabled(enabled, button).clicked()
}

/// Sits beside Update All, so it's drawn the same height.
pub fn scan_again_button(ui: &mut egui::Ui, enabled: bool) -> bool {
    let button = egui::Button::new(RichText::new("Scan Again").font(bold(14.0)))
        .fill(pal().hover)
        .corner_radius(CornerRadius::same(8))
        .min_size(Vec2::new(116.0, 38.0));
    ui.add_enabled(enabled, button).clicked()
}

pub fn pill(ui: &mut egui::Ui, text: &str, color: Color32) {
    Frame::new()
        .fill(color.gamma_multiply(0.16))
        .corner_radius(CornerRadius::same(9))
        .inner_margin(Margin::symmetric(8, 1))
        .show(ui, |ui| {
            ui.label(RichText::new(text).size(11.5).color(color));
        });
}

pub fn progress_bar(ui: &mut egui::Ui, percent: u32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 5.0), Sense::hover());
    ui.painter().rect_filled(rect, 2.5, pal().line);
    let mut part = rect;
    part.set_width(rect.width() * percent.min(100) as f32 / 100.0);
    ui.painter().rect_filled(part, 2.5, teal());
}

pub fn toggle(ui: &mut egui::Ui, on: &mut bool) -> bool {
    let (rect, response) = ui.allocate_exact_size(Vec2::new(42.0, 22.0), Sense::click());
    let mut changed = false;
    if response.clicked() {
        *on = !*on;
        changed = true;
    }
    let t = ui.ctx().animate_bool(response.id, *on);
    ui.painter().rect_filled(rect, 11.0, if *on { teal() } else { pal().faint });
    let x = egui::lerp(rect.left() + 11.0..=rect.right() - 11.0, t);
    ui.painter().circle_filled(Pos2::new(x, rect.center().y), 8.0, Color32::WHITE);
    response.on_hover_cursor(egui::CursorIcon::PointingHand);
    changed
}

/// Text that sits to the left of buttons: it keeps `reserve` points free on the right and wraps
/// onto more lines instead of running underneath them.
pub fn beside(ui: &mut egui::Ui, reserve: f32, content: impl FnOnce(&mut egui::Ui)) {
    ui.vertical(|ui| {
        ui.set_max_width((ui.available_width() - reserve).max(120.0));
        content(ui);
    });
}

/// One line on the Settings page: icon, words, and its control on the right.
pub fn setting(
    ui: &mut egui::Ui,
    glyph: &str,
    title: &str,
    detail: &str,
    control: impl FnOnce(&mut egui::Ui),
) {
    setting_with(ui, 120.0, glyph, title, detail, control);
}

/// Like `setting`, for a control wider than a switch: `reserve` is the room it needs.
pub fn setting_with(
    ui: &mut egui::Ui,
    reserve: f32,
    glyph: &str,
    title: &str,
    detail: &str,
    control: impl FnOnce(&mut egui::Ui),
) {
    card().show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal(|ui| {
            ui.label(icon(glyph, 17.0, pal().soft));
            ui.add_space(6.0);
            beside(ui, reserve + 30.0, |ui| {
                ui.spacing_mut().item_spacing.y = 1.0;
                ui.label(RichText::new(title).size(14.0));
                ui.label(small(detail));
            });
            ui.with_layout(Layout::right_to_left(Align::Center), control);
        });
    });
    ui.add_space(4.0);
}

pub fn group_title(ui: &mut egui::Ui, text: &str) {
    ui.add_space(12.0);
    ui.label(RichText::new(text).font(bold(14.0)).color(pal().soft));
    ui.add_space(2.0);
}

pub fn hour_picker(ui: &mut egui::Ui, id: &str, value: &mut u32) -> bool {
    let mut changed = false;
    egui::ComboBox::from_id_salt(id)
        .selected_text(hour_text(*value))
        .width(84.0)
        .show_ui(ui, |ui| {
            for hour in 0..24 {
                changed |= ui.selectable_value(value, hour, hour_text(hour)).changed();
            }
        });
    changed
}

pub fn lerp_color(a: Color32, b: Color32, t: f32) -> Color32 {
    let mix = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t) as u8;
    Color32::from_rgb(mix(a.r(), b.r()), mix(a.g(), b.g()), mix(a.b(), b.b()))
}
