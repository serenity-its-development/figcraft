//! OneOS: what the editor OFFERS on a compose page (Mona 2026-10-27: FigCraft IS the Pages & apps editor). The engine refuses a raw
//! spacing, colour, type size or radius beneath a page frame by name; these controls mean a person never gets that far. A spacing,
//! radius or type field is a menu of the theme's tokens (name and px) instead of a free number, a handle drag lands on the nearest
//! token (`Session::compose_snap`) and a colour is chosen from the theme's roles ([`role_swatches`]).

use egui::{Color32, CornerRadius, Id, Sense, Stroke, StrokeKind, Ui, vec2};

use crate::i18n::tr;
use crate::theme::Tokens;
use crate::widgets::{self, Lead};

/// "md 16" for a value on the scale, "20 (off the scale)" for one that is not (a layer drawn before the tokens were stated), the empty text for none.
pub fn scale_label(scale: &[(String, f64)], value: Option<f64>, empty: &str) -> String {
    match value {
        Some(v) => match scale.iter().find(|(_, x)| (x - v).abs() < 1e-6) {
            Some((k, x)) => format!("{k}  {}", trim(*x)),
            None => format!("{} ({})", trim(v), tr("off the scale")),
        },
        None => empty.to_string(),
    }
}

fn trim(v: f64) -> String {
    if v.fract() == 0.0 { format!("{v:.0}") } else { format!("{v}") }
}

/// A token menu standing where a number field would: the picked token's px, or `None`.
pub fn scale_field(ui: &mut Ui, scale: &[(String, f64)], value: Option<f64>, width: f32, lead: &Lead, empty: &str) -> Option<f64> {
    let label = scale_label(scale, value, empty);
    let icon = match lead {
        Lead::Icon(i, _) => Some(*i),
        _ => None,
    };
    let resp = widgets::dropdown(ui, &label, width, icon);
    let mut picked = None;
    crate::right::menu_popup(&resp).show(|ui| {
        for (k, x) in scale {
            let mark = if value.is_some_and(|v| (v - x).abs() < 1e-6) { "✓ " } else { "   " };
            if crate::right::menu_item(ui, &format!("{mark}{k}  {}", trim(*x)), "") {
                picked = Some(*x);
                ui.close();
            }
        }
    });
    picked
}

/// The theme's colour roles as swatches (name under each); the role's `#RRGGBB` clicked, or `None`.
pub fn role_swatches(ui: &mut Ui, roles: &[(String, String)], current: Option<&str>) -> Option<String> {
    let t = Tokens::get(ui.ctx());
    let mut picked = None;
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = vec2(8.0, 8.0);
        for (role, hex) in roles {
            let colour = parse_hex(hex).unwrap_or(Color32::GRAY);
            let (rect, resp) = ui.allocate_exact_size(vec2(64.0, 52.0), Sense::click());
            let swatch = egui::Rect::from_min_size(rect.min, vec2(64.0, 30.0));
            let on = current.is_some_and(|c| c.eq_ignore_ascii_case(hex));
            ui.painter().rect_filled(swatch, CornerRadius::same(5), colour);
            ui.painter().rect_stroke(
                swatch,
                CornerRadius::same(5),
                Stroke::new(if on { 2.0 } else { 1.0 }, if on { crate::theme::ACCENT } else { t.field_hover_border }),
                StrokeKind::Inside,
            );
            ui.painter().text(
                egui::pos2(rect.center().x, rect.max.y - 10.0),
                egui::Align2::CENTER_CENTER,
                role,
                crate::theme::regular(10.0),
                t.text_secondary,
            );
            crate::a11y::text(&resp, role, hex);
            if resp.clicked() {
                picked = Some(hex.clone());
            }
            let _ = Id::new(role);
        }
    });
    picked
}

fn parse_hex(h: &str) -> Option<Color32> {
    let t = h.trim().trim_start_matches('#');
    (t.len() == 6).then(|| u32::from_str_radix(t, 16).ok()).flatten().map(|n| Color32::from_rgb((n >> 16) as u8, (n >> 8) as u8, n as u8))
}

/// The heading above the role swatches.
pub fn roles_title() -> &'static str {
    tr("Theme colours")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scale() -> Vec<(String, f64)> {
        vec![("none".into(), 0.0), ("sm".into(), 8.0), ("md".into(), 16.0)]
    }

    #[test]
    fn a_value_is_shown_by_its_token() {
        assert_eq!(scale_label(&scale(), Some(16.0), ""), "md  16");
        assert_eq!(scale_label(&scale(), Some(0.0), ""), "none  0");
        assert_eq!(scale_label(&scale(), None, "Mixed"), "Mixed");
        assert!(scale_label(&scale(), Some(20.0), "").starts_with("20 ("));
    }

    #[test]
    fn a_role_is_a_colour() {
        assert_eq!(parse_hex("#3D5AFE"), Some(Color32::from_rgb(0x3D, 0x5A, 0xFE)));
        assert_eq!(parse_hex("nope"), None);
    }
}
