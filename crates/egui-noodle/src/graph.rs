/// A node: its place in the graph, the data of the application, and the node of each input.
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Node<N> {
    pub id: u32,
    pub pos: [f32; 2],
    pub data: N,
    /// The node linked to each input (None: no link).
    pub inputs: Vec<Option<u32>>,
    #[cfg_attr(feature = "serde", serde(default))]
    pub collapsed: bool,
    /// A muted node is off: the application gives its first input in its place.
    #[cfg_attr(feature = "serde", serde(default))]
    pub muted: bool,
}

/// The nodes of a graph. A link goes from the output of a node to an input of an other node.
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Graph<N> {
    pub nodes: Vec<Node<N>>,
}

impl<N> Default for Graph<N> {
    fn default() -> Self {
        Graph { nodes: vec![] }
    }
}

impl<N> Graph<N> {
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    pub fn node(&self, id: u32) -> Option<&Node<N>> {
        self.nodes.iter().find(|n| n.id == id)
    }

    pub fn node_mut(&mut self, id: u32) -> Option<&mut Node<N>> {
        self.nodes.iter_mut().find(|n| n.id == id)
    }

    /// Add a node with `n_inputs` inputs (or the links `inputs` if they are given). Returns its id.
    pub fn add(&mut self, data: N, pos: [f32; 2], n_inputs: usize, inputs: Vec<Option<u32>>) -> u32 {
        let id = self.nodes.iter().map(|n| n.id + 1).max().unwrap_or(1);
        let inputs = if inputs.is_empty() { vec![None; n_inputs] } else { inputs };
        self.nodes.push(Node { id, pos, data, inputs, collapsed: false, muted: false });
        id
    }

    /// Remove node `id` and the links to it.
    pub fn remove(&mut self, id: u32) {
        self.nodes.retain(|n| n.id != id);
        self.nodes.iter_mut().flat_map(|n| n.inputs.iter_mut()).filter(|i| **i == Some(id)).for_each(|i| *i = None);
    }

    /// True if node `a` is node `b`, or uses it (a link from `b` to `a`, or to a node that `a` uses).
    pub fn depends(&self, a: u32, b: u32) -> bool {
        a == b || self.node(a).is_some_and(|n| n.inputs.iter().flatten().any(|&i| self.depends(i, b)))
    }

    /// Link the output of node `from` to input `k` of node `to`. A link that makes a cycle is refused
    /// (false).
    pub fn link(&mut self, from: u32, to: u32, k: usize) -> bool {
        if self.depends(from, to) {
            return false;
        }
        match self.node_mut(to).and_then(|n| n.inputs.get_mut(k)) {
            Some(i) => {
                *i = Some(from);
                true
            }
            None => false,
        }
    }

    /// Put the nodes in columns: the nodes without inputs at the left, then each node one column after its
    /// inputs. `height` gives the height of a node (an estimate is enough).
    pub fn layout(&mut self, width: f32, height: impl Fn(&Node<N>) -> f32) {
        fn depth<N>(g: &Graph<N>, id: u32, seen: usize) -> usize {
            if seen > g.nodes.len() {
                return 0;
            }
            g.node(id).map_or(0, |n| n.inputs.iter().flatten().map(|&i| 1 + depth(g, i, seen + 1)).max().unwrap_or(0))
        }
        let d: Vec<usize> = self.nodes.iter().map(|n| depth(self, n.id, 0)).collect();
        let mut cols: std::collections::HashMap<usize, f32> = std::collections::HashMap::new();
        for (k, n) in d.into_iter().enumerate() {
            let y = cols.entry(n).or_default();
            let h = height(&self.nodes[k]);
            self.nodes[k].pos = [20.0 + n as f32 * (width + 60.0), 20.0 + *y];
            *y += h + 30.0;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn links_and_cycles() {
        let mut g = Graph::default();
        let a = g.add("a", [0.0; 2], 0, vec![]);
        let b = g.add("b", [0.0; 2], 1, vec![]);
        let c = g.add("c", [0.0; 2], 2, vec![]);
        assert!(g.link(a, b, 0) && g.link(b, c, 0) && g.link(a, c, 1));
        assert!(!g.link(c, b, 0), "a cycle");
        assert!(g.depends(c, a) && !g.depends(a, c));
        g.layout(200.0, |_| 100.0);
        assert!(g.node(c).unwrap().pos[0] > g.node(b).unwrap().pos[0]);
        g.remove(b);
        assert_eq!(g.node(c).unwrap().inputs, vec![None, Some(a)]);
    }
}
