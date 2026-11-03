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
pub(super) struct MergeSession {
    view: MergePlan,
    target_branch: String,
    refs: ContentHash,
    revision: u64,
    working: ContentHash,
    result: Value,
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
    Ok(v)
}
fn result_doc(v: Value) -> Result<Document, EngineError> {
    let doc: Document =
        serde_json::from_value(v).map_err(|e| fail("INVALID_MERGE_RESULT", e.to_string()))?;
    doc.validate_graph()
        .map_err(|e| fail("INVALID_MERGE_RESULT", e.to_string()))?;
    Ok(doc)
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
        if o.root != t.root {
            return Err(fail("UNRELATED_HISTORY", "different document identity"));
        }
        let ov = value(&o)?;
        let tv = value(&t)?;
        let mut conflicts = vec![];
        let result = if let Some(base) = base {
            let bv = value(&self.resolve(h, VersionRef::Commit(base))?)?;
            let v =
                merge_value(Some(&bv), Some(&ov), Some(&tv), &mut vec![], &mut conflicts).unwrap();
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
            target_branch: name,
            refs: self.versions_digest(h)?,
            revision: self.document(h)?.revision,
            working: ContentHash::of(&self.snapshot(h)?),
            result,
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
        if plan.view.token != token
            || plan.refs != self.versions_digest(h)?
            || plan.revision != self.document(h)?.revision
            || plan.working != ContentHash::of(&self.snapshot(h)?)
            || s.refs.branches.get(&plan.target_branch).map(|b| b.head) != Some(plan.view.theirs)
        {
            return Err(fail("STALE_MERGE", "document or references changed"));
        }
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
            set_path(&mut result, &c.path, selected.optional())?;
        }
        if plan.view.kind == "no_op" {
            self.cancel_merge(h)?;
            return Ok(MergeResult {
                kind: "no_op".into(),
                head: plan.view.ours,
                base: None,
            });
        }
        let target = result_doc(result)?;
        if target.root != self.document(h)?.root || target.meta.format_version != 5 {
            return Err(fail(
                "INVALID_MERGE_RESULT",
                "document identity or format changed",
            ));
        }
        for hash in doc_hashes(&target) {
            if !self.tiles.reachable(&hash) && !self.blobs.contains_key(&hash) {
                return Err(EngineError::MissingTile(hash));
            }
        }
        for node in target.nodes.values() {
            self.validate_node_content(node)?;
        }
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
        let revision = self.document(h)?.revision;
        revision
            .checked_add(1)
            .ok_or_else(|| fail("RESOURCE_LIMIT", "revision overflow"))?;
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
        Ok(MergeResult {
            kind: if plan.view.kind == "fast_forward" {
                "fast_forward"
            } else {
                "committed"
            }
            .into(),
            head,
            base: plan.view.base,
        })
    }
}
