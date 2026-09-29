//! The folder or subject tree the graph draws.

/// One node. Nodes are indexed by position: node 0 is the library root.
pub struct GraphNode {
    pub name: String,
    pub parent: Option<usize>,
    /// Filled in by [`GraphTree::new`], in name order.
    pub children: Vec<usize>,
    /// Books in this node and everything beneath it, after filters.
    pub books: usize,
}

pub struct GraphTree {
    pub nodes: Vec<GraphNode>,
}

impl GraphTree {
    /// Links each node into its parent's children, sorted by name.
    pub fn new(mut nodes: Vec<GraphNode>) -> Self {
        for id in 1..nodes.len() {
            if let Some(parent) = nodes[id].parent {
                nodes[parent].children.push(id);
            }
        }
        let sort_keys: Vec<String> = nodes.iter().map(|node| node.name.to_lowercase()).collect();
        for id in 0..nodes.len() {
            let mut children = std::mem::take(&mut nodes[id].children);
            children.sort_by(|a, b| sort_keys[*a].cmp(&sort_keys[*b]).then_with(|| a.cmp(b)));
            nodes[id].children = children;
        }
        Self { nodes }
    }

    /// The path from the root to `id`, both included.
    pub fn ancestry(&self, id: usize) -> Vec<usize> {
        let mut path: Vec<usize> = std::iter::successors(Some(id), |&node| self.nodes[node].parent).collect();
        path.reverse();
        path
    }
}
