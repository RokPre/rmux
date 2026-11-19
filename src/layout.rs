use serde::Serialize;

#[derive(Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Axis {
    Vertical,
    Horizontal,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Direction {
    Left,
    Down,
    Up,
    Right,
}

impl Direction {
    pub(crate) fn from_byte(value: u8) -> Option<Self> {
        match value {
            b'h' => Some(Self::Left),
            b'j' => Some(Self::Down),
            b'k' => Some(Self::Up),
            b'l' => Some(Self::Right),
            _ => None,
        }
    }
}

#[derive(Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum WindowNode {
    Window {
        id: u64,
        buffer: u64,
    },
    Split {
        axis: Axis,
        ratio: u8,
        first: Box<WindowNode>,
        second: Box<WindowNode>,
    },
}

pub(crate) struct WindowLayout {
    tree: WindowNode,
    focused_window: u64,
    next_window: u64,
}

#[derive(Clone, Copy)]
struct Rect {
    left: f64,
    top: f64,
    right: f64,
    bottom: f64,
}

impl WindowLayout {
    pub(crate) fn new(initial_buffer: u64) -> Self {
        Self {
            tree: WindowNode::Window {
                id: 1,
                buffer: initial_buffer,
            },
            focused_window: 1,
            next_window: 2,
        }
    }

    pub(crate) fn focused_buffer(&self) -> Option<u64> {
        find_buffer(&self.tree, self.focused_window)
    }

    pub(crate) fn set_focused_buffer(&mut self, buffer: u64) -> bool {
        set_buffer(&mut self.tree, self.focused_window, buffer)
    }

    pub(crate) fn split_focused(&mut self, axis: Axis) -> bool {
        let id = self.next_window;
        let Some(next) = id.checked_add(1) else {
            return false;
        };
        let Some(buffer) = self.focused_buffer() else {
            return false;
        };
        if !split(&mut self.tree, self.focused_window, id, buffer, axis) {
            return false;
        }
        self.focused_window = id;
        self.next_window = next;
        true
    }

    pub(crate) fn close_focused_window(&mut self) -> bool {
        let mut ids = Vec::new();
        collect_ids(&self.tree, &mut ids);
        if ids.len() <= 1 {
            return false;
        }
        let Some(index) = ids.iter().position(|id| *id == self.focused_window) else {
            return false;
        };
        if !remove_window(&mut self.tree, self.focused_window) {
            return false;
        }
        let mut remaining = Vec::new();
        collect_ids(&self.tree, &mut remaining);
        self.focused_window = remaining[index.min(remaining.len() - 1)];
        true
    }

    pub(crate) fn focus_direction(&mut self, direction: Direction) -> bool {
        let rects = window_rects(&self.tree);
        let Some((_, source)) = rects.iter().find(|(id, _)| *id == self.focused_window) else {
            return false;
        };
        let target = rects
            .iter()
            .filter(|(id, rect)| *id != self.focused_window && adjacent(*source, *rect, direction))
            .max_by(|(_, a), (_, b)| compare_candidates(*source, *a, *b, direction));
        let Some((id, _)) = target else { return false };
        self.focused_window = *id;
        true
    }

    pub(crate) fn move_focused_window(&mut self, direction: Direction) -> bool {
        let rects = window_rects(&self.tree);
        let Some((_, source)) = rects.iter().find(|(id, _)| *id == self.focused_window) else {
            return false;
        };
        let target = rects
            .iter()
            .filter(|(id, rect)| *id != self.focused_window && adjacent(*source, *rect, direction))
            .max_by(|(_, a), (_, b)| compare_candidates(*source, *a, *b, direction));
        let Some((target_id, _)) = target else {
            return false;
        };
        swap_windows(&mut self.tree, self.focused_window, *target_id)
    }

    pub(crate) fn resize_focused_window(&mut self, direction: Direction) -> bool {
        resize(&mut self.tree, self.focused_window, direction).unwrap_or(false)
    }

    pub(crate) fn replace_buffer(&mut self, old: u64, new: u64) -> bool {
        replace_buffer(&mut self.tree, old, new)
    }

    pub(crate) fn snapshot(&self, buffers: &[u64]) -> Vec<u8> {
        #[derive(Serialize)]
        struct Snapshot<'a> {
            tree: &'a WindowNode,
            focused_window: u64,
            active_buffer: Option<u64>,
            buffers: &'a [u64],
        }
        serde_json::to_vec(&Snapshot {
            tree: &self.tree,
            focused_window: self.focused_window,
            active_buffer: self.focused_buffer(),
            buffers,
        })
        .expect("serializing a window layout cannot fail")
    }
}

