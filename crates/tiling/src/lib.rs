//! Hyprland-style "dwindle" tiling, as pure data.
//!
//! The layout is a binary tree: each split divides its rectangle between two children along
//! one axis, and each leaf is a tile. A new tile splits the focused one along its longer
//! side, so tiles spiral inwards the way Hyprland's dwindle layout does. Unlike Hyprland, a
//! split that would leave a chart too small to read ([`MIN_TILE_WIDTH`] ×
//! [`MIN_TILE_HEIGHT`]) goes to the largest tile instead, so 16 tiles end as a grid rather
//! than a spiral of slivers. Nothing here knows about the UI; the app turns
//! [`Layout::arrange`] into widgets.
//!
//! The behaviour is deliberately fixed: there are no options to configure.

use serde::{Deserialize, Serialize};

/// Most tiles a layout holds. Rendering is cheap; this keeps charts readable.
pub const MAX_TILES: usize = 16;
/// Smallest share of a split either child keeps when resizing.
pub const MIN_RATIO: f32 = 0.1;
/// Splitting the focused tile must leave halves at least this wide...
pub const MIN_TILE_WIDTH: f32 = 320.0;
/// ...and this tall, or the largest tile is split instead.
pub const MIN_TILE_HEIGHT: f32 = 200.0;

const EPSILON: f32 = 0.5;

