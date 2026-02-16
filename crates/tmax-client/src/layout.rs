use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SplitAxis {
    Horizontal,
    Vertical,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
}

#[derive(Debug)]
enum Node {
    Leaf(String),
    Split {
        axis: SplitAxis,
        ratio: f32,
        first: Box<Node>,
        second: Box<Node>,
    },
}

#[derive(Debug)]
pub struct PaneLayout {
    root: Node,
    next_id: u64,
}

impl PaneLayout {
    pub fn new() -> Self {
        Self {
            root: Node::Leaf("pane-1".to_string()),
            next_id: 2,
        }
    }

    pub fn root_id(&self) -> &str {
        match &self.root {
            Node::Leaf(id) => id,
            Node::Split { first, .. } => first.first_leaf(),
        }
    }

    pub fn split(&mut self, pane_id: &str, axis: SplitAxis) -> Option<String> {
        let new_id = format!("pane-{}", self.next_id);
        self.next_id += 1;

        if self.root.split_target(pane_id, axis, &new_id) {
            return Some(new_id);
        }

        None
    }

    pub fn resize_towards(&mut self, pane_id: &str, delta: f32) -> bool {
        self.root.resize_towards(pane_id, delta)
    }

    pub fn compute_rects(&self, width: u16, height: u16) -> HashMap<String, Rect> {
        let mut out = HashMap::new();
        self.root.compute_rects(
            Rect {
                x: 0,
                y: 0,
                width,
                height,
            },
            &mut out,
        );
        out
    }
}

impl Node {
    fn first_leaf(&self) -> &str {
        match self {
            Node::Leaf(id) => id,
            Node::Split { first, .. } => first.first_leaf(),
        }
    }

    fn split_target(&mut self, pane_id: &str, axis: SplitAxis, new_id: &str) -> bool {
        match self {
            Node::Leaf(existing) if existing == pane_id => {
                let old_id = existing.clone();
                *self = Node::Split {
                    axis,
                    ratio: 0.5,
                    first: Box::new(Node::Leaf(old_id)),
                    second: Box::new(Node::Leaf(new_id.to_string())),
                };
                true
            }
            Node::Leaf(_) => false,
            Node::Split { first, second, .. } => {
                first.split_target(pane_id, axis, new_id)
                    || second.split_target(pane_id, axis, new_id)
            }
        }
    }

    fn resize_towards(&mut self, pane_id: &str, delta: f32) -> bool {
        match self {
            Node::Leaf(_) => false,
            Node::Split {
                ratio,
                first,
                second,
                ..
            } => {
                if first.contains(pane_id) {
                    *ratio = (*ratio + delta).clamp(0.1, 0.9);
                    true
                } else if second.contains(pane_id) {
                    *ratio = (*ratio - delta).clamp(0.1, 0.9);
                    true
                } else {
                    first.resize_towards(pane_id, delta) || second.resize_towards(pane_id, delta)
                }
            }
        }
    }

    fn contains(&self, pane_id: &str) -> bool {
        match self {
            Node::Leaf(id) => id == pane_id,
            Node::Split { first, second, .. } => {
                first.contains(pane_id) || second.contains(pane_id)
            }
        }
    }

    fn compute_rects(&self, rect: Rect, out: &mut HashMap<String, Rect>) {
        match self {
            Node::Leaf(id) => {
                out.insert(id.clone(), rect);
            }
            Node::Split {
                axis,
                ratio,
                first,
                second,
            } => match axis {
                SplitAxis::Horizontal => {
                    let first_h = ((f32::from(rect.height) * *ratio).round() as u16)
                        .clamp(1, rect.height.saturating_sub(1));
                    let second_h = rect.height.saturating_sub(first_h);
                    first.compute_rects(
                        Rect {
                            x: rect.x,
                            y: rect.y,
                            width: rect.width,
                            height: first_h,
                        },
                        out,
                    );
                    second.compute_rects(
                        Rect {
                            x: rect.x,
                            y: rect.y.saturating_add(first_h),
                            width: rect.width,
                            height: second_h.max(1),
                        },
                        out,
                    );
                }
                SplitAxis::Vertical => {
                    let first_w = ((f32::from(rect.width) * *ratio).round() as u16)
                        .clamp(1, rect.width.saturating_sub(1));
                    let second_w = rect.width.saturating_sub(first_w);
                    first.compute_rects(
                        Rect {
                            x: rect.x,
                            y: rect.y,
                            width: first_w,
                            height: rect.height,
                        },
                        out,
                    );
                    second.compute_rects(
                        Rect {
                            x: rect.x.saturating_add(first_w),
                            y: rect.y,
                            width: second_w.max(1),
                            height: rect.height,
                        },
                        out,
                    );
                }
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{PaneLayout, SplitAxis};

    #[test]
    fn split_creates_two_panes_and_rects() {
        let mut layout = PaneLayout::new();
        let root = layout.root_id().to_string();
        let second = layout
            .split(&root, SplitAxis::Vertical)
            .expect("split should create pane");
        let rects = layout.compute_rects(100, 20);
        assert_eq!(rects.len(), 2);
        let left = rects.get(&root).expect("left pane");
        let right = rects.get(&second).expect("right pane");
        assert_eq!(left.width + right.width, 100);
        assert_eq!(left.height, 20);
        assert_eq!(right.height, 20);
    }

    #[test]
    fn resize_adjusts_split_ratio() {
        let mut layout = PaneLayout::new();
        let root = layout.root_id().to_string();
        let second = layout
            .split(&root, SplitAxis::Horizontal)
            .expect("split should create pane");
        assert!(layout.resize_towards(&second, 0.2));
        let rects = layout.compute_rects(80, 30);
        let top = rects.get(&root).expect("top pane");
        let bottom = rects.get(&second).expect("bottom pane");
        assert!(top.height < bottom.height);
        assert_eq!(top.height + bottom.height, 30);
    }
}
