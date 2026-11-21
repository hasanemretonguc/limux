#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StageDirection {
    Right,
    Left,
    Down,
    Up,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StageOrientation {
    Horizontal,
    Vertical,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StageNode {
    Leaf {
        workspace_id: String,
    },
    Split {
        orientation: StageOrientation,
        first: Box<StageNode>,
        second: Box<StageNode>,
    },
}

pub const WORKSPACE_STAGE_CSS: &str = r#"
.limux-stage-slot {
    border: 1px solid alpha(@window_fg_color, 0.10);
}
.limux-stage-slot-focused {
    border: 1px solid alpha(@accent_bg_color, 0.65);
}
.limux-stage-header {
    background-color: @sidebar_bg_color;
    border-bottom: 1px solid alpha(@window_fg_color, 0.08);
    padding: 2px 6px;
    min-height: 24px;
}
.limux-stage-header-title {
    font-size: 11px;
    font-weight: 600;
    color: alpha(@window_fg_color, 0.78);
}
.limux-stage-slot-focused .limux-stage-header-title {
    color: @window_fg_color;
}
.limux-stage-detach-btn {
    background: transparent;
    border: none;
    border-radius: 2px;
    padding: 1px 4px;
    min-height: 18px;
    min-width: 18px;
    color: alpha(@window_fg_color, 0.45);
}
.limux-stage-detach-btn:hover {
    background: alpha(@window_fg_color, 0.10);
    color: @window_fg_color;
}
.limux-sidebar-row-staged .limux-sidebar-row-box {
    border-left: 2px solid alpha(@accent_bg_color, 0.45);
    background: alpha(@accent_bg_color, 0.07);
}
.limux-stage-dir-btn {
    padding: 3px 8px;
    min-height: 22px;
    font-size: 12px;
}
"#;

impl StageNode {
    pub fn single(workspace_id: impl Into<String>) -> Self {
        Self::Leaf {
            workspace_id: workspace_id.into(),
        }
    }

    pub fn is_multi(&self) -> bool {
        matches!(self, Self::Split { .. })
    }

    pub fn contains(&self, workspace_id: &str) -> bool {
        match self {
            Self::Leaf { workspace_id: id } => id == workspace_id,
            Self::Split { first, second, .. } => {
                first.contains(workspace_id) || second.contains(workspace_id)
            }
        }
    }

    pub fn workspace_ids(&self) -> Vec<String> {
        let mut ids = Vec::new();
        self.collect_ids(&mut ids);
        ids
    }

    fn collect_ids(&self, out: &mut Vec<String>) {
        match self {
            Self::Leaf { workspace_id } => out.push(workspace_id.clone()),
            Self::Split { first, second, .. } => {
                first.collect_ids(out);
                second.collect_ids(out);
            }
        }
    }

    /// Split `target_ws_id` in `direction` to place `new_ws_id` alongside it.
    /// If `new_ws_id` is already present elsewhere in the stage, it is moved
    /// from its previous slot into the new position.
    pub fn split_workspace(
        self,
        target_ws_id: &str,
        new_ws_id: &str,
        direction: StageDirection,
    ) -> Self {
        if target_ws_id == new_ws_id {
            return self;
        }
        let cleaned = if self.contains(new_ws_id) {
            match self.remove_workspace(new_ws_id) {
                Some(tree) => tree,
                None => Self::single(target_ws_id),
            }
        } else {
            self
        };

        let mut tree = cleaned;
        if !tree.replace_leaf_with_split(target_ws_id, new_ws_id, direction) {
            // If target wasn't in the tree, split the whole tree root.
            let new_leaf = Box::new(Self::single(new_ws_id));
            let old_root = Box::new(tree);
            return make_split(old_root, new_leaf, direction);
        }
        tree
    }

    fn replace_leaf_with_split(
        &mut self,
        target_ws_id: &str,
        new_ws_id: &str,
        direction: StageDirection,
    ) -> bool {
        match self {
            Self::Leaf { workspace_id } if workspace_id == target_ws_id => {
                let target_leaf = Box::new(Self::single(target_ws_id));
                let new_leaf = Box::new(Self::single(new_ws_id));
                *self = make_split(target_leaf, new_leaf, direction);
                true
            }
            Self::Leaf { .. } => false,
            Self::Split { first, second, .. } => {
                first.replace_leaf_with_split(target_ws_id, new_ws_id, direction)
                    || second.replace_leaf_with_split(target_ws_id, new_ws_id, direction)
            }
        }
    }

    /// Remove `workspace_id` from the stage tree, collapsing its parent split.
    pub fn remove_workspace(self, workspace_id: &str) -> Option<Self> {
        match self {
            Self::Leaf { workspace_id: id } => {
                if id == workspace_id {
                    None
                } else {
                    Some(Self::Leaf { workspace_id: id })
                }
            }
            Self::Split {
                orientation,
                first,
                second,
            } => {
                let first_res = first.remove_workspace(workspace_id);
                let second_res = second.remove_workspace(workspace_id);
                match (first_res, second_res) {
                    (Some(a), Some(b)) => Some(Self::Split {
                        orientation,
                        first: Box::new(a),
                        second: Box::new(b),
                    }),
                    (Some(only), None) | (None, Some(only)) => Some(only),
                    (None, None) => None,
                }
            }
        }
    }
}

fn make_split(
    target: Box<StageNode>,
    incoming: Box<StageNode>,
    direction: StageDirection,
) -> StageNode {
    match direction {
        StageDirection::Right => StageNode::Split {
            orientation: StageOrientation::Horizontal,
            first: target,
            second: incoming,
        },
        StageDirection::Left => StageNode::Split {
            orientation: StageOrientation::Horizontal,
            first: incoming,
            second: target,
        },
        StageDirection::Down => StageNode::Split {
            orientation: StageOrientation::Vertical,
            first: target,
            second: incoming,
        },
        StageDirection::Up => StageNode::Split {
            orientation: StageOrientation::Vertical,
            first: incoming,
            second: target,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::{StageDirection, StageNode, StageOrientation};

    #[test]
    fn splits_arbitrarily_in_all_four_directions_and_collapses_on_remove() {
        let tree = StageNode::single("ws-1")
            .split_workspace("ws-1", "ws-2", StageDirection::Right)
            .split_workspace("ws-2", "ws-3", StageDirection::Down)
            .split_workspace("ws-1", "ws-4", StageDirection::Up);

        assert!(tree.is_multi());
        assert_eq!(tree.workspace_ids(), vec!["ws-4", "ws-1", "ws-2", "ws-3"]);

        // Moving an already-staged workspace relocates it without duplicating.
        let moved = tree.split_workspace("ws-3", "ws-4", StageDirection::Left);
        assert_eq!(moved.workspace_ids(), vec!["ws-1", "ws-2", "ws-4", "ws-3"]);

        // Removing workspaces collapses splits cleanly back to a single leaf.
        let collapsed = moved
            .remove_workspace("ws-4")
            .unwrap()
            .remove_workspace("ws-3")
            .unwrap()
            .remove_workspace("ws-2")
            .unwrap();
        assert_eq!(collapsed, StageNode::single("ws-1"));
        assert!(!collapsed.is_multi());
    }

    #[test]
    fn left_and_up_place_new_workspace_first() {
        let left = StageNode::single("a").split_workspace("a", "b", StageDirection::Left);
        assert_eq!(
            left,
            StageNode::Split {
                orientation: StageOrientation::Horizontal,
                first: Box::new(StageNode::single("b")),
                second: Box::new(StageNode::single("a")),
            }
        );

        let up = StageNode::single("a").split_workspace("a", "b", StageDirection::Up);
        assert_eq!(
            up,
            StageNode::Split {
                orientation: StageOrientation::Vertical,
                first: Box::new(StageNode::single("b")),
                second: Box::new(StageNode::single("a")),
            }
        );
    }
}
