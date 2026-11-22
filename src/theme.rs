//! Central place for colors. Retro "year 2000 web / Windows 98" palette:
//! silver bevels, navy title bars, teal desktop, blue underlined links.

#![allow(dead_code)]

use gpui::{rgb, rgba, BoxShadow, Rgba};

pub const CRUST: u32 = 0x008080; // desktop teal behind the panes
pub const BG: u32 = 0xffffff; // white content areas
pub const MANTLE: u32 = 0xc0c0c0; // silver "button face"
pub const SURFACE0: u32 = 0x808080; // dark gray edges
pub const SURFACE1: u32 = 0xffffff; // light edges / hover tint
pub const OVERLAY: u32 = 0x606060;
pub const SUBTEXT: u32 = 0x404040;
pub const SUBTEXT1: u32 = 0x000000;
pub const TEXT: u32 = 0x000000;
pub const ACCENT: u32 = 0x000080; // navy
pub const FOLDER: u32 = 0xb38600;
pub const ERROR: u32 = 0xcc0000;

pub const LINK: u32 = 0x0000ee; // classic hyperlink blue
pub const LINK_HOVER: u32 = 0xff0000;
pub const HILITE: u32 = 0xffff99; // yellow highlighter for selected rows
pub const HOVER_ROW: u32 = 0xe8e8ff;
pub const TITLE_ACTIVE: u32 = 0x000080;
pub const TITLE_INACTIVE: u32 = 0x808080;

pub const FONT: &str = "Verdana";
pub const FONT_MONO: &str = "Courier New";

/// `hex` with an alpha channel (0-255).
pub fn alpha(hex: u32, a: u8) -> Rgba {
    rgba((hex << 8) | a as u32)
}

fn edge(hex: u32, dx: f32, dy: f32) -> BoxShadow {
    BoxShadow {
        color: rgb(hex).into(),
        offset: gpui::point(gpui::px(dx), gpui::px(dy)),
        blur_radius: gpui::px(0.),
        spread_radius: gpui::px(0.),
    }
}

/// Raised 3D bevel (buttons, panels): light top-left, dark bottom-right.
pub fn raised() -> Vec<BoxShadow> {
    vec![edge(0xffffff, -1., -1.), edge(0x404040, 1., 1.)]
}

/// Sunken 3D bevel (text fields, pressed buttons).
pub fn sunken() -> Vec<BoxShadow> {
    vec![edge(0x404040, -1., -1.), edge(0xffffff, 1., 1.)]
}
