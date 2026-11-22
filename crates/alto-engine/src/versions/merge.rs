use super::branches::fail;
use super::*;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct MergeValue {
    pub present: bool,
    pub value: Value,
}
impl MergeValue {
    fn of(v: Option<&Value>) -> Self {
        Self {
            present: v.is_some(),
            value: v.cloned().unwrap_or(Value::Null),
        }
    }
    fn optional(&self) -> Option<Value> {
        self.present.then(|| self.value.clone())
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MergeConflict {
    pub id: String,
    pub kind: String,
    pub path: Vec<String>,
    pub base: MergeValue,
    pub ours: MergeValue,
    pub theirs: MergeValue,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MergeResolution {
    pub conflict_id: String,
    pub choice: String,
    pub value: Option<MergeValue>,
}
#[derive(Clone, Debug, Serialize)]
pub struct MergePlan {
    pub kind: String,
    pub token: String,
    pub ours: ContentHash,
    pub theirs: ContentHash,
    pub base: Option<ContentHash>,
    pub conflicts: Vec<MergeConflict>,
}
#[derive(Clone)]
pub(super) enum MergeTarget {
    Branch { name: String },
    Stash { commit: ContentHash, drop: bool },
}
#[derive(Clone)]
pub(super) struct MergeSession {
    view: MergePlan,
    target: MergeTarget,
    refs: ContentHash,
    revision: u64,
    working: ContentHash,
    result: Value,
    canvas: Option<CanvasLink>,
}
/// A canvas edit moves the root transform and resizes the canvas together, so the
/// transform only fits the canvas size of its own side. When the root transform
/// conflicts, its resolution carries that side's size along.
#[derive(Clone)]
struct CanvasLink {
    conflict: String,
    theirs: Value,
}
const CANVAS_SIZE: [&str; 2] = ["width", "height"];
fn canvas_size(doc: &Value) -> Value {
    let meta = &doc["meta"];
    serde_json::json!({ "width": meta["width"], "height": meta["height"] })
}
fn set_canvas_size(doc: &mut Value, size: &Value) {
    for key in CANVAS_SIZE {
        doc["meta"][key] = size[key].clone();
    }
}
/// Folds the canvas size into a root transform conflict. The result starts with our
/// size, matching the default of keeping our transform.
fn link_canvas(
    result: &mut Value,
    ours: &Value,
    theirs: &Value,
    conflicts: &mut Vec<MergeConflict>,
) -> Option<CanvasLink> {
    let root = ours["root"].as_str()?;
    let transform = ["nodes", root, "props", "transform"];
    let conflict = conflicts.iter().find(|c| c.path == transform)?.id.clone();
    conflicts.retain(|c| {
        !(c.path.len() == 2 && c.path[0] == "meta" && CANVAS_SIZE.contains(&c.path[1].as_str()))
    });
    set_canvas_size(result, &canvas_size(ours));
    Some(CanvasLink {
        conflict,
        theirs: canvas_size(theirs),
    })
}
impl MergeSession {
    pub(super) fn roots(&self) -> impl Iterator<Item = ContentHash> {
        [Some(self.view.ours), Some(self.view.theirs), self.view.base]
            .into_iter()
            .flatten()
    }
}
#[derive(Clone, Debug, Serialize)]
pub struct MergeResult {
    pub kind: String,
    pub head: ContentHash,
    pub base: Option<ContentHash>,
}

fn conflict_kind(
    path: &[String],
    b: Option<&Value>,
    o: Option<&Value>,
    t: Option<&Value>,
) -> &'static str {
    if path.iter().any(|s| s == "tiles") {
        return "pixels";
    }
    if path.first().is_some_and(|s| s == "assets") {
        return "asset";
    }
    if o.is_none() || t.is_none() || b.is_none() {
        return "existence";
    }
    match path.last().map(String::as_str) {
        Some("parent") => "parent",
        Some("children") => "order",
        Some("bounds") => "geometry",
        Some("paths" | "content" | "adjustment") => "content",
        Some("kind") => "kind",
        _ if path.iter().any(|s| s == "tiles") => "pixels",
        _ if path.first().is_some_and(|s| s == "assets") => "asset",
        _ => "property",
    }
}
fn merge_value(
    b: Option<&Value>,
    o: Option<&Value>,
    t: Option<&Value>,
    path: &mut Vec<String>,
    conflicts: &mut Vec<MergeConflict>,
) -> Option<Value> {
    if o == t {
        return o.cloned();
    }
    if b == o {
        return t.cloned();
    }
    if b == t {
        return o.cloned();
    }
    let mut geometry = false;
    if let (Some(b), Some(o), Some(t)) = (b, o, t) {
        if b.get("kind").and_then(Value::as_str) == Some("raster") {
            geometry = (o.get("bounds") != b.get("bounds") && t.get("tiles") != b.get("tiles"))
                || (t.get("bounds") != b.get("bounds") && o.get("tiles") != b.get("tiles"));
        }
        let atomic = path.last().is_some_and(|s| {
            [
                "content",
                "paths",
                "effects",
                "selection",
                "transform",
                "vector_mask",
                "clip_path",
            ]
            .contains(&s.as_str())
        });
        let tag = |v: &Value| v.get("kind").and_then(Value::as_str).map(str::to_owned);
        let nested = |v: &Value| {
            v.get("kind")
                .and_then(|k| k.get("kind"))
                .and_then(Value::as_str)
                .map(str::to_owned)
        };
        let type_change = tag(o) != tag(b)
            || tag(t) != tag(b)
            || nested(o) != nested(b)
            || nested(t) != nested(b);
        if !geometry
            && !atomic
            && !type_change
            && let (Some(b), Some(o), Some(t)) = (b.as_object(), o.as_object(), t.as_object())
        {
            let keys: BTreeSet<_> = b.keys().chain(o.keys()).chain(t.keys()).cloned().collect();
            let mut result = serde_json::Map::new();
            for key in keys {
                path.push(key.clone());
                if let Some(v) = merge_value(b.get(&key), o.get(&key), t.get(&key), path, conflicts)
                {
                    result.insert(key, v);
                }
                path.pop();
            }
            return Some(Value::Object(result));
        }
    }
    conflicts.push(MergeConflict {
        id: conflicts.len().to_string(),
        kind: if geometry {
            "geometry"
        } else {
            conflict_kind(path, b, o, t)
        }
        .into(),
        path: path.clone(),
        base: MergeValue::of(b),
        ours: MergeValue::of(o),
        theirs: MergeValue::of(t),
    });
    o.cloned()
}
fn value(doc: &Document) -> Result<Value, EngineError> {
    let mut v = serde_json::to_value(doc).map_err(|e| invalid(e.to_string()))?;
    v["revision"] = Value::from(0);
    // The format is derived from the merged content, so it cannot conflict.
    v["meta"]["format_version"] = Value::from(alto_doc::BASE_FORMAT);
    Ok(v)
}
fn stash_label(message: &str) -> String {
    message
        .lines()
        .next()
        .unwrap_or("")
        .chars()
        .take(40)
        .collect()
}
fn result_doc(v: Value) -> Result<Document, EngineError> {
    let doc: Document =
        serde_json::from_value(v).map_err(|e| fail("INVALID_MERGE_RESULT", e.to_string()))?;
    doc.validate_graph()
        .map_err(|e| fail("INVALID_MERGE_RESULT", e.to_string()))?;
    Ok(doc)
}
fn structure(doc: &Value) -> Option<Value> {
    let nodes = doc.get("nodes")?.as_object()?;
    Some(Value::Object(
        nodes
            .iter()
            .map(|(id, node)| {
                let mut entry = serde_json::Map::new();
                entry.insert(
                    "parent".into(),
                    node.get("parent").cloned().unwrap_or(Value::Null),
                );
                if let Some(children) = node.get("kind").and_then(|kind| kind.get("children")) {
                    entry.insert("children".into(), children.clone());
                }
                (id.clone(), Value::Object(entry))
            })
            .collect(),
    ))
}
fn set_structure(doc: &mut Value, selected: &Value) -> Result<(), EngineError> {
    let entries = selected
        .as_object()
        .ok_or_else(|| fail("INVALID_ARGUMENT", "invalid structure"))?;
    let nodes = doc
        .get_mut("nodes")
        .and_then(Value::as_object_mut)
        .ok_or_else(|| fail("INVALID_MERGE_RESULT", "missing nodes"))?;
    if nodes.len() != entries.len() || nodes.keys().any(|id| !entries.contains_key(id)) {
        return Err(fail("INVALID_MERGE_RESULT", "structure nodes do not match"));
    }
    for (id, entry) in entries {
        let node = nodes.get_mut(id).unwrap();
        node["parent"] = entry
            .get("parent")
            .cloned()
            .ok_or_else(|| fail("INVALID_ARGUMENT", "missing parent"))?;
        match (
            node.get_mut("kind").and_then(Value::as_object_mut),
            entry.get("children"),
        ) {
            (Some(kind), Some(children)) if kind.contains_key("children") => {
                kind.insert("children".into(), children.clone());
            }
            (Some(kind), None) if !kind.contains_key("children") => {}
            _ => return Err(fail("INVALID_MERGE_RESULT", "structure node kind mismatch")),
        }
    }
    Ok(())
}
fn link_structure(
    result: &Value,
    base: Option<&Value>,
    ours: &Value,
    theirs: &Value,
    conflicts: &mut Vec<MergeConflict>,
) {
    if result_doc(result.clone()).is_ok() {
        return;
    }
    let Some((o, t)) = structure(ours).zip(structure(theirs)) else {
        return;
    };
    for candidate in [&o, &t] {
        let mut checked = result.clone();
        if set_structure(&mut checked, candidate).is_err() || result_doc(checked).is_err() {
            return;
        }
    }
    conflicts.retain(|c| {
        !(c.path.first().is_some_and(|p| p == "nodes")
            && matches!(
                c.path.last().map(String::as_str),
                Some("parent" | "children")
            ))
    });
    let id = conflicts
        .iter()
        .filter_map(|c| c.id.parse::<usize>().ok())
        .max()
        .map_or(0, |id| id + 1)
        .to_string();
    conflicts.push(MergeConflict {
        id,
        kind: "structure".into(),
        path: vec![],
        base: MergeValue::of(base.and_then(structure).as_ref()),
        ours: MergeValue::of(Some(&o)),
        theirs: MergeValue::of(Some(&t)),
    });
}

fn set_path(
    value: &mut Value,
    path: &[String],
    replacement: Option<Value>,
) -> Result<(), EngineError> {
    if path.is_empty() {
        *value = replacement.ok_or_else(|| fail("INVALID_ARGUMENT", "cannot delete document"))?;
        return Ok(());
    }
    let mut next = value;
    for key in &path[..path.len() - 1] {
        next = next
            .get_mut(key)
            .ok_or_else(|| fail("INVALID_MERGE_RESULT", "conflict path no longer exists"))?;
    }
    let map = next
        .as_object_mut()
        .ok_or_else(|| fail("INVALID_MERGE_RESULT", "conflict parent is not an object"))?;
    let key = path.last().unwrap();
    if let Some(v) = replacement {
        map.insert(key.clone(), v);
    } else {
        map.remove(key);
    }
    Ok(())
}
impl Engine {
    /// Applies a merged document to the working document as one undoable edit.
    fn apply_stash_result(
        &mut self,
        h: DocHandle,
        target: Document,
        message: &str,
        drop: bool,
        commit: ContentHash,
    ) -> Result<(), EngineError> {
        // Checked before the edit so a failed reference update cannot leave it applied.
        let generation = if drop {
            Some(
                self.versions(h)?
                    .refs
                    .generation
                    .checked_add(1)
                    .ok_or_else(|| fail("RESOURCE_LIMIT", "generation overflow"))?,
            )
        } else {
            None
        };
        if self.state_digest(h)? != alto_doc::digest::StateDigest::of(&target).value() {
            let current = self.document(h)?;
            let revision = current.revision;
            if revision == u64::MAX {
                return Err(fail("RESOURCE_LIMIT", "revision overflow"));
            }
            let ops =
                alto_doc::restore_ops(current, &target).map_err(|e| invalid(e.to_string()))?;
            let intent = alto_doc::Intent::labeled("version.stash_apply", stash_label(message));
            self.transact(h, intent, ops, revision)?;
        }
        let s = self.versions.get_mut(&h).unwrap();
        s.merge = None;
        if let Some(generation) = generation {
            s.refs.stashes.retain(|entry| entry.commit != commit);
            s.refs.generation = generation;
        }
        Ok(())
    }
    /// Validates a merge result as a document before it is applied.
    fn merge_result_document(&self, h: DocHandle, result: Value) -> Result<Document, EngineError> {
        let mut target = result_doc(result)?;
        if target.root != self.document(h)?.root {
            return Err(fail("INVALID_MERGE_RESULT", "document identity changed"));
        }
        for hash in doc_hashes(&target) {
            if !self.tiles.reachable(&hash) && !self.blobs.contains_key(&hash) {
                return Err(EngineError::MissingTile(hash));
            }
        }
        for node in target.nodes.values() {
            self.validate_node_content(node)?;
        }
        target.meta.format_version = target.required_format();
        Ok(target)
    }
    /// Merges a stash into the head. A clean result is applied at once and `None`
    /// returned; conflicts leave a stash-targeted session for `finish_merge`.
    pub fn stash_apply(
        &mut self,
        h: DocHandle,
        commit: ContentHash,
        drop: bool,
    ) -> Result<Option<MergePlan>, EngineError> {
        self.document(h)?;
        let ours = self.clean_head(h)?;
        let entry = self
            .versions(h)?
            .refs
            .stashes
            .iter()
            .find(|entry| entry.commit == commit)
            .cloned()
            .ok_or_else(|| fail("NOT_FOUND", "stash"))?;
        self.require_objects(h, &super::lazy::ObjectTarget::Stash { commit })?;
        let s = self.versions(h)?;
        let stash = s
            .commits
            .get(&commit)
            .ok_or_else(|| fail("CORRUPT_DATA", "stash commit is missing"))?;
        let base = stash.parents.first().copied();
        let o = self.resolve(h, VersionRef::Commit(ours))?;
        let t = self.resolve(h, VersionRef::Commit(commit))?;
        if ![o.meta.format_version, t.meta.format_version]
            .iter()
            .all(|format| alto_doc::SUPPORTED_FORMATS.contains(format))
        {
            return Err(fail("UNSUPPORTED", "document format"));
        }
        if o.root != t.root {
            return Err(fail("UNRELATED_HISTORY", "different document identity"));
        }
        let ov = value(&o)?;
        let tv = value(&t)?;
        let bv = match base {
            Some(base) => Some(value(&self.resolve(h, VersionRef::Commit(base))?)?),
            None => None,
        };
        let mut conflicts = vec![];
        let mut result = merge_value(
            bv.as_ref(),
            Some(&ov),
            Some(&tv),
            &mut vec![],
            &mut conflicts,
        )
        .ok_or_else(|| fail("INVALID_MERGE_RESULT", "stash merge produced no document"))?;
        let canvas = link_canvas(&mut result, &ov, &tv, &mut conflicts);
        link_structure(&result, bv.as_ref(), &ov, &tv, &mut conflicts);
        if conflicts.is_empty() && result_doc(result.clone()).is_err() {
            conflicts.push(MergeConflict {
                id: "0".into(),
                kind: "structure".into(),
                path: vec![],
                base: MergeValue::of(bv.as_ref()),
                ours: MergeValue::of(Some(&ov)),
                theirs: MergeValue::of(Some(&tv)),
            });
        }
        let plan = MergePlan {
            kind: "stash".into(),
            token: NodeId::new().0.to_string(),
            ours,
            theirs: commit,
            base,
            conflicts,
        };
        if plan.conflicts.is_empty() {
            let target = self.merge_result_document(h, result)?;
            self.apply_stash_result(h, target, &entry.message, drop, commit)?;
            return Ok(None);
        }
        let session = MergeSession {
            view: plan.clone(),
            target: MergeTarget::Stash { commit, drop },
            refs: self.versions_digest(h)?,
            revision: self.document(h)?.revision,
            working: ContentHash::of(&self.snapshot(h)?),
            result,
            canvas,
        };
        self.versions.get_mut(&h).unwrap().merge = Some(session);
        Ok(Some(plan))
    }
    pub fn prepare_merge(&mut self, h: DocHandle, name: &str) -> Result<MergePlan, EngineError> {
        let ours = self.clean_head(h)?;
        let name = super::branches::ref_name(name)?;
        self.require_objects(h, &super::lazy::ObjectTarget::Merge { name: name.clone() })?;
        let s = self.versions(h)?;
        if s.refs.current_branch.is_none() {
            return Err(fail("CONFLICT", "create a branch before merging"));
        }
        let theirs = s
            .refs
            .branches
            .get(&name)
            .ok_or_else(|| fail("NOT_FOUND", "branch"))?
            .head;
        let a = s.ancestors(ours)?;
        let b = s.ancestors(theirs)?;
        let mut base = None;
        let kind = if a.contains(&theirs) {
            "no_op"
        } else if b.contains(&ours) {
            "fast_forward"
        } else {
            let common: BTreeSet<_> = a.intersection(&b).copied().collect();
            if common.is_empty() {
                return Err(fail("UNRELATED_HISTORY", "no common ancestor"));
            }
            let mut best = common.clone();
            for c in &common {
                for parent in &s.commits[c].parents {
                    best.remove(parent);
                }
            }
            if best.len() != 1 {
                return Err(fail(
                    "MULTIPLE_MERGE_BASES",
                    "multiple best common ancestors",
                ));
            }
            base = best.first().copied();
            "three_way"
        };
        let o = self.resolve(h, VersionRef::Commit(ours))?;
        let t = self.resolve(h, VersionRef::Commit(theirs))?;
        if ![o.meta.format_version, t.meta.format_version]
            .iter()
            .all(|format| alto_doc::SUPPORTED_FORMATS.contains(format))
        {
            return Err(fail("UNSUPPORTED", "document format"));
        }
        if o.root != t.root {
            return Err(fail("UNRELATED_HISTORY", "different document identity"));
        }
        let ov = value(&o)?;
        let tv = value(&t)?;
        let mut conflicts = vec![];
        let mut canvas = None;
        let result = if let Some(base) = base {
            let bv = value(&self.resolve(h, VersionRef::Commit(base))?)?;
            let mut v =
                merge_value(Some(&bv), Some(&ov), Some(&tv), &mut vec![], &mut conflicts).unwrap();
            canvas = link_canvas(&mut v, &ov, &tv, &mut conflicts);
            link_structure(&v, Some(&bv), &ov, &tv, &mut conflicts);
            if conflicts.is_empty() && result_doc(v.clone()).is_err() {
                conflicts.push(MergeConflict {
                    id: "0".into(),
                    kind: "structure".into(),
                    path: vec![],
                    base: MergeValue::of(Some(&bv)),
                    ours: MergeValue::of(Some(&ov)),
                    theirs: MergeValue::of(Some(&tv)),
                });
            }
            v
        } else if kind == "fast_forward" {
            tv
        } else {
            ov
        };
        let plan = MergePlan {
            kind: kind.into(),
            token: NodeId::new().0.to_string(),
            ours,
            theirs,
            base,
            conflicts,
        };
        let session = MergeSession {
            view: plan.clone(),
            target: MergeTarget::Branch { name },
            refs: self.versions_digest(h)?,
            revision: self.document(h)?.revision,
            working: ContentHash::of(&self.snapshot(h)?),
            result,
            canvas,
        };
        self.versions.get_mut(&h).unwrap().merge = Some(session);
        Ok(plan)
    }
    pub fn cancel_merge(&mut self, h: DocHandle) -> Result<(), EngineError> {
        self.document(h)?;
        if let Some(s) = self.versions.get_mut(&h) {
            s.merge = None;
        }
        self.maintain();
        Ok(())
    }
    pub fn finish_merge(
        &mut self,
        h: DocHandle,
        token: &str,
        resolutions: Vec<MergeResolution>,
        message: &str,
        created_at: i64,
        author: Option<String>,
    ) -> Result<MergeResult, EngineError> {
        self.require_objects(h, &super::lazy::ObjectTarget::MergePlan)?;
        let s = self.versions(h)?;
        let plan = s
            .merge
            .clone()
            .ok_or_else(|| fail("STALE_MERGE", "merge plan expired"))?;
        let target_ok = match &plan.target {
            MergeTarget::Branch { name } => {
                s.refs.branches.get(name).map(|b| b.head) == Some(plan.view.theirs)
            }
            MergeTarget::Stash { commit, .. } => {
                plan.view.theirs == *commit
                    && s.refs.stashes.iter().any(|entry| entry.commit == *commit)
            }
        };
        if plan.view.token != token
            || plan.refs != self.versions_digest(h)?
            || plan.revision != self.document(h)?.revision
            || plan.working != ContentHash::of(&self.snapshot(h)?)
            || !target_ok
        {
            return Err(fail("STALE_MERGE", "document or references changed"));
        }
        let stash_message = match &plan.target {
            MergeTarget::Stash { commit, .. } => self
                .versions(h)?
                .commits
                .get(commit)
                .map(|c| c.message.clone())
                .unwrap_or_default(),
            MergeTarget::Branch { .. } => String::new(),
        };
        self.clean_head(h)?;
        let mut supplied = BTreeMap::new();
        for resolution in resolutions {
            if supplied
                .insert(resolution.conflict_id.clone(), resolution)
                .is_some()
            {
                return Err(fail("INVALID_ARGUMENT", "duplicate resolution"));
            }
        }
        if supplied.len() != plan.view.conflicts.len() {
            return Err(fail(
                "INVALID_ARGUMENT",
                "resolve every conflict exactly once",
            ));
        }
        let mut result = plan.result;
        for c in &plan.view.conflicts {
            let r = supplied
                .remove(&c.id)
                .ok_or_else(|| fail("INVALID_ARGUMENT", "missing resolution"))?;
            let selected = match r.choice.as_str() {
                "ours" => c.ours.clone(),
                "theirs" => c.theirs.clone(),
                "explicit" => r
                    .value
                    .ok_or_else(|| fail("INVALID_ARGUMENT", "missing explicit value"))?,
                _ => return Err(fail("INVALID_ARGUMENT", "unknown resolution choice")),
            };
            if c.kind == "structure" && c.path.is_empty() && selected.value.get("nodes").is_none() {
                set_structure(
                    &mut result,
                    &selected
                        .optional()
                        .ok_or_else(|| fail("INVALID_ARGUMENT", "missing structure"))?,
                )?;
            } else {
                set_path(&mut result, &c.path, selected.optional())?;
            }
            // An explicit transform keeps our size, the default the plan started with.
            if let Some(link) = plan.canvas.as_ref().filter(|l| l.conflict == c.id)
                && r.choice == "theirs"
            {
                set_canvas_size(&mut result, &link.theirs);
            }
        }
        if plan.view.kind == "no_op" {
            self.cancel_merge(h)?;
            return Ok(MergeResult {
                kind: "no_op".into(),
                head: plan.view.ours,
                base: None,
            });
        }
        let target = self.merge_result_document(h, result)?;
        let revision = self.document(h)?.revision;
        revision
            .checked_add(1)
            .ok_or_else(|| fail("RESOURCE_LIMIT", "revision overflow"))?;
        let (kind, head) = match &plan.target {
            MergeTarget::Branch { .. } => {
                let snapshot = target.snapshot_bytes()?;
                let commit = if plan.view.kind == "three_way" {
                    let c = Commit {
                        format: 2,
                        parents: vec![plan.view.ours, plan.view.theirs],
                        snapshot: ContentHash::of(&snapshot),
                        message: message.trim_end().into(),
                        intents: vec![],
                        created_at,
                        author,
                        merge_base: plan.view.base,
                    };
                    c.validate().map_err(invalid)?;
                    Some(c)
                } else {
                    None
                };
                let s = self.versions.get_mut(&h).unwrap();
                let head = commit
                    .as_ref()
                    .map(Commit::hash)
                    .unwrap_or(plan.view.theirs);
                let generation = s
                    .refs
                    .generation
                    .checked_add(1)
                    .ok_or_else(|| fail("RESOURCE_LIMIT", "generation overflow"))?;
                if let Some(c) = commit {
                    let bytes = c.bytes();
                    s.insert(head, c, bytes, snapshot)?;
                }
                let current = s
                    .refs
                    .current_branch
                    .clone()
                    .ok_or_else(|| fail("STALE_MERGE", "detached"))?;
                s.refs.branches.get_mut(&current).unwrap().head = head;
                s.refs.generation = generation;
                s.set_head(Some(head));
                s.merge = None;
                self.install_version_document(h, target, revision)?;
                (
                    if plan.view.kind == "fast_forward" {
                        "fast_forward"
                    } else {
                        "committed"
                    },
                    head,
                )
            }
            MergeTarget::Stash { commit, drop } => {
                // No commit and no branch or head movement: the stash result is an
                // undoable working edit, and only a successful finish drops it.
                self.apply_stash_result(h, target, &stash_message, *drop, *commit)?;
                ("stash", plan.view.ours)
            }
        };
        Ok(MergeResult {
            kind: kind.into(),
            head,
            base: plan.view.base,
        })
    }
}

#[cfg(test)]
mod structure_tests {
    use super::*;
    use alto_doc::{DocumentMeta, Node, Op};

    #[test]
    fn branch_and_stash_resolve_opposite_reparenting() {
        for stash in [false, true] {
            let mut e = Engine::new();
            let h = e.create_document(DocumentMeta::srgb_u8(16, 16)).unwrap();
            let root = e.document(h).unwrap().root;
            let a = Node::group("A");
            let b = Node::group("B");
            let (aid, bid) = (a.id, b.id);
            e.transact(
                h,
                "groups",
                vec![
                    Op::InsertNode {
                        node: a,
                        parent: root,
                        index: 0,
                    },
                    Op::InsertNode {
                        node: b,
                        parent: root,
                        index: 1,
                    },
                ],
                0,
            )
            .unwrap();
            e.commit(h, "base", 0, None, false).unwrap();
            if !stash {
                e.create_branch(h, "feature", None, true).unwrap();
            }
            let rev = e.document(h).unwrap().revision;
            e.transact(
                h,
                "move A",
                vec![Op::MoveNode {
                    id: aid,
                    parent: bid,
                    index: 0,
                }],
                rev,
            )
            .unwrap();
            let stash_entry = if stash {
                Some(e.stash(h, "move A", 1, None).unwrap())
            } else {
                e.commit(h, "feature", 1, None, false).unwrap();
                e.checkout(h, "main").unwrap();
                None
            };
            let rev = e.document(h).unwrap().revision;
            e.transact(
                h,
                "move B",
                vec![Op::MoveNode {
                    id: bid,
                    parent: aid,
                    index: 0,
                }],
                rev,
            )
            .unwrap();
            e.commit(h, "ours", 2, None, false).unwrap();
            let plan = if let Some(entry) = stash_entry {
                e.stash_apply(h, entry.commit, false).unwrap().unwrap()
            } else {
                e.prepare_merge(h, "feature").unwrap()
            };
            assert_eq!(plan.conflicts.len(), 1);
            assert_eq!(plan.conflicts[0].kind, "structure");
            e.finish_merge(
                h,
                &plan.token,
                vec![MergeResolution {
                    conflict_id: plan.conflicts[0].id.clone(),
                    choice: "theirs".into(),
                    value: None,
                }],
                "merge",
                3,
                None,
            )
            .unwrap();
            assert_eq!(e.document(h).unwrap().node(aid).unwrap().parent, Some(bid));
            e.document(h).unwrap().validate_graph().unwrap();
        }
    }

    #[test]
    fn opposing_reparents_offer_repair_without_losing_other_fields() {
        let mut engine = Engine::new();
        let h = engine
            .create_document(DocumentMeta::srgb_u8(16, 16))
            .unwrap();
        let root = engine.document(h).unwrap().root;
        let a = Node::group("A");
        let b = Node::group("B");
        let (aid, bid) = (a.id, b.id);
        engine
            .transact(
                h,
                "groups",
                vec![
                    Op::InsertNode {
                        node: a,
                        parent: root,
                        index: 0,
                    },
                    Op::InsertNode {
                        node: b,
                        parent: root,
                        index: 1,
                    },
                ],
                0,
            )
            .unwrap();
        let base = value(engine.document(h).unwrap()).unwrap();
        let rev = engine.document(h).unwrap().revision;
        engine
            .transact(
                h,
                "move A",
                vec![Op::MoveNode {
                    id: aid,
                    parent: bid,
                    index: 0,
                }],
                rev,
            )
            .unwrap();
        let ours = value(engine.document(h).unwrap()).unwrap();
        engine.undo(h).unwrap();
        let rev = engine.document(h).unwrap().revision;
        engine
            .transact(
                h,
                "move B",
                vec![Op::MoveNode {
                    id: bid,
                    parent: aid,
                    index: 0,
                }],
                rev,
            )
            .unwrap();
        let mut theirs = value(engine.document(h).unwrap()).unwrap();
        theirs["nodes"][aid.0.to_string()]["name"] = Value::String("changed".into());
        let mut conflicts = vec![];
        let merged = merge_value(
            Some(&base),
            Some(&ours),
            Some(&theirs),
            &mut vec![],
            &mut conflicts,
        )
        .unwrap();
        assert!(result_doc(merged.clone()).is_err());
        link_structure(&merged, Some(&base), &ours, &theirs, &mut conflicts);
        let repair = conflicts.iter().find(|c| c.kind == "structure").unwrap();
        for choice in [&repair.ours, &repair.theirs] {
            let mut result = merged.clone();
            set_structure(&mut result, &choice.value).unwrap();
            let doc = result_doc(result).unwrap();
            assert_eq!(doc.nodes[&aid].name, "changed");
        }
    }
}