fn find_buffer(node: &WindowNode, id: u64) -> Option<u64> {
    match node {
        WindowNode::Window { id: window, buffer } => (*window == id).then_some(*buffer),
        WindowNode::Split { first, second, .. } => {
            find_buffer(first, id).or_else(|| find_buffer(second, id))
        }
    }
}

fn set_buffer(node: &mut WindowNode, id: u64, value: u64) -> bool {
    match node {
        WindowNode::Window { id: window, buffer } if *window == id => {
            *buffer = value;
            true
        }
        WindowNode::Split { first, second, .. } => {
            set_buffer(first, id, value) || set_buffer(second, id, value)
        }
        _ => false,
    }
}

fn split(node: &mut WindowNode, focused: u64, id: u64, buffer: u64, axis: Axis) -> bool {
    match node {
        WindowNode::Window {
            id: existing,
            buffer: current,
        } if *existing == focused => {
            *node = WindowNode::Split {
                axis,
                ratio: 50,
                first: Box::new(WindowNode::Window {
                    id: *existing,
                    buffer: *current,
                }),
                second: Box::new(WindowNode::Window { id, buffer }),
            };
            true
        }
        WindowNode::Split { first, second, .. } => {
            split(first, focused, id, buffer, axis) || split(second, focused, id, buffer, axis)
        }
        _ => false,
    }
}

fn collect_ids(node: &WindowNode, ids: &mut Vec<u64>) {
    match node {
        WindowNode::Window { id, .. } => ids.push(*id),
        WindowNode::Split { first, second, .. } => {
            collect_ids(first, ids);
            collect_ids(second, ids);
        }
    }
}

fn remove_window(node: &mut WindowNode, id: u64) -> bool {
    match node {
        WindowNode::Window { id: window, .. } => *window == id,
        WindowNode::Split { first, second, .. } => {
            if matches!(first.as_ref(), WindowNode::Window { id: window, .. } if *window == id) {
                *node = (**second).clone();
                true
            } else if matches!(second.as_ref(), WindowNode::Window { id: window, .. } if *window == id)
            {
                *node = (**first).clone();
                true
            } else {
                remove_window(first, id) || remove_window(second, id)
            }
        }
    }
}

fn window_rects(node: &WindowNode) -> Vec<(u64, Rect)> {
    let mut rects = Vec::new();
    collect_rects(
        node,
        Rect {
            left: 0.0,
            top: 0.0,
            right: 1.0,
            bottom: 1.0,
        },
        &mut rects,
    );
    rects
}

fn collect_rects(node: &WindowNode, rect: Rect, out: &mut Vec<(u64, Rect)>) {
    match node {
        WindowNode::Window { id, .. } => out.push((*id, rect)),
        WindowNode::Split {
            axis,
            ratio,
            first,
            second,
        } => {
            let fraction = f64::from(*ratio) / 100.0;
            match axis {
                Axis::Vertical => {
                    let edge = rect.left + (rect.right - rect.left) * fraction;
                    collect_rects(
                        first,
                        Rect {
                            right: edge,
                            ..rect
                        },
                        out,
                    );
                    collect_rects(second, Rect { left: edge, ..rect }, out);
                }
                Axis::Horizontal => {
                    let edge = rect.top + (rect.bottom - rect.top) * fraction;
                    collect_rects(
                        first,
                        Rect {
                            bottom: edge,
                            ..rect
                        },
                        out,
                    );
                    collect_rects(second, Rect { top: edge, ..rect }, out);
                }
            }
        }
    }
}

fn adjacent(a: Rect, b: Rect, direction: Direction) -> bool {
    let overlap = match direction {
        Direction::Left | Direction::Right => (a.bottom.min(b.bottom) - a.top.max(b.top)).max(0.0),
        Direction::Up | Direction::Down => (a.right.min(b.right) - a.left.max(b.left)).max(0.0),
    };
    if overlap <= 0.0 {
        return false;
    }
    match direction {
        Direction::Left => (b.right - a.left).abs() < 1e-9,
        Direction::Right => (b.left - a.right).abs() < 1e-9,
        Direction::Up => (b.bottom - a.top).abs() < 1e-9,
        Direction::Down => (b.top - a.bottom).abs() < 1e-9,
    }
}

