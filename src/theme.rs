//! Colours, lifted from the key art so the launcher and the login screen look
//! like the same game rather than like a utility that ships beside it.

use eframe::egui::Color32;

/// Night sky behind the city.
pub const NIGHT: Color32 = Color32::from_rgb(0x0d, 0x1b, 0x24);
/// Moonlit haze -- panel fills.
pub const HAZE: Color32 = Color32::from_rgb(0x14, 0x2b, 0x38);
/// Lantern light. The one warm colour, so it is the one that draws the eye.
pub const EMBER: Color32 = Color32::from_rgb(0xe8, 0x9c, 0x3f);
pub const EMBER_BRIGHT: Color32 = Color32::from_rgb(0xff, 0xbe, 0x6b);
/// The arcane blue on the wolf's blade.
pub const ARCANE: Color32 = Color32::from_rgb(0x7e, 0xd4, 0xf0);

pub const TEXT: Color32 = Color32::from_rgb(0xe6, 0xef, 0xf3);
pub const TEXT_DIM: Color32 = Color32::from_rgb(0x9a, 0xb2, 0xbd);

/// Scrim over the art. The background is busy and full of light sources, and
/// text laid straight onto it is legible in some places and not in others.
pub fn scrim(alpha: u8) -> Color32 {
    Color32::from_rgba_unmultiplied(0x06, 0x10, 0x16, alpha)
}

pub fn panel(alpha: u8) -> Color32 {
    Color32::from_rgba_unmultiplied(0x0a, 0x1a, 0x23, alpha)
}