pub type TileId = u32;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl Rect {
    #[must_use]
    pub const fn new(x: f32, y: f32, width: f32, height: f32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    #[must_use]
    pub fn right(&self) -> f32 {
        self.x + self.width
    }

    #[must_use]
    pub fn bottom(&self) -> f32 {
        self.y + self.height
    }

    #[must_use]
    pub fn center(&self) -> (f32, f32) {
        (self.x + self.width / 2.0, self.y + self.height / 2.0)
    }

    #[must_use]
    pub fn area(&self) -> f32 {
        self.width * self.height
    }

    /// Shrinks the rectangle by `by` on every side.
    #[must_use]
    pub fn inset(&self, by: f32) -> Self {
        Self {
            x: self.x + by,
            y: self.y + by,
            width: (self.width - 2.0 * by).max(0.0),
            height: (self.height - 2.0 * by).max(0.0),
        }
    }

    /// Linear interpolation towards `to`; `t` = 0 is `self`, 1 is `to`.
    #[must_use]
    pub fn lerp(&self, to: &Self, t: f32) -> Self {
        let mix = |a: f32, b: f32| a + (b - a) * t;
        Self {
            x: mix(self.x, to.x),
            y: mix(self.y, to.y),
            width: mix(self.width, to.width),
            height: mix(self.height, to.height),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Axis {
    /// Children side by side.
    Horizontal,
    /// Children stacked.
    Vertical,
}

impl Axis {
    fn flipped(self) -> Self {
        match self {
            Self::Horizontal => Self::Vertical,
            Self::Vertical => Self::Horizontal,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    Left,
    Right,
    Up,
    Down,
}

impl Direction {
    fn axis(self) -> Axis {
        match self {
            Self::Left | Self::Right => Axis::Horizontal,
            Self::Up | Self::Down => Axis::Vertical,
        }
    }

    /// Right and down move away from the origin.
    fn is_forward(self) -> bool {
        matches!(self, Self::Right | Self::Down)
    }
}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    #[error("a layout holds at most {MAX_TILES} tiles")]
    Full,
    #[error("the last tile can't be closed")]
    LastTile,
    #[error("no tile {0}")]
    NoSuchTile(TileId),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
enum Node {
    Leaf(TileId),
    Split {
        axis: Axis,
        /// Share of the rectangle that goes to the first child.
        ratio: f32,
        children: Box<[Node; 2]>,
    },
}

impl Node {
    fn path_to(&self, id: TileId, path: &mut Vec<usize>) -> bool {
        match self {
            Self::Leaf(leaf) => *leaf == id,
            Self::Split { children, .. } => (0..2).any(|side| {
                path.push(side);
                let found = children[side].path_to(id, path);
                if !found {
                    path.pop();
                }
                found
            }),
        }
    }

    fn at_mut(&mut self, path: &[usize]) -> &mut Self {
        match (path.split_first(), self) {
            (None, node) => node,
            (Some((side, rest)), Self::Split { children, .. }) => children[*side].at_mut(rest),
            (Some(_), Self::Leaf(_)) => unreachable!("path runs past a leaf"),
        }
    }

    fn leaves(&self, out: &mut Vec<TileId>) {
        match self {
            Self::Leaf(id) => out.push(*id),
            Self::Split { children, .. } => children.iter().for_each(|c| c.leaves(out)),
        }
    }

    /// The leaf of this subtree nearest to its `side` edge (0 = first, 1 = last).
    fn edge_leaf(&self, side: usize) -> TileId {
        match self {
            Self::Leaf(id) => *id,
            Self::Split { children, .. } => children[side].edge_leaf(side),
        }
    }

    fn arrange(&self, rect: Rect, gap: f32, out: &mut Vec<(TileId, Rect)>) {
        match self {
            Self::Leaf(id) => out.push((*id, rect)),
            Self::Split {
                axis,
                ratio,
                children,
            } => {
                let (a, b) = split(rect, *axis, *ratio, gap);
                children[0].arrange(a, gap, out);
                children[1].arrange(b, gap, out);
            }
        }
    }

    fn ratios_valid(&self) -> bool {
        match self {
            Self::Leaf(_) => true,
            Self::Split {
                ratio, children, ..
            } => {
                (MIN_RATIO..=1.0 - MIN_RATIO).contains(ratio)
                    && children.iter().all(Self::ratios_valid)
            }
        }
    }
}

/// Splits `rect` at `ratio`, leaving `gap` pixels between the halves. The first half's size is
/// rounded to whole pixels so tile edges stay crisp.
fn split(rect: Rect, axis: Axis, ratio: f32, gap: f32) -> (Rect, Rect) {
    match axis {
        Axis::Horizontal => {
            let room = (rect.width - gap).max(0.0);
            let first = (room * ratio).round();
            (
                Rect::new(rect.x, rect.y, first, rect.height),
                Rect::new(rect.x + first + gap, rect.y, room - first, rect.height),
            )
        }
        Axis::Vertical => {
            let room = (rect.height - gap).max(0.0);
            let first = (room * ratio).round();
            (
                Rect::new(rect.x, rect.y, rect.width, first),
                Rect::new(rect.x, rect.y + first + gap, rect.width, room - first),
            )
        }
    }
}

/// Side by side for wide rectangles, stacked for tall ones.
fn longer_axis(rect: Rect) -> Axis {
    if rect.width >= rect.height {
        Axis::Horizontal
    } else {
        Axis::Vertical
    }
}

/// Whether splitting `rect` along its longer side leaves halves big enough for a chart.
fn half_is_readable(rect: Rect) -> bool {
    let (width, height) = match longer_axis(rect) {
        Axis::Horizontal => (rect.width / 2.0, rect.height),
        Axis::Vertical => (rect.width, rect.height / 2.0),
    };
    width >= MIN_TILE_WIDTH && height >= MIN_TILE_HEIGHT
}

/// Length of the overlap between `[a0, a1]` and `[b0, b1]`.
fn overlap(a0: f32, a1: f32, b0: f32, b1: f32) -> f32 {
    (a1.min(b1) - a0.max(b0)).max(0.0)
}

/// The tiling state: the split tree, which tile has focus, and whether it is fullscreen.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Layout {
    root: Node,
    focused: TileId,
    fullscreen: bool,
    next_id: TileId,
}

impl Default for Layout {
    fn default() -> Self {
        Self::new()
    }
}

impl Layout {
    /// One tile, id 0.
    #[must_use]
    pub fn new() -> Self {
        Self {
            root: Node::Leaf(0),
            focused: 0,
            fullscreen: false,
            next_id: 1,
        }
    }

    /// Tile ids in tree order.
    #[must_use]
    pub fn tiles(&self) -> Vec<TileId> {
        let mut out = Vec::new();
        self.root.leaves(&mut out);
        out
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.tiles().len()
    }

    /// Always false: a layout keeps at least one tile.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        false
    }

    #[must_use]
    pub fn focused(&self) -> TileId {
        self.focused
    }

    #[must_use]
    pub fn is_fullscreen(&self) -> bool {
        self.fullscreen
    }

    #[must_use]
    pub fn contains(&self, id: TileId) -> bool {
        self.path(id).is_some()
    }

    /// Each tile's rectangle within `area`, with `gap` pixels between neighbours, in tree
    /// order. Fullscreen is not applied here; see [`Layout::is_fullscreen`].
    #[must_use]
    pub fn arrange(&self, area: Rect, gap: f32) -> Vec<(TileId, Rect)> {
        let mut out = Vec::new();
        self.root.arrange(area, gap, &mut out);
        out
    }

    /// Splits the focused tile along its longer side and focuses the new tile, which takes
    /// the right or bottom half. If the halves would be smaller than [`MIN_TILE_WIDTH`] ×
    /// [`MIN_TILE_HEIGHT`], the largest tile is split instead.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Full`] at [`MAX_TILES`].
    pub fn insert(&mut self, area: Rect) -> Result<TileId, Error> {
        if self.len() >= MAX_TILES {
            return Err(Error::Full);
        }
        let rects = self.arrange(area, 0.0);
        let focused = rects.iter().find(|(id, _)| *id == self.focused).copied();
        let (target, rect) = match focused {
            Some((_, rect)) if half_is_readable(rect) => focused.unwrap_or((self.focused, area)),
            _ => rects
                .iter()
                .copied()
                .reduce(|best, next| {
                    if next.1.area() > best.1.area() + EPSILON {
                        next
                    } else {
                        best
                    }
                })
                .unwrap_or((self.focused, area)),
        };
        let axis = longer_axis(rect);

        let id = self.next_id;
        self.next_id += 1;
        let path = self.path(target).ok_or(Error::NoSuchTile(target))?;
        let node = self.root.at_mut(&path);
        let old = std::mem::replace(node, Node::Leaf(id));
        *node = Node::Split {
            axis,
            ratio: 0.5,
            children: Box::new([old, Node::Leaf(id)]),
        };

        self.focused = id;
        self.fullscreen = false;
        Ok(id)
    }

    /// Removes a tile; its sibling takes the freed space. If it had focus, the neighbouring
    /// tile on that side gets it.
    ///
    /// # Errors
    ///
    /// Returns [`Error::LastTile`] for the only tile and [`Error::NoSuchTile`] for an
    /// unknown id.
    pub fn close(&mut self, id: TileId) -> Result<(), Error> {
        let path = self.path(id).ok_or(Error::NoSuchTile(id))?;
        let Some((side, parent_path)) = path.split_last() else {
            return Err(Error::LastTile);
        };
        let parent = self.root.at_mut(parent_path);
        let Node::Split { children, .. } = parent else {
            unreachable!("a leaf's parent is a split");
        };
        let sibling = std::mem::replace(&mut children[1 - side], Node::Leaf(id));
        // The closed tile sat on `side` of its sibling, so focus the sibling's leaf facing it.
        let next = sibling.edge_leaf(*side);
        *parent = sibling;

        if self.focused == id {
            self.focused = next;
            self.fullscreen = false;
        }
        Ok(())
    }

    /// Focuses a tile directly (e.g. when the mouse enters it).
    pub fn focus(&mut self, id: TileId) -> bool {
        if self.focused == id || !self.contains(id) {
            return false;
        }
        self.focused = id;
        self.fullscreen = false;
        true
    }

    /// Moves focus to the nearest tile in `direction`. Returns whether focus moved.
    pub fn focus_towards(&mut self, direction: Direction, area: Rect) -> bool {
        match self.neighbour(direction, area) {
            Some(id) => self.focus(id),
            None => false,
        }
    }

    /// Swaps the focused tile with its neighbour in `direction`; focus follows the tile.
    pub fn swap_towards(&mut self, direction: Direction, area: Rect) -> bool {
        let Some(other) = self.neighbour(direction, area) else {
            return false;
        };
        let (Some(a), Some(b)) = (self.path(self.focused), self.path(other)) else {
            return false;
        };
        *self.root.at_mut(&a) = Node::Leaf(other);
        *self.root.at_mut(&b) = Node::Leaf(self.focused);
        self.fullscreen = false;
        true
    }

    /// Moves the nearest divider of the focused tile that runs across `direction`'s axis by
    /// `step` (a fraction of that split) in `direction`.
    pub fn resize(&mut self, direction: Direction, step: f32) -> bool {
        let Some(mut path) = self.path(self.focused) else {
            return false;
        };
        while !path.is_empty() {
            path.pop();
            if let Node::Split { axis, ratio, .. } = self.root.at_mut(&path)
                && *axis == direction.axis()
            {
                let delta = if direction.is_forward() { step } else { -step };
                let new = (*ratio + delta).clamp(MIN_RATIO, 1.0 - MIN_RATIO);
                let moved = (new - *ratio).abs() > f32::EPSILON;
                *ratio = new;
                return moved;
            }
        }
        false
    }

    /// Flips the axis of the split that holds the focused tile.
    pub fn toggle_split(&mut self) -> bool {
        let Some(mut path) = self.path(self.focused) else {
            return false;
        };
        if path.pop().is_none() {
            return false;
        }
        if let Node::Split { axis, .. } = self.root.at_mut(&path) {
            *axis = axis.flipped();
        }
        true
    }

    pub fn toggle_fullscreen(&mut self) {
        self.fullscreen = !self.fullscreen;
    }

    /// Checks a layout from outside (e.g. a saved file): unique ids, a focused tile that
    /// exists, ratios in range, at most [`MAX_TILES`] tiles and fresh ids above all others.
    #[must_use]
    pub fn is_valid(&self) -> bool {
        let mut tiles = self.tiles();
        let len = tiles.len();
        tiles.sort_unstable();
        tiles.dedup();
        tiles.len() == len
            && len <= MAX_TILES
            && tiles.contains(&self.focused)
            && tiles.iter().all(|id| *id < self.next_id)
            && self.root.ratios_valid()
    }

    fn path(&self, id: TileId) -> Option<Vec<usize>> {
        let mut path = Vec::new();
        self.root.path_to(id, &mut path).then_some(path)
    }

    #[cfg(test)]
    fn rect(&self, id: TileId, area: Rect) -> Option<Rect> {
        self.arrange(area, 0.0)
            .into_iter()
            .find_map(|(tile, rect)| (tile == id).then_some(rect))
    }

    /// The tile next to the focused one in `direction`: among tiles beyond that edge, prefer
    /// those sharing some of its span, then the closest edge, then the closest center.
    fn neighbour(&self, direction: Direction, area: Rect) -> Option<TileId> {
        let rects = self.arrange(area, 0.0);
        let current = rects.iter().find(|(id, _)| *id == self.focused)?.1;
        let (cx, cy) = current.center();

        rects
            .iter()
            .filter(|(id, _)| *id != self.focused)
            .filter_map(|(id, r)| {
                let (gap, shared, off_center) = match direction {
                    Direction::Right => (
                        r.x - current.right(),
                        overlap(current.y, current.bottom(), r.y, r.bottom()),
                        (r.center().1 - cy).abs(),
                    ),
                    Direction::Left => (
                        current.x - r.right(),
                        overlap(current.y, current.bottom(), r.y, r.bottom()),
                        (r.center().1 - cy).abs(),
                    ),
                    Direction::Down => (
                        r.y - current.bottom(),
                        overlap(current.x, current.right(), r.x, r.right()),
                        (r.center().0 - cx).abs(),
                    ),
                    Direction::Up => (
                        current.y - r.bottom(),
                        overlap(current.x, current.right(), r.x, r.right()),
                        (r.center().0 - cx).abs(),
                    ),
                };
                (gap > -EPSILON).then_some((*id, shared <= EPSILON, gap, off_center))
            })
            .min_by(|a, b| {
                a.1.cmp(&b.1)
                    .then(a.2.total_cmp(&b.2))
                    .then(a.3.total_cmp(&b.3))
            })
            .map(|(id, ..)| id)
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    const WIDE: Rect = Rect::new(0.0, 0.0, 1600.0, 900.0);

    fn ids_at(layout: &Layout, area: Rect) -> Vec<(TileId, (f32, f32, f32, f32))> {
        layout
            .arrange(area, 0.0)
            .into_iter()
            .map(|(id, r)| (id, (r.x, r.y, r.width, r.height)))
            .collect()
    }

    #[test]
    fn tiles_dwindle_inwards() {
        let mut layout = Layout::new();
        assert_eq!(layout.insert(WIDE), Ok(1));
        // Wide screen: the second tile goes to the right.
        assert_eq!(
            ids_at(&layout, WIDE),
            vec![
                (0, (0.0, 0.0, 800.0, 900.0)),
                (1, (800.0, 0.0, 800.0, 900.0))
            ]
        );
        // The right half is tall: the third tile goes below it.
        assert_eq!(layout.insert(WIDE), Ok(2));
        assert_eq!(ids_at(&layout, WIDE)[2], (2, (800.0, 450.0, 800.0, 450.0)));
        // That quarter is wide again: the fourth goes to its right.
        assert_eq!(layout.insert(WIDE), Ok(3));
        assert_eq!(ids_at(&layout, WIDE)[3], (3, (1200.0, 450.0, 400.0, 450.0)));
        assert_eq!(layout.focused(), 3);
    }

    #[test]
    fn sixteen_tiles_end_as_a_grid_not_a_spiral() {
        let mut layout = Layout::new();
        for _ in 1..MAX_TILES {
            layout.insert(WIDE).unwrap();
        }
        let sizes: Vec<_> = ids_at(&layout, WIDE)
            .into_iter()
            .map(|(_, r)| (r.2, r.3))
            .collect();
        assert!(sizes.iter().all(|s| *s == (400.0, 225.0)), "{sizes:?}");
    }

    #[test]
    fn small_focused_tiles_hand_the_split_to_the_largest() {
        let mut layout = Layout::new();
        for _ in 0..4 {
            layout.insert(WIDE).unwrap(); // spiral: 800x900, 800x450, 400x450, 400x225
        }
        assert_eq!(ids_at(&layout, WIDE)[4].1, (1200.0, 675.0, 400.0, 225.0));
        // Halving a 400x225 tile would leave 200px-wide charts, so the largest tile (the tall
        // 800x900 one on the left) splits instead, top and bottom.
        let id = layout.insert(WIDE).unwrap();
        let rect = ids_at(&layout, WIDE)
            .into_iter()
            .find(|(t, _)| *t == id)
            .unwrap()
            .1;
        assert_eq!(rect, (0.0, 450.0, 800.0, 450.0));
        assert_eq!(layout.focused(), id);
    }

    #[test]
    fn caps_at_max_tiles() {
        let mut layout = Layout::new();
        for _ in 1..MAX_TILES {
            layout.insert(WIDE).unwrap();
        }
        assert_eq!(layout.len(), MAX_TILES);
        assert_eq!(layout.insert(WIDE), Err(Error::Full));
        assert_eq!(layout.len(), MAX_TILES);
    }

    #[test]
    fn closing_gives_space_and_focus_to_the_neighbour() {
        let mut layout = Layout::new();
        layout.insert(WIDE).unwrap(); // 1 right of 0
        layout.insert(WIDE).unwrap(); // 2 below 1
        layout.close(2).unwrap();
        assert_eq!(layout.focused(), 1);
        assert_eq!(ids_at(&layout, WIDE)[1], (1, (800.0, 0.0, 800.0, 900.0)));

        layout.close(0).unwrap();
        assert_eq!(ids_at(&layout, WIDE), vec![(1, (0.0, 0.0, 1600.0, 900.0))]);
        assert_eq!(layout.close(1), Err(Error::LastTile));
        assert_eq!(layout.close(7), Err(Error::NoSuchTile(7)));
    }

    #[test]
    fn focus_moves_by_direction() {
        let mut layout = Layout::new();
        layout.insert(WIDE).unwrap(); // 1: right half
        layout.insert(WIDE).unwrap(); // 2: bottom-right
        assert!(layout.focus_towards(Direction::Up, WIDE));
        assert_eq!(layout.focused(), 1);
        assert!(layout.focus_towards(Direction::Left, WIDE));
        assert_eq!(layout.focused(), 0);
        assert!(
            !layout.focus_towards(Direction::Left, WIDE),
            "nothing further left"
        );
        // Going right from the full-height left tile picks the nearer-centred tile on top.
        assert!(layout.focus_towards(Direction::Right, WIDE));
        assert_eq!(layout.focused(), 1);
        assert!(layout.focus_towards(Direction::Down, WIDE));
        assert_eq!(layout.focused(), 2);
    }

    #[test]
    fn swap_moves_the_focused_tile() {
        let mut layout = Layout::new();
        layout.insert(WIDE).unwrap();
        layout.focus(0);
        assert!(layout.swap_towards(Direction::Right, WIDE));
        assert_eq!(layout.focused(), 0);
        assert_eq!(ids_at(&layout, WIDE)[1], (0, (800.0, 0.0, 800.0, 900.0)));
        assert!(!layout.swap_towards(Direction::Right, WIDE));
    }

    #[test]
    fn resize_moves_the_divider_and_clamps() {
        let mut layout = Layout::new();
        layout.insert(WIDE).unwrap(); // focused: right half
        assert!(layout.resize(Direction::Left, 0.1));
        assert_eq!(ids_at(&layout, WIDE)[1].1, (640.0, 0.0, 960.0, 900.0));
        assert!(!layout.resize(Direction::Up, 0.1), "no vertical divider");
        for _ in 0..20 {
            layout.resize(Direction::Left, 0.1);
        }
        let left = ids_at(&layout, WIDE)[0].1;
        assert_eq!(left.2, (1600.0 * MIN_RATIO).round());
        assert!(!layout.resize(Direction::Left, 0.1), "already at the limit");
    }

    #[test]
    fn toggle_split_and_fullscreen() {
        let mut layout = Layout::new();
        assert!(!layout.toggle_split(), "a lone tile has no split");
        layout.insert(WIDE).unwrap();
        assert!(layout.toggle_split());
        assert_eq!(ids_at(&layout, WIDE)[1].1, (0.0, 450.0, 1600.0, 450.0));

        layout.toggle_fullscreen();
        assert!(layout.is_fullscreen());
        layout.focus(0);
        assert!(!layout.is_fullscreen(), "changing focus leaves fullscreen");
    }

    #[test]
    fn gaps_separate_tiles() {
        let mut layout = Layout::new();
        layout.insert(WIDE).unwrap();
        let rects = layout.arrange(WIDE, 8.0);
        assert_eq!(rects[1].1.x - rects[0].1.right(), 8.0);
    }

    #[test]
    fn round_trips_through_serde() {
        let mut layout = Layout::new();
        layout.insert(WIDE).unwrap();
        layout.insert(WIDE).unwrap();
        layout.resize(Direction::Up, 0.2);
        let json = serde_json::to_string(&layout).unwrap();
        let back: Layout = serde_json::from_str(&json).unwrap();
        assert_eq!(back, layout);
        assert!(back.is_valid());
    }

    #[test]
    fn rejects_invalid_layouts() {
        let bad: Layout = serde_json::from_str(
            r#"{"root":{"Split":{"axis":"Horizontal","ratio":0.5,"children":[{"Leaf":3},{"Leaf":3}]}},"focused":3,"fullscreen":false,"next_id":4}"#,
        )
        .unwrap();
        assert!(!bad.is_valid(), "duplicate ids");
    }

    #[derive(Clone, Debug)]
    enum Op {
        Insert,
        Close(usize),
        Focus(Direction),
        Swap(Direction),
        Resize(Direction),
        ToggleSplit,
        Fullscreen,
    }

    fn direction() -> impl Strategy<Value = Direction> {
        prop_oneof![
            Just(Direction::Left),
            Just(Direction::Right),
            Just(Direction::Up),
            Just(Direction::Down),
        ]
    }

    fn op() -> impl Strategy<Value = Op> {
        prop_oneof![
            4 => Just(Op::Insert),
            2 => (0usize..MAX_TILES).prop_map(Op::Close),
            2 => direction().prop_map(Op::Focus),
            1 => direction().prop_map(Op::Swap),
            1 => direction().prop_map(Op::Resize),
            1 => Just(Op::ToggleSplit),
            1 => Just(Op::Fullscreen),
        ]
    }

    proptest! {
        #[test]
        fn any_sequence_keeps_the_screen_exactly_covered(
            ops in prop::collection::vec(op(), 1..80),
            width in 300f32..3000.0,
            height in 300f32..2000.0,
        ) {
            let area = Rect::new(0.0, 0.0, width.round(), height.round());
            let mut layout = Layout::new();

            for op in ops {
                match op {
                    Op::Insert => { let _ = layout.insert(area); }
                    Op::Close(i) => {
                        let tiles = layout.tiles();
                        let _ = layout.close(tiles[i % tiles.len()]);
                    }
                    Op::Focus(d) => { layout.focus_towards(d, area); }
                    Op::Swap(d) => { layout.swap_towards(d, area); }
                    Op::Resize(d) => { layout.resize(d, 0.05); }
                    Op::ToggleSplit => { layout.toggle_split(); }
                    Op::Fullscreen => layout.toggle_fullscreen(),
                }

                prop_assert!(layout.is_valid());
                prop_assert!((1..=MAX_TILES).contains(&layout.len()));

                let rects: Vec<Rect> = layout.arrange(area, 0.0).into_iter().map(|(_, r)| r).collect();
                let covered: f32 = rects.iter().map(Rect::area).sum();
                prop_assert!((covered - area.area()).abs() < 1.0, "covered {} of {}", covered, area.area());
                for (i, a) in rects.iter().enumerate() {
                    prop_assert!(a.x >= 0.0 && a.y >= 0.0 && a.right() <= area.width + 0.01 && a.bottom() <= area.height + 0.01);
                    for b in &rects[i + 1..] {
                        let shared = overlap(a.x, a.right(), b.x, b.right()) * overlap(a.y, a.bottom(), b.y, b.bottom());
                        prop_assert!(shared < 0.01, "{:?} overlaps {:?}", a, b);
                    }
                }
            }
        }

        #[test]
        fn focus_moves_only_in_the_asked_direction(
            inserts in 1usize..MAX_TILES,
            moves in prop::collection::vec(direction(), 1..20),
        ) {
            let mut layout = Layout::new();
            for _ in 0..inserts {
                layout.insert(WIDE).unwrap();
            }
            for d in moves {
                let before = layout.rect(layout.focused(), WIDE).unwrap();
                if layout.focus_towards(d, WIDE) {
                    let after = layout.rect(layout.focused(), WIDE).unwrap();
                    let ok = match d {
                        Direction::Right => after.x >= before.right() - EPSILON,
                        Direction::Left => after.right() <= before.x + EPSILON,
                        Direction::Down => after.y >= before.bottom() - EPSILON,
                        Direction::Up => after.bottom() <= before.y + EPSILON,
                    };
                    prop_assert!(ok, "{:?}: {:?} -> {:?}", d, before, after);
                }
            }
        }
    }
}