fn compare_candidates(source: Rect, a: Rect, b: Rect, direction: Direction) -> std::cmp::Ordering {
    let overlap = |rect: Rect| match direction {
        Direction::Left | Direction::Right => {
            (source.bottom.min(rect.bottom) - source.top.max(rect.top)).max(0.0)
        }
        Direction::Up | Direction::Down => {
            (source.right.min(rect.right) - source.left.max(rect.left)).max(0.0)
        }
    };
    overlap(a)
        .total_cmp(&overlap(b))
        .then_with(|| center_distance(source, b).total_cmp(&center_distance(source, a)))
}

fn center_distance(a: Rect, b: Rect) -> f64 {
    let dx = (a.left + a.right - b.left - b.right) / 2.0;
    let dy = (a.top + a.bottom - b.top - b.bottom) / 2.0;
    dx * dx + dy * dy
}

fn swap_windows(node: &mut WindowNode, a: u64, b: u64) -> bool {
    let Some(left_buffer) = take_window(node, a, 0) else {
        return false;
    };
    let Some(right_buffer) = take_window(node, b, u64::MAX) else {
        fill_window(node, 0, a, left_buffer);
        return false;
    };
    fill_window(node, 0, b, right_buffer);
    fill_window(node, u64::MAX, a, left_buffer);
    true
}

fn take_window(node: &mut WindowNode, id: u64, marker: u64) -> Option<u64> {
    match node {
        WindowNode::Window { id: window, buffer } if *window == id => {
            let taken = *buffer;
            *window = marker;
            *buffer = 0;
            Some(taken)
        }
        WindowNode::Split { first, second, .. } => {
            take_window(first, id, marker).or_else(|| take_window(second, id, marker))
        }
        _ => None,
    }
}

fn fill_window(node: &mut WindowNode, marker: u64, id: u64, buffer: u64) -> bool {
    match node {
        WindowNode::Window {
            id: window,
            buffer: value,
        } if *window == marker => {
            *window = id;
            *value = buffer;
            true
        }
        WindowNode::Split { first, second, .. } => {
            fill_window(first, marker, id, buffer) || fill_window(second, marker, id, buffer)
        }
        _ => false,
    }
}

fn resize(node: &mut WindowNode, focused: u64, direction: Direction) -> Option<bool> {
    match node {
        WindowNode::Window { .. } => None,
        WindowNode::Split {
            axis,
            ratio,
            first,
            second,
        } => {
            let in_first = contains(first, focused);
            let in_second = contains(second, focused);
            if !in_first && !in_second {
                return None;
            }
            let child_result = if in_first {
                resize(first, focused, direction)
            } else {
                resize(second, focused, direction)
            };
            if child_result.is_some() {
                return child_result;
            }
            let matching_axis = match direction {
                Direction::Left | Direction::Right => *axis == Axis::Vertical,
                Direction::Up | Direction::Down => *axis == Axis::Horizontal,
            };
            if !matching_axis {
                return None;
            }
            // Move the split boundary in the requested direction.
            if matches!(direction, Direction::Right | Direction::Down) {
                *ratio = (*ratio).saturating_add(5).min(90);
            } else {
                *ratio = (*ratio).saturating_sub(5).max(10);
            }
            Some(true)
        }
    }
}

fn contains(node: &WindowNode, id: u64) -> bool {
    match node {
        WindowNode::Window { id: window, .. } => *window == id,
        WindowNode::Split { first, second, .. } => contains(first, id) || contains(second, id),
    }
}

fn replace_buffer(node: &mut WindowNode, old: u64, new: u64) -> bool {
    match node {
        WindowNode::Window { buffer, .. } if *buffer == old => {
            *buffer = new;
            true
        }
        WindowNode::Window { .. } => false,
        WindowNode::Split { first, second, .. } => {
            let left = replace_buffer(first, old, new);
            let right = replace_buffer(second, old, new);
            left || right
        }
    }
}
#[cfg(test)]
mod tests {
    use super::{Axis, Direction, WindowLayout};

    #[test]
    fn moving_window_swaps_positions_and_keeps_focus() {
        let mut layout = WindowLayout::new(1);
        assert!(layout.split_focused(Axis::Vertical));
        assert!(layout.split_focused(Axis::Horizontal));
        assert!(layout.focus_direction(Direction::Up));
        assert!(layout.move_focused_window(Direction::Left));

        let snapshot: serde_json::Value = serde_json::from_slice(&layout.snapshot(&[1])).unwrap();
        assert_eq!(snapshot["focused_window"], 2);
        assert_eq!(snapshot["tree"]["first"]["id"], 2);
        assert_eq!(snapshot["tree"]["second"]["first"]["id"], 1);
        assert_eq!(snapshot["tree"]["second"]["second"]["id"], 3);
    }
}
