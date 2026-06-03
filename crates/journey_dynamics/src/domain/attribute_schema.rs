//! Schema types that describe how journey attributes are classified for privacy
//! purposes, and a pure classification function that routes a flat change map
//! into plaintext and per-subject secret buckets.

use std::collections::BTreeMap;

use serde_json::Value;
use uuid::Uuid;

use super::AttributePath;

// ── PiiClass ─────────────────────────────────────────────────────────────────

/// Privacy classification for a single attribute path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PiiClass {
    /// The value may be stored and indexed in plaintext.
    Plaintext,
    /// The value must be encrypted under the DEK belonging to `subject`.
    /// `subject` is itself an `AttributePath` that points to the slot name
    /// (e.g. `"persons/0"`) whose `subject_id` field provides the DEK key.
    Secret { subject: AttributePath },
}

// ── AttributeSchema ───────────────────────────────────────────────────────────

/// Schema that maps known attribute paths to their [`PiiClass`].
///
/// An `AttributeSchema` can be constructed in two modes:
///
/// - **Explicit** (`new`): only the provided paths are known; anything else is
///   treated as unknown by [`classify`](Self::classify).
/// - **Permissive** (`permissive`): every path is treated as `Plaintext`
///   regardless of whether it is listed. Useful for tests and bootstrap
///   contexts where the caller does not yet have a real schema.
pub struct AttributeSchema {
    paths: BTreeMap<AttributePath, PiiClass>,
    json_schema: Option<Value>,
    /// When `true`, [`classify`](Self::classify) returns `Some(Plaintext)` for
    /// any path not listed in `paths`.
    permissive: bool,
}

/// A static `Plaintext` sentinel returned by reference in permissive mode.
static PLAINTEXT: PiiClass = PiiClass::Plaintext;

impl AttributeSchema {
    /// Construct a schema from an explicit path-to-class map and an optional
    /// JSON Schema used for structural validation.
    #[must_use]
    pub const fn new(paths: BTreeMap<AttributePath, PiiClass>, json_schema: Option<Value>) -> Self {
        Self {
            paths,
            json_schema,
            permissive: false,
        }
    }

    /// Construct a permissive schema that classifies every path as
    /// [`PiiClass::Plaintext`] and performs no JSON Schema validation.
    ///
    /// Suitable for tests and for the default binary bootstrap where a real
    /// schema has not yet been wired in.
    #[must_use]
    pub const fn permissive() -> Self {
        Self {
            paths: BTreeMap::new(),
            json_schema: None,
            permissive: true,
        }
    }

    /// Classify a single path.
    ///
    /// Returns `None` when the path is not known and the schema is not
    /// permissive.  In permissive mode always returns
    /// `Some(&PiiClass::Plaintext)`.
    #[must_use]
    pub fn classify(&self, path: &AttributePath) -> Option<&PiiClass> {
        if self.permissive {
            Some(self.paths.get(path).unwrap_or(&PLAINTEXT))
        } else {
            self.paths.get(path)
        }
    }

    /// Returns the JSON Schema value if one was provided.
    #[must_use]
    pub const fn json_schema(&self) -> Option<&Value> {
        self.json_schema.as_ref()
    }

    /// Iterates over all explicitly registered paths.
    pub fn known_paths(&self) -> impl Iterator<Item = &AttributePath> {
        self.paths.keys()
    }
}

// ── Classification ────────────────────────────────────────────────────────────

/// The output of [`classify_changes`]: a flat batch of attribute changes split
/// by their privacy classification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Classification {
    /// Changes that may be stored in plaintext.
    pub plaintext: BTreeMap<AttributePath, Value>,
    /// Changes grouped by subject UUID — each group must be encrypted under
    /// that subject's DEK.
    pub secret_by_subject: BTreeMap<Uuid, BTreeMap<AttributePath, Value>>,
    /// Paths that are neither in the schema nor handled by permissive mode.
    /// Also includes secret paths whose subject UUID could not be resolved by
    /// the caller-supplied lookup.  The caller decides how to react (typically
    /// an error).
    pub unknown: Vec<AttributePath>,
}

