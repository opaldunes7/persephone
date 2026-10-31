//! The fixed keymap. Like Hyprland's defaults, but deliberately not configurable.
//!
//! Every command is Control plus a key (⌘ belongs to macOS). Keys are matched by physical
//! position, so the bindings work on any keyboard layout.
//!
//! | Keys                  | Action                     |
//! |-----------------------|----------------------------|
//! | Ctrl+Enter             | new chart (dwindle split)  |
//! | Ctrl+Q                 | close chart                |
//! | Ctrl+H/J/K/L, arrows   | move focus                 |
//! | Ctrl+Shift+H/J/K/L     | swap chart                 |
//! | Ctrl+Alt+H/J/K/L       | move the divider           |
//! | Ctrl+T                 | toggle split direction     |
//! | Ctrl+F                 | fullscreen                 |
//! | Ctrl+B                 | backtest panel, open/close |
//! | Ctrl+1…9               | switch workspace           |
//! | Ctrl+Shift+1…9         | move chart to workspace    |

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
    /// Open the backtest panel on the focused chart, or close it.
    Backtest,
    /// Show workspace 1 to 9.
    Workspace(u8),
    /// Move the focused chart to workspace 1 to 9.
    MoveToWorkspace(u8),
}

impl Action {
    /// Whether holding the key down should keep repeating the action.
    fn repeats(self) -> bool {
        matches!(self, Self::Focus(_) | Self::Resize(_))
    }
}

/// The action bound to a key press, if any.
pub fn action(key: Physical, modifiers: Modifiers, repeat: bool) -> Option<Action> {
    if !modifiers.control() || modifiers.logo() {
        return None;
    }
    let Physical::Code(code) = key else {
        return None;
    };
    if let Some(n) = digit(code) {
        let action = match (modifiers.shift(), modifiers.alt()) {
            (false, false) => Action::Workspace(n),
            (true, false) => Action::MoveToWorkspace(n),
            _ => return None,
        };
        return (!repeat).then_some(action);
    }

    let action = match direction(code) {
        Some(d) if modifiers.shift() => Action::Swap(d),
        Some(d) if modifiers.alt() => Action::Resize(d),
        Some(d) => Action::Focus(d),
        None if modifiers.shift() || modifiers.alt() => return None,
        None => match code {
            Code::Enter | Code::NumpadEnter => Action::NewTile,
            Code::KeyQ => Action::CloseTile,
            Code::KeyT => Action::ToggleSplit,
            Code::KeyF => Action::Fullscreen,
            Code::KeyB => Action::Backtest,
            _ => return None,
        },
    };
    (!repeat || action.repeats()).then_some(action)
}

fn digit(code: Code) -> Option<u8> {
    let n = match code {
        Code::Digit1 => 1,
        Code::Digit2 => 2,
        Code::Digit3 => 3,
        Code::Digit4 => 4,
        Code::Digit5 => 5,
        Code::Digit6 => 6,
        Code::Digit7 => 7,
        Code::Digit8 => 8,
        Code::Digit9 => 9,
        _ => return None,
    };
    Some(n)
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
    fn control_bindings() {
        let ctrl = Modifiers::CTRL;
        assert_eq!(press(Code::Enter, ctrl), Some(Action::NewTile));
        assert_eq!(press(Code::KeyQ, ctrl), Some(Action::CloseTile));
        assert_eq!(press(Code::KeyB, ctrl), Some(Action::Backtest));
        assert_eq!(
            press(Code::KeyH, ctrl),
            Some(Action::Focus(Direction::Left))
        );
        assert_eq!(
            press(Code::ArrowDown, ctrl),
            Some(Action::Focus(Direction::Down))
        );
        assert_eq!(
            press(Code::KeyL, ctrl | Modifiers::SHIFT),
            Some(Action::Swap(Direction::Right))
        );
        assert_eq!(
            press(Code::KeyK, ctrl | Modifiers::ALT),
            Some(Action::Resize(Direction::Up))
        );
        assert_eq!(press(Code::KeyT, ctrl), Some(Action::ToggleSplit));
        assert_eq!(press(Code::KeyF, ctrl), Some(Action::Fullscreen));
        assert_eq!(press(Code::Digit3, ctrl), Some(Action::Workspace(3)));
        assert_eq!(
            press(Code::Digit9, ctrl | Modifiers::SHIFT),
            Some(Action::MoveToWorkspace(9))
        );
        assert_eq!(press(Code::Digit0, ctrl), None, "workspaces are 1 to 9");
    }

    #[test]
    fn ignores_everything_else() {
        assert_eq!(
            press(Code::Enter, Modifiers::empty()),
            None,
            "Control is required"
        );
        assert_eq!(
            press(Code::KeyQ, Modifiers::LOGO | Modifiers::CTRL),
            None,
            "⌘ belongs to the OS"
        );
        assert_eq!(press(Code::KeyQ, Modifiers::CTRL | Modifiers::SHIFT), None);
        assert_eq!(press(Code::KeyZ, Modifiers::CTRL), None);
        assert_eq!(
            press(Code::Enter, Modifiers::ALT),
            None,
            "Option alone no longer does anything"
        );
        assert_eq!(
            press(Code::Digit3, Modifiers::CTRL | Modifiers::ALT),
            None,
            "workspaces take Shift to move, nothing else"
        );
    }

    #[test]
    fn only_moves_repeat() {
        let held = |code, m| action(Physical::Code(code), m, true);
        assert_eq!(
            held(Code::KeyL, Modifiers::CTRL),
            Some(Action::Focus(Direction::Right))
        );
        assert_eq!(
            held(Code::KeyL, Modifiers::CTRL | Modifiers::ALT),
            Some(Action::Resize(Direction::Right))
        );
        assert_eq!(
            held(Code::Enter, Modifiers::CTRL),
            None,
            "holding Ctrl+Enter opens one chart"
        );
        assert_eq!(held(Code::KeyL, Modifiers::CTRL | Modifiers::SHIFT), None);
        assert_eq!(held(Code::Digit2, Modifiers::CTRL), None);
    }
}
