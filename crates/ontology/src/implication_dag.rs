use std::collections::{HashMap, HashSet, VecDeque};

use mimisbrunnr_types::TagId;

use crate::tag_def::TagDefinition;
use crate::OntologyError;

/// Directed Acyclic Graph of tag implications.
///
/// If tag A implies tag B (e.g., "car" → "vehicle"), then any object tagged
/// with A is also considered to have tag B. The DAG stores these relationships
/// and computes transitive closures for materialization.
#[derive(Clone)]
pub struct ImplicationDag {
    /// Tag definitions by ID.
    definitions: HashMap<TagId, TagDefinition>,
    /// Tag name → ID lookup.
    name_to_id: HashMap<String, TagId>,
    /// Forward edges: tag → tags it implies (parent direction).
    implies: HashMap<TagId, Vec<TagId>>,
    /// Reverse edges: tag → tags that imply it (child direction).
    implied_by: HashMap<TagId, Vec<TagId>>,
    /// Next tag ID to assign.
    next_tag_id: u32,
}

impl ImplicationDag {
    pub fn new() -> Self {
        Self {
            definitions: HashMap::new(),
            name_to_id: HashMap::new(),
            implies: HashMap::new(),
            implied_by: HashMap::new(),
            next_tag_id: 1,
        }
    }

    /// Register a new tag, returning its ID.
    pub fn register_tag(&mut self, def: TagDefinition) -> Result<TagId, OntologyError> {
        if self.name_to_id.contains_key(&def.name) {
            return Err(OntologyError::DuplicateTagName(def.name.clone()));
        }

        let id = def.id;
        if id.raw() >= self.next_tag_id {
            self.next_tag_id = id.raw() + 1;
        }

        // Register implications
        for &target in &def.implies {
            self.add_implication_unchecked(id, target);
        }

        self.name_to_id.insert(def.name.clone(), id);
        self.definitions.insert(id, def);
        Ok(id)
    }

    /// Add an implication: `from` implies `to` (e.g., "car" → "vehicle").
    pub fn add_implication(&mut self, from: TagId, to: TagId) -> Result<(), OntologyError> {
        // Check for cycles: would adding from→to create a path to→...→from?
        if self.is_reachable(to, from) {
            return Err(OntologyError::CycleDetected(from, to));
        }
        self.add_implication_unchecked(from, to);
        Ok(())
    }

    fn add_implication_unchecked(&mut self, from: TagId, to: TagId) {
        self.implies.entry(from).or_default().push(to);
        self.implied_by.entry(to).or_default().push(from);
    }

    /// Check if `from` can reach `to` via implication edges (BFS).
    pub fn is_reachable(&self, from: TagId, to: TagId) -> bool {
        if from == to {
            return true;
        }
        let mut visited = HashSet::new();
        let mut queue = VecDeque::new();
        queue.push_back(from);
        visited.insert(from);

        while let Some(current) = queue.pop_front() {
            if let Some(targets) = self.implies.get(&current) {
                for &target in targets {
                    if target == to {
                        return true;
                    }
                    if visited.insert(target) {
                        queue.push_back(target);
                    }
                }
            }
        }
        false
    }

    /// Compute the transitive closure of implications from a tag.
    /// Returns all tags that should be materialized when this tag is applied.
    pub fn transitive_closure(&self, tag: TagId) -> Vec<TagId> {
        let mut result = Vec::new();
        let mut visited = HashSet::new();
        let mut queue = VecDeque::new();

        if let Some(targets) = self.implies.get(&tag) {
            for &t in targets {
                if visited.insert(t) {
                    queue.push_back(t);
                }
            }
        }

        while let Some(current) = queue.pop_front() {
            result.push(current);
            if let Some(targets) = self.implies.get(&current) {
                for &t in targets {
                    if visited.insert(t) {
                        queue.push_back(t);
                    }
                }
            }
        }

        result
    }

    /// Get all tags that imply the given tag (descendants in the DAG).
    /// Used for IsA queries: "vehicle" includes "car", "truck", etc.
    pub fn descendants(&self, tag: TagId) -> Vec<TagId> {
        let mut result = Vec::new();
        let mut visited = HashSet::new();
        let mut queue = VecDeque::new();

        if let Some(children) = self.implied_by.get(&tag) {
            for &c in children {
                if visited.insert(c) {
                    queue.push_back(c);
                }
            }
        }

        while let Some(current) = queue.pop_front() {
            result.push(current);
            if let Some(children) = self.implied_by.get(&current) {
                for &c in children {
                    if visited.insert(c) {
                        queue.push_back(c);
                    }
                }
            }
        }

        result
    }

    /// Get a tag definition by ID.
    pub fn get(&self, id: TagId) -> Option<&TagDefinition> {
        self.definitions.get(&id)
    }

    /// Look up a tag ID by name.
    pub fn lookup(&self, name: &str) -> Option<TagId> {
        self.name_to_id.get(name).copied()
    }

    /// Get direct implications for a tag.
    pub fn direct_implies(&self, tag: TagId) -> &[TagId] {
        self.implies.get(&tag).map(|v| v.as_slice()).unwrap_or(&[])
    }