/// Classify a flat map of attribute changes against `schema`.
///
/// `subject_lookup` resolves a *subject path* (e.g. `"persons/0"`) to the
/// `Uuid` of the underlying data-subject.  When the lookup returns `None` for
/// a secret path, that path is routed to [`Classification::unknown`].
pub fn classify_changes(
    schema: &AttributeSchema,
    changes: &BTreeMap<AttributePath, Value>,
    subject_lookup: impl Fn(&AttributePath) -> Option<Uuid>,
) -> Classification {
    let mut plaintext: BTreeMap<AttributePath, Value> = BTreeMap::new();
    let mut secret_by_subject: BTreeMap<Uuid, BTreeMap<AttributePath, Value>> = BTreeMap::new();
    let mut unknown: Vec<AttributePath> = Vec::new();

    for (path, value) in changes {
        match schema.classify(path) {
            None => {
                unknown.push(path.clone());
            }
            Some(PiiClass::Plaintext) => {
                plaintext.insert(path.clone(), value.clone());
            }
            Some(PiiClass::Secret { subject }) => match subject_lookup(subject) {
                None => {
                    unknown.push(path.clone());
                }
                Some(uuid) => {
                    secret_by_subject
                        .entry(uuid)
                        .or_default()
                        .insert(path.clone(), value.clone());
                }
            },
        }
    }

    Classification {
        plaintext,
        secret_by_subject,
        unknown,
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn path(s: &str) -> AttributePath {
        s.parse().unwrap()
    }

    fn subject_a() -> Uuid {
        Uuid::parse_str("00000000-0000-0000-0000-000000000001").unwrap()
    }

    fn subject_b() -> Uuid {
        Uuid::parse_str("00000000-0000-0000-0000-000000000002").unwrap()
    }

    fn simple_schema() -> AttributeSchema {
        let mut paths = BTreeMap::new();
        paths.insert(path("search/origin"), PiiClass::Plaintext);
        paths.insert(path("search/destination"), PiiClass::Plaintext);
        paths.insert(
            path("persons/0/passport"),
            PiiClass::Secret {
                subject: path("persons/0"),
            },
        );
        paths.insert(
            path("persons/1/passport"),
            PiiClass::Secret {
                subject: path("persons/1"),
            },
        );
        AttributeSchema::new(paths, None)
    }

    fn lookup_both(subject_path: &AttributePath) -> Option<Uuid> {
        match subject_path.as_str() {
            "persons/0" => Some(subject_a()),
            "persons/1" => Some(subject_b()),
            _ => None,
        }
    }

    // ── all plaintext ─────────────────────────────────────────────────────

    #[test]
    fn all_plaintext_changes_land_in_plaintext_bucket() {
        let schema = simple_schema();
        let mut changes = BTreeMap::new();
        changes.insert(path("search/origin"), json!("LHR"));
        changes.insert(path("search/destination"), json!("JFK"));

        let result = classify_changes(&schema, &changes, lookup_both);

        assert_eq!(result.plaintext.len(), 2);
        assert!(result.secret_by_subject.is_empty());
        assert!(result.unknown.is_empty());
        assert_eq!(result.plaintext[&path("search/origin")], json!("LHR"));
    }

    // ── mixed plaintext + single secret subject ───────────────────────────

    #[test]
    fn mixed_changes_split_correctly_for_one_subject() {
        let schema = simple_schema();
        let mut changes = BTreeMap::new();
        changes.insert(path("search/origin"), json!("LHR"));
        changes.insert(path("persons/0/passport"), json!("AB123456"));

        let result = classify_changes(&schema, &changes, lookup_both);

        assert_eq!(result.plaintext.len(), 1);
        assert_eq!(result.secret_by_subject.len(), 1);
        assert!(result.unknown.is_empty());
        let slot = result.secret_by_subject.get(&subject_a()).unwrap();
        assert_eq!(slot[&path("persons/0/passport")], json!("AB123456"));
    }

    // ── two subjects in one batch ─────────────────────────────────────────

    #[test]
    fn two_subjects_produce_two_keys_in_secret_map() {
        let schema = simple_schema();
        let mut changes = BTreeMap::new();
        changes.insert(path("persons/0/passport"), json!("AB111111"));
        changes.insert(path("persons/1/passport"), json!("CD222222"));

        let result = classify_changes(&schema, &changes, lookup_both);

        assert!(result.plaintext.is_empty());
        assert_eq!(result.secret_by_subject.len(), 2);
        assert!(result.unknown.is_empty());
        assert!(result.secret_by_subject.contains_key(&subject_a()));
        assert!(result.secret_by_subject.contains_key(&subject_b()));
    }

    // ── unknown path ──────────────────────────────────────────────────────

    #[test]
    fn unknown_path_lands_in_unknown_bucket_and_is_not_lost() {
        let schema = simple_schema();
        let mut changes = BTreeMap::new();
        changes.insert(path("search/origin"), json!("LHR"));
        changes.insert(path("mystery/field"), json!("surprise"));

        let result = classify_changes(&schema, &changes, lookup_both);

        assert_eq!(result.plaintext.len(), 1);
        assert!(result.secret_by_subject.is_empty());
        assert_eq!(result.unknown, vec![path("mystery/field")]);
    }

    // ── secret path with no resolvable subject ────────────────────────────

    #[test]
    fn secret_with_unresolvable_subject_lands_in_unknown() {
        let schema = simple_schema();
        let mut changes = BTreeMap::new();
        changes.insert(path("persons/0/passport"), json!("AB123456"));

        // Lookup always returns None.
        let result = classify_changes(&schema, &changes, |_| None);

        assert!(result.plaintext.is_empty());
        assert!(result.secret_by_subject.is_empty());
        assert_eq!(result.unknown, vec![path("persons/0/passport")]);
    }

    // ── permissive schema ─────────────────────────────────────────────────

    #[test]
    fn permissive_schema_classifies_all_paths_as_plaintext() {
        let schema = AttributeSchema::permissive();
        let mut changes = BTreeMap::new();
        changes.insert(path("anything/at/all"), json!(42));
        changes.insert(path("another/one"), json!(true));

        let result = classify_changes(&schema, &changes, |_| None);

        assert_eq!(result.plaintext.len(), 2);
        assert!(result.secret_by_subject.is_empty());
        assert!(result.unknown.is_empty());
    }
}
