//! The fixed keymap. Like Hyprland's defaults, but deliberately not configurable.
//!
//! Mod is ⌥ Option on macOS and Alt elsewhere (⌘ belongs to macOS). Keys are matched by
//! physical position, so the bindings work on any keyboard layout.
//!
//! | Keys                  | Action                     |
//! |-----------------------|----------------------------|
//! | Mod+Enter             | new chart (dwindle split)  |
//! | Mod+Q                 | close chart                |
//! | Mod+H/J/K/L, arrows   | move focus                 |
//! | Mod+Shift+H/J/K/L     | swap chart                 |
//! | Mod+Ctrl+H/J/K/L      | move the divider           |
//! | Mod+T                 | toggle split direction     |
//! | Mod+F                 | fullscreen                 |

use iced::keyboard::{
    Modifiers,
    key::{Code, Physical},
};
use persephone_tiling::Direction;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    NewTile,
    CloseTile,
    Focus(Direction),
    Swap(Direction),
    Resize(Direction),
    ToggleSplit,
    Fullscreen,
}

impl Action {
    /// Whether holding the key down should keep repeating the action.
    fn repeats(self) -> bool {
        matches!(self, Self::Focus(_) | Self::Resize(_))
    }
}

/// The action bound to a key press, if any.
pub fn action(key: Physical, modifiers: Modifiers, repeat: bool) -> Option<Action> {
    if !modifiers.alt() || modifiers.logo() {
        return None;
    }
    let Physical::Code(code) = key else {
        return None;
    };

    let action = match direction(code) {
        Some(d) if modifiers.shift() => Action::Swap(d),
        Some(d) if modifiers.control() => Action::Resize(d),
        Some(d) => Action::Focus(d),
        None if modifiers.shift() || modifiers.control() => return None,
        None => match code {
            Code::Enter | Code::NumpadEnter => Action::NewTile,
            Code::KeyQ => Action::CloseTile,
            Code::KeyT => Action::ToggleSplit,
            Code::KeyF => Action::Fullscreen,
            _ => return None,
        },
    };
    (!repeat || action.repeats()).then_some(action)
}

fn direction(code: Code) -> Option<Direction> {
    match code {
        Code::KeyH | Code::ArrowLeft => Some(Direction::Left),
        Code::KeyJ | Code::ArrowDown => Some(Direction::Down),
        Code::KeyK | Code::ArrowUp => Some(Direction::Up),
        Code::KeyL | Code::ArrowRight => Some(Direction::Right),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn press(code: Code, modifiers: Modifiers) -> Option<Action> {
        action(Physical::Code(code), modifiers, false)
    }

    #[test]
    fn mod_bindings() {
        let alt = Modifiers::ALT;
        assert_eq!(press(Code::Enter, alt), Some(Action::NewTile));
        assert_eq!(press(Code::KeyQ, alt), Some(Action::CloseTile));
        assert_eq!(press(Code::KeyH, alt), Some(Action::Focus(Direction::Left)));
        assert_eq!(
            press(Code::ArrowDown, alt),
            Some(Action::Focus(Direction::Down))
        );
        assert_eq!(
            press(Code::KeyL, alt | Modifiers::SHIFT),
            Some(Action::Swap(Direction::Right))
        );
        assert_eq!(
            press(Code::KeyK, alt | Modifiers::CTRL),
            Some(Action::Resize(Direction::Up))
        );
        assert_eq!(press(Code::KeyT, alt), Some(Action::ToggleSplit));
        assert_eq!(press(Code::KeyF, alt), Some(Action::Fullscreen));
    }

    #[test]
    fn ignores_everything_else() {
        assert_eq!(
            press(Code::Enter, Modifiers::empty()),
            None,
            "Mod is required"
        );
        assert_eq!(
            press(Code::KeyQ, Modifiers::LOGO | Modifiers::ALT),
            None,
            "⌘ belongs to the OS"
        );
        assert_eq!(press(Code::KeyQ, Modifiers::ALT | Modifiers::SHIFT), None);
        assert_eq!(press(Code::KeyZ, Modifiers::ALT), None);
    }

    #[test]
    fn only_moves_repeat() {
        let held = |code, m| action(Physical::Code(code), m, true);
        assert_eq!(
            held(Code::KeyL, Modifiers::ALT),
            Some(Action::Focus(Direction::Right))
        );
        assert_eq!(
            held(Code::KeyL, Modifiers::ALT | Modifiers::CTRL),
            Some(Action::Resize(Direction::Right))
        );
        assert_eq!(
            held(Code::Enter, Modifiers::ALT),
            None,
            "holding Mod+Enter opens one chart"
        );
        assert_eq!(held(Code::KeyL, Modifiers::ALT | Modifiers::SHIFT), None);
    }
}
