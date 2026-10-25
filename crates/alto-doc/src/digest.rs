//! A content digest of the document that an edit updates in proportion to what it
//! changed. It identifies states (saved or not, equal or not); file names still use
//! the hash of the snapshot bytes.
use crate::{ChangeSet, Document, Node};
use alto_core::{ContentHash, NodeId};
use std::collections::HashMap;

#[derive(Clone, Debug)]
pub struct StateDigest {
    nodes: HashMap<NodeId, ContentHash>,
    meta: ContentHash,
    assets: ContentHash,
    selection: ContentHash,
    total: ContentHash,
}

fn json_hash<T: serde::Serialize>(value: &T) -> ContentHash {
    // Serializing these document types cannot fail: keys are strings or ids.
    ContentHash::of(&serde_json::to_vec(value).unwrap_or_default())
}
fn node_hash(node: &Node) -> ContentHash {
    json_hash(node)
}

impl StateDigest {
    pub fn of(doc: &Document) -> Self {
        let mut d = Self {
            nodes: doc
                .nodes
                .iter()
                .map(|(id, n)| (*id, node_hash(n)))
                .collect(),
            meta: json_hash(&doc.meta),
            assets: json_hash(&doc.assets),
            selection: json_hash(&doc.selection),
            total: ContentHash([0; 32]),
        };
        d.total = d.combine(doc);
        d
    }
    /// Rehashes only what `changes` names. `changes` must come from applying ops to `doc`.
    pub fn update(&mut self, doc: &Document, changes: &ChangeSet) {
        for id in &changes.nodes {
            match doc.nodes.get(id) {
                Some(n) => {
                    self.nodes.insert(*id, node_hash(n));
                }
                None => {
                    self.nodes.remove(id);
                }
            }
        }
        if changes.meta_changed {
            self.meta = json_hash(&doc.meta);
        }
        if changes.assets_changed {
            self.assets = json_hash(&doc.assets);
        }
        if changes.selection_changed {
            self.selection = json_hash(&doc.selection);
        }
        self.total = self.combine(doc);
    }
    pub fn value(&self) -> ContentHash {
        self.total
    }
    /// Tree order, so moving a node changes the digest even if no node bytes did.
    fn combine(&self, doc: &Document) -> ContentHash {
        let mut bytes = Vec::with_capacity(96 + self.nodes.len() * 48);
        for part in [self.meta, self.assets, self.selection] {
            bytes.extend_from_slice(&part.0);
        }
        let mut stack = vec![doc.root];
        while let Some(id) = stack.pop() {
            bytes.extend_from_slice(id.0.as_bytes());
            if let Some(d) = self.nodes.get(&id) {
                bytes.extend_from_slice(&d.0);
            }
            if let Some(children) = doc.nodes.get(&id).and_then(Node::children) {
                stack.extend(children.iter().rev());
            }
        }
        ContentHash::of(&bytes)
    }
}
