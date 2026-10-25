//! The fixed dark palette, modeled on TradingView's.

use iced::Color;

pub const BACKGROUND: Color = Color::from_rgb8(0x13, 0x17, 0x22);
pub const GRID: Color = Color::from_rgb8(0x1e, 0x22, 0x2d);
pub const GRID_MAJOR: Color = Color::from_rgb8(0x24, 0x29, 0x36);
pub const AXIS_LINE: Color = Color::from_rgb8(0x2a, 0x2e, 0x39);
pub const TEXT: Color = Color::from_rgb8(0xb2, 0xb5, 0xbe);
pub const TEXT_MUTED: Color = Color::from_rgb8(0x78, 0x7b, 0x86);
pub const TEXT_BRIGHT: Color = Color::from_rgb8(0xd1, 0xd4, 0xdc);

pub const UP: Color = Color::from_rgb8(0x26, 0xa6, 0x9a);
pub const DOWN: Color = Color::from_rgb8(0xef, 0x53, 0x50);
pub const UP_VOLUME: Color = Color::from_rgba8(0x26, 0xa6, 0x9a, 0.45);
pub const DOWN_VOLUME: Color = Color::from_rgba8(0xef, 0x53, 0x50, 0.45);

pub const CROSSHAIR: Color = Color::from_rgb8(0x75, 0x86, 0x96);
pub const LABEL_BACKGROUND: Color = Color::from_rgb8(0x36, 0x3a, 0x45);
pub const LABEL_TEXT: Color = Color::WHITE;

/// Label font size, in logical pixels.
pub const TEXT_SIZE: f32 = 11.0;
/// Legend font size.
pub const LEGEND_SIZE: f32 = 13.0;
