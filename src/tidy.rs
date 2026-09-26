//! Compact ("tidy") tree layout, after Reingold & Tilford.
//!
//! Positions every node along the *breadth* axis (across a generation);
//! the caller places generations along the depth axis. Each subtree takes
//! only the room it uses at each depth, sibling subtrees are pushed
//! together until they would touch at some depth, and a parent is centred
//! over its first and last child. The result is far tighter than giving
//! every generation a fixed, doubling number of slots.

/// One node of the tree to lay out, stored in an arena.
pub struct TidyNode {
    /// Extent along the breadth axis.
    pub size: f32,
    /// How many depth levels the node occupies (more than one for a
    /// stacked column of leaves).
    pub span: usize,
    pub children: Vec<usize>,
}

impl TidyNode {
    pub fn new(size: f32) -> Self {
        Self { size, span: 1, children: Vec::new() }
    }
}

/// Per depth (0 = the subtree's root), the (min, max) breadth occupied,
/// relative to the subtree root's centre.
type Contour = Vec<(f32, f32)>;

/// Returns each node's centre along the breadth axis, with `root` at 0.
/// `gap` is the minimum clearance between neighbouring subtrees.
pub fn layout(nodes: &[TidyNode], root: usize, gap: f32) -> Vec<f32> {
    let mut rel = vec![0.0; nodes.len()];
    contour(nodes, root, gap, &mut rel);
    // Relative offsets → absolute positions, top-down.
    let mut abs = vec![0.0; nodes.len()];
    let mut stack = vec![root];
    while let Some(i) = stack.pop() {
        for &c in &nodes[i].children {
            abs[c] = abs[i] + rel[c];
            stack.push(c);
        }
    }
    abs
}

fn contour(nodes: &[TidyNode], i: usize, gap: f32, rel: &mut [f32]) -> Contour {
    let half = nodes[i].size / 2.0;
    let kids = &nodes[i].children;
    if kids.is_empty() {
        return vec![(-half, half); nodes[i].span.max(1)];
    }
    // Place children left to right, each as close to the accumulated
    // contour of its elder siblings as every shared depth allows.
    let mut merged: Contour = Vec::new();
    let mut offsets = Vec::with_capacity(kids.len());
    for &k in kids {
        let c = contour(nodes, k, gap, rel);
        let shift = if merged.is_empty() {
            0.0
        } else {
            merged.iter().zip(&c).map(|(m, n)| m.1 - n.0 + gap).fold(f32::MIN, f32::max)
        };
        for (d, (lo, hi)) in c.into_iter().enumerate() {
            let (lo, hi) = (lo + shift, hi + shift);
            match merged.get_mut(d) {
                Some(m) => *m = (m.0.min(lo), m.1.max(hi)),
                None => merged.push((lo, hi)),
            }
        }
        offsets.push(shift);
    }
    let mid = (offsets[0] + offsets[offsets.len() - 1]) / 2.0;
    for (&k, off) in kids.iter().zip(&offsets) {
        rel[k] = off - mid;
    }
    let mut out = vec![(-half, half)];
    out.extend(merged.into_iter().map(|(lo, hi)| (lo - mid, hi - mid)));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(size: f32, children: Vec<usize>) -> TidyNode {
        TidyNode { size, span: 1, children }
    }

    #[test]
    fn siblings_are_packed_and_parent_centred() {
        // 0 has children 1 and 2, each a leaf of size 10, gap 2.
        let nodes = [node(10.0, vec![1, 2]), node(10.0, vec![]), node(10.0, vec![])];
        let x = layout(&nodes, 0, 2.0);
        assert_eq!(x, vec![0.0, -6.0, 6.0]);
    }

    #[test]
    fn a_shallow_branch_tucks_beside_a_wide_deep_one() {
        // Root's first child (1) has four grandchildren; second child (2)
        // has none. Child 2 only has to clear child 1, not 1's grandchildren.
        let mut nodes = vec![node(10.0, vec![1, 2]), node(10.0, vec![3, 4, 5, 6]), node(10.0, vec![])];
        nodes.extend((0..4).map(|_| node(10.0, vec![])));
        let x = layout(&nodes, 0, 2.0);
        assert_eq!(x[2] - x[1], 12.0, "only depth 1 constrains the gap");
        // Grandchildren still don't overlap each other.
        for w in [3, 4, 5].iter().zip([4, 5, 6].iter()) {
            assert!(x[*w.1] - x[*w.0] >= 12.0);
        }
    }

    #[test]
    fn a_tall_leaf_keeps_deeper_neighbours_away() {
        // Child 1 spans two depths; child 2's own child (3) sits at the
        // second of them and must clear it.
        let mut tall = node(10.0, vec![]);
        tall.span = 2;
        let nodes = [node(10.0, vec![1, 2]), tall, node(4.0, vec![3]), node(30.0, vec![])];
        let x = layout(&nodes, 0, 2.0);
        assert!(x[3] - 15.0 >= x[1] + 5.0 + 2.0 - 1e-4, "grandchild clears the tall leaf");
    }
}