    /// Allocate a fresh TagId.
    pub fn alloc_tag_id(&mut self) -> TagId {
        let id = TagId::new(self.next_tag_id);
        self.next_tag_id += 1;
        id
    }

    /// Number of registered tags.
    pub fn tag_count(&self) -> usize {
        self.definitions.len()
    }

    /// All registered tag IDs.
    pub fn all_tags(&self) -> Vec<TagId> {
        self.definitions.keys().copied().collect()
    }
}

impl Default for ImplicationDag {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tag_def::TagSemantics;

    fn tag(id: u32) -> TagId {
        TagId::new(id)
    }

    fn label(id: u32, name: &str) -> TagDefinition {
        TagDefinition::new(tag(id), name, TagSemantics::Label)
    }

    #[test]
    fn register_and_lookup() {
        let mut dag = ImplicationDag::new();
        dag.register_tag(label(1, "electronic")).unwrap();

        assert_eq!(dag.lookup("electronic"), Some(tag(1)));
        assert_eq!(dag.tag_count(), 1);
    }

    #[test]
    fn duplicate_name_rejected() {
        let mut dag = ImplicationDag::new();
        dag.register_tag(label(1, "electronic")).unwrap();
        assert!(dag.register_tag(label(2, "electronic")).is_err());
    }

    #[test]
    fn simple_implication() {
        let mut dag = ImplicationDag::new();
        dag.register_tag(label(1, "car")).unwrap();
        dag.register_tag(label(2, "vehicle")).unwrap();
        dag.add_implication(tag(1), tag(2)).unwrap();

        let closure = dag.transitive_closure(tag(1));
        assert_eq!(closure, vec![tag(2)]);
    }

    #[test]
    fn transitive_implication() {
        let mut dag = ImplicationDag::new();
        dag.register_tag(label(1, "car")).unwrap();
        dag.register_tag(label(2, "vehicle")).unwrap();
        dag.register_tag(label(3, "physical_object")).unwrap();

        dag.add_implication(tag(1), tag(2)).unwrap(); // car → vehicle
        dag.add_implication(tag(2), tag(3)).unwrap(); // vehicle → physical_object

        let closure = dag.transitive_closure(tag(1));
        assert_eq!(closure.len(), 2);
        assert!(closure.contains(&tag(2)));
        assert!(closure.contains(&tag(3)));
    }

    #[test]
    fn cycle_detection() {
        let mut dag = ImplicationDag::new();
        dag.register_tag(label(1, "a")).unwrap();
        dag.register_tag(label(2, "b")).unwrap();
        dag.register_tag(label(3, "c")).unwrap();

        dag.add_implication(tag(1), tag(2)).unwrap();
        dag.add_implication(tag(2), tag(3)).unwrap();

        // Adding 3→1 would create a cycle
        assert!(matches!(
            dag.add_implication(tag(3), tag(1)),
            Err(OntologyError::CycleDetected(_, _))
        ));
    }

    #[test]
    fn descendants() {
        let mut dag = ImplicationDag::new();
        dag.register_tag(label(1, "car")).unwrap();
        dag.register_tag(label(2, "truck")).unwrap();
        dag.register_tag(label(3, "vehicle")).unwrap();

        dag.add_implication(tag(1), tag(3)).unwrap(); // car → vehicle
        dag.add_implication(tag(2), tag(3)).unwrap(); // truck → vehicle

        let desc = dag.descendants(tag(3));
        assert_eq!(desc.len(), 2);
        assert!(desc.contains(&tag(1)));
        assert!(desc.contains(&tag(2)));
    }

    #[test]
    fn diamond_implication() {
        let mut dag = ImplicationDag::new();
        dag.register_tag(label(1, "electric_car")).unwrap();
        dag.register_tag(label(2, "car")).unwrap();
        dag.register_tag(label(3, "electric")).unwrap();
        dag.register_tag(label(4, "vehicle")).unwrap();

        dag.add_implication(tag(1), tag(2)).unwrap(); // electric_car → car
        dag.add_implication(tag(1), tag(3)).unwrap(); // electric_car → electric
        dag.add_implication(tag(2), tag(4)).unwrap(); // car → vehicle
        dag.add_implication(tag(3), tag(4)).unwrap(); // electric → vehicle

        let closure = dag.transitive_closure(tag(1));
        // Should contain car, electric, vehicle (vehicle only once)
        assert_eq!(closure.len(), 3);
        assert!(closure.contains(&tag(2)));
        assert!(closure.contains(&tag(3)));
        assert!(closure.contains(&tag(4)));
    }

    #[test]
    fn register_with_implies() {
        let mut dag = ImplicationDag::new();
        dag.register_tag(label(2, "vehicle")).unwrap();

        let car = label(1, "car").with_implies(vec![tag(2)]);
        dag.register_tag(car).unwrap();

        let closure = dag.transitive_closure(tag(1));
        assert_eq!(closure, vec![tag(2)]);
    }

    #[test]
    fn no_implications() {
        let mut dag = ImplicationDag::new();
        dag.register_tag(label(1, "standalone")).unwrap();

        let closure = dag.transitive_closure(tag(1));
        assert!(closure.is_empty());
    }
}
