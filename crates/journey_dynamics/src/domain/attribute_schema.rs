//! Schema types that describe how journey attributes are classified for privacy
//! purposes, and a pure classification function that routes a flat change map
//! into plaintext and per-subject secret buckets.

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use jsonptr::PointerBuf;

// ── PiiClass ─────────────────────────────────────────────────────────────────

/// Privacy classification for a single attribute path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PiiClass {
    /// The value may be stored and indexed in plaintext.
    Plaintext,
    /// The value must be encrypted under the DEK belonging to `subject`.
    /// `subject` is a `PointerBuf` that points to the slot name
    /// (e.g. `/persons/0`) whose `subject_id` field provides the DEK key.
    Secret { subject: PointerBuf },
}

// ── NamespacePattern ──────────────────────────────────────────────────────────

/// A prefix-based classification rule for dynamic three-segment paths of the
/// form `<namespace>/<ref>/<field>`.
///
/// For example, with `namespace = "persons"`:
/// - `/persons/passenger_0/firstName` → `Secret { subject: "/persons/passenger_0" }`
///   (if `"firstName"` is in `secret_fields`)
/// - `/persons/passenger_0/passengerType` → `Plaintext`
///   (if `"passengerType"` is in `plaintext_fields`)
///
/// Any field not listed in either set is treated as unknown (or falls through
/// to permissive mode if the schema is permissive).
#[derive(Debug, Clone)]
pub struct NamespacePattern {
    /// First path segment, e.g. `"persons"`.
    pub namespace: String,
    /// Leaf field names classified as `Secret` under `<namespace>/<ref>`.
    pub secret_fields: BTreeSet<String>,
    /// Leaf field names classified as `Plaintext`.
    pub plaintext_fields: BTreeSet<String>,
}

// ── AttributeSchema ───────────────────────────────────────────────────────────

/// Schema that maps known attribute paths to their [`PiiClass`].
///
/// An `AttributeSchema` can be constructed in two modes:
///
/// - **Explicit** (`new`): only the provided paths are known; anything else is
///   treated as unknown by [`classify`](Self::classify).
/// - **Permissive** (`permissive`): every path not matched by an exact entry or
///   namespace pattern is treated as `Plaintext`. Useful for tests and bootstrap
///   contexts where the caller does not yet have a real schema.
///
/// Namespace patterns (see [`NamespacePattern`]) can be layered on top of
/// either mode via [`with_namespace_patterns`](Self::with_namespace_patterns).
pub struct AttributeSchema {
    paths: BTreeMap<PointerBuf, PiiClass>,
    json_schema: Option<Value>,
    /// When `true`, [`classify`](Self::classify) returns `Some(Plaintext)` for
    /// any path not matched by an exact entry, namespace pattern, or prefix.
    permissive: bool,
    /// Prefix-based rules applied when the exact path lookup misses.
    namespace_patterns: Vec<NamespacePattern>,
    /// Top-level path segments whose subtrees are unconditionally `Plaintext`.
    /// Checked after namespace patterns and before the permissive fallback.
    /// Example: `["search", "booking"]` classifies `search/origin`,
    /// `booking/pricing/totalPrice`, etc. as plaintext without listing every
    /// individual path.
    plaintext_prefixes: Vec<String>,
}

/// A static `Plaintext` sentinel returned by reference in permissive/plaintext mode.
static PLAINTEXT: PiiClass = PiiClass::Plaintext;

impl AttributeSchema {
    /// Construct a schema from an explicit path-to-class map and an optional
    /// JSON Schema used for structural validation.
    #[must_use]
    pub const fn new(paths: BTreeMap<PointerBuf, PiiClass>, json_schema: Option<Value>) -> Self {
        Self {
            paths,
            json_schema,
            permissive: false,
            namespace_patterns: Vec::new(),
            plaintext_prefixes: Vec::new(),
        }
    }

    /// Construct a permissive schema that classifies every unmatched path as
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
            namespace_patterns: Vec::new(),
            plaintext_prefixes: Vec::new(),
        }
    }

    /// Attach namespace patterns to this schema.
    ///
    /// Patterns are tried in order after the exact-path lookup misses and
    /// before the plaintext-prefix and permissive steps.
    #[must_use]
    pub fn with_namespace_patterns(mut self, patterns: Vec<NamespacePattern>) -> Self {
        self.namespace_patterns = patterns;
        self
    }

    /// Attach top-level plaintext-prefix rules to this schema.
    ///
    /// Any path whose first segment appears in `prefixes` is classified as
    /// [`PiiClass::Plaintext`], regardless of depth. Checked after namespace
    /// patterns and before the permissive fallback.
    #[must_use]
    pub fn with_plaintext_prefixes(mut self, prefixes: Vec<String>) -> Self {
        self.plaintext_prefixes = prefixes;
        self
    }

    /// Classify a single path.
    ///
    /// Resolution order:
    /// 1. Exact match in `paths`.
    /// 2. Namespace pattern match for three-segment paths (`namespace/ref/field`).
    /// 3. Plaintext prefix — `Some(Plaintext)` if the first segment is listed.
    /// 4. Permissive fallback — `Some(Plaintext)` if the schema is permissive.
    /// 5. `None` — path is unknown.
    ///
    /// Returns a [`Cow`] so that statically-known classifications can be
    /// borrowed while dynamically-constructed `Secret` values (whose subject
    /// path is built at call time) are owned.
    #[must_use]
    pub fn classify<'a>(&'a self, path: &PointerBuf) -> Option<Cow<'a, PiiClass>> {
        // 1. Exact match.
        if let Some(cls) = self.paths.get(path) {
            return Some(Cow::Borrowed(cls));
        }

        // 2. Namespace pattern: only applies to exactly three segments.
        let segs: Vec<String> = path.tokens().map(|t| t.decoded().to_string()).collect();
        if segs.len() == 3 {
            for pattern in &self.namespace_patterns {
                if segs[0] != pattern.namespace {
                    continue;
                }
                let field = segs[2].as_str();
                if pattern.plaintext_fields.contains(field) {
                    return Some(Cow::Borrowed(&PLAINTEXT));
                }
                if pattern.secret_fields.contains(field) {
                    // Build subject pointer dynamically: /namespace/ref
                    let subject = PointerBuf::from_tokens([&segs[0], &segs[1]]);
                    return Some(Cow::Owned(PiiClass::Secret { subject }));
                }
            }
        }

        // 3. Plaintext prefix.
        if let Some(first) = segs.first()
            && self
                .plaintext_prefixes
                .iter()
                .any(|p| p.as_str() == first.as_str())
        {
            return Some(Cow::Borrowed(&PLAINTEXT));
        }

        // 4. Permissive fallback.
        if self.permissive {
            return Some(Cow::Borrowed(&PLAINTEXT));
        }

        None
    }

    /// Returns the JSON Schema value if one was provided.
    #[must_use]
    pub const fn json_schema(&self) -> Option<&Value> {
        self.json_schema.as_ref()
    }

    /// Iterates over all explicitly registered paths.
    pub fn known_paths(&self) -> impl Iterator<Item = &PointerBuf> {
        self.paths.keys()
    }
}

// ── AttributeSchemaConfig ─────────────────────────────────────────────────────

/// JSON-serialisable configuration for building an [`AttributeSchema`].
///
/// Intended for loading from a file pointed to by the
/// `JOURNEY_ATTRIBUTE_SCHEMA_PATH` environment variable.
///
/// ```json
/// {
///   "permissive": true,
///   "namespace_patterns": [
///     {
///       "namespace": "persons",
///       "secret_fields": ["firstName", "lastName", "dateOfBirth", "passportNumber", "nationality"],
///       "plaintext_fields": ["passengerType"]
///     }
///   ]
/// }
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttributeSchemaConfig {
    /// If `true`, paths not matched by exact entries, namespace patterns, or
    /// prefix rules are treated as `Plaintext` (permissive mode).
    #[serde(default)]
    pub permissive: bool,
    /// Top-level path segments whose entire subtrees are `Plaintext`.
    /// e.g. `["search", "booking"]` covers `search/origin`,
    /// `booking/pricing/totalPrice`, etc.
    #[serde(default)]
    pub plaintext_prefixes: Vec<String>,
    /// Dynamic namespace-based classification rules.
    #[serde(default)]
    pub namespace_patterns: Vec<NamespacePatternConfig>,
}

/// JSON-serialisable form of [`NamespacePattern`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NamespacePatternConfig {
    pub namespace: String,
    #[serde(default)]
    pub secret_fields: BTreeSet<String>,
    #[serde(default)]
    pub plaintext_fields: BTreeSet<String>,
}

impl From<AttributeSchemaConfig> for AttributeSchema {
    fn from(config: AttributeSchemaConfig) -> Self {
        let base = if config.permissive {
            Self::permissive()
        } else {
            Self::new(BTreeMap::new(), None)
        };
        base.with_plaintext_prefixes(config.plaintext_prefixes)
            .with_namespace_patterns(
                config
                    .namespace_patterns
                    .into_iter()
                    .map(|p| NamespacePattern {
                        namespace: p.namespace,
                        secret_fields: p.secret_fields,
                        plaintext_fields: p.plaintext_fields,
                    })
                    .collect(),
            )
    }
}

// ── Classification ────────────────────────────────────────────────────────────

/// The output of [`classify_changes`]: a flat batch of attribute changes split
/// by their privacy classification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Classification {
    /// Changes that may be stored in plaintext.
    pub plaintext: BTreeMap<PointerBuf, Value>,
    /// Changes grouped by subject UUID — each group must be encrypted under
    /// that subject's DEK.
    pub secret_by_subject: BTreeMap<Uuid, BTreeMap<PointerBuf, Value>>,
    /// Paths that are neither in the schema nor handled by permissive mode.
    /// Also includes secret paths whose subject UUID could not be resolved by
    /// the caller-supplied lookup.  The caller decides how to react (typically
    /// an error).
    pub unknown: Vec<PointerBuf>,
}

/// Classify a flat map of attribute changes against `schema`.
///
/// `subject_lookup` resolves a *subject path* (e.g. `"/persons/0"`) to the
/// `Uuid` of the underlying data-subject.  When the lookup returns `None` for
/// a secret path, that path is routed to [`Classification::unknown`].
pub fn classify_changes(
    schema: &AttributeSchema,
    changes: &BTreeMap<PointerBuf, Value>,
    subject_lookup: impl Fn(&PointerBuf) -> Option<Uuid>,
) -> Classification {
    let mut plaintext: BTreeMap<PointerBuf, Value> = BTreeMap::new();
    let mut secret_by_subject: BTreeMap<Uuid, BTreeMap<PointerBuf, Value>> = BTreeMap::new();
    let mut unknown: Vec<PointerBuf> = Vec::new();

    for (path, value) in changes {
        match schema.classify(path) {
            None => {
                unknown.push(path.clone());
            }
            Some(cls) => match cls.as_ref() {
                PiiClass::Plaintext => {
                    plaintext.insert(path.clone(), value.clone());
                }
                PiiClass::Secret { subject } => match subject_lookup(subject) {
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

    fn path(s: &str) -> PointerBuf {
        PointerBuf::parse(s).unwrap()
    }

    fn subject_a() -> Uuid {
        Uuid::parse_str("00000000-0000-0000-0000-000000000001").unwrap()
    }

    fn subject_b() -> Uuid {
        Uuid::parse_str("00000000-0000-0000-0000-000000000002").unwrap()
    }

    fn simple_schema() -> AttributeSchema {
        let mut paths = BTreeMap::new();
        paths.insert(path("/search/origin"), PiiClass::Plaintext);
        paths.insert(path("/search/destination"), PiiClass::Plaintext);
        paths.insert(
            path("/persons/0/passport"),
            PiiClass::Secret {
                subject: path("/persons/0"),
            },
        );
        paths.insert(
            path("/persons/1/passport"),
            PiiClass::Secret {
                subject: path("/persons/1"),
            },
        );
        AttributeSchema::new(paths, None)
    }

    fn lookup_both(subject_path: &PointerBuf) -> Option<Uuid> {
        match subject_path.to_string().as_str() {
            "/persons/0" => Some(subject_a()),
            "/persons/1" => Some(subject_b()),
            _ => None,
        }
    }

    // ── all plaintext ─────────────────────────────────────────────────────

    #[test]
    fn all_plaintext_changes_land_in_plaintext_bucket() {
        let schema = simple_schema();
        let mut changes = BTreeMap::new();
        changes.insert(path("/search/origin"), json!("LHR"));
        changes.insert(path("/search/destination"), json!("JFK"));

        let result = classify_changes(&schema, &changes, lookup_both);

        assert_eq!(result.plaintext.len(), 2);
        assert!(result.secret_by_subject.is_empty());
        assert!(result.unknown.is_empty());
        assert_eq!(result.plaintext[&path("/search/origin")], json!("LHR"));
    }

    // ── mixed plaintext + single secret subject ───────────────────────────

    #[test]
    fn mixed_changes_split_correctly_for_one_subject() {
        let schema = simple_schema();
        let mut changes = BTreeMap::new();
        changes.insert(path("/search/origin"), json!("LHR"));
        changes.insert(path("/persons/0/passport"), json!("AB123456"));

        let result = classify_changes(&schema, &changes, lookup_both);

        assert_eq!(result.plaintext.len(), 1);
        assert_eq!(result.secret_by_subject.len(), 1);
        assert!(result.unknown.is_empty());
        let slot = result.secret_by_subject.get(&subject_a()).unwrap();
        assert_eq!(slot[&path("/persons/0/passport")], json!("AB123456"));
    }

    // ── two subjects in one batch ─────────────────────────────────────────

    #[test]
    fn two_subjects_produce_two_keys_in_secret_map() {
        let schema = simple_schema();
        let mut changes = BTreeMap::new();
        changes.insert(path("/persons/0/passport"), json!("AB111111"));
        changes.insert(path("/persons/1/passport"), json!("CD222222"));

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
        changes.insert(path("/search/origin"), json!("LHR"));
        changes.insert(path("/mystery/field"), json!("surprise"));

        let result = classify_changes(&schema, &changes, lookup_both);

        assert_eq!(result.plaintext.len(), 1);
        assert!(result.secret_by_subject.is_empty());
        assert_eq!(result.unknown, vec![path("/mystery/field")]);
    }

    // ── secret path with no resolvable subject ────────────────────────────

    #[test]
    fn secret_with_unresolvable_subject_lands_in_unknown() {
        let schema = simple_schema();
        let mut changes = BTreeMap::new();
        changes.insert(path("/persons/0/passport"), json!("AB123456"));

        let result = classify_changes(&schema, &changes, |_| None);

        assert!(result.plaintext.is_empty());
        assert!(result.secret_by_subject.is_empty());
        assert_eq!(result.unknown, vec![path("/persons/0/passport")]);
    }

    // ── permissive schema ─────────────────────────────────────────────────

    #[test]
    fn permissive_schema_classifies_all_paths_as_plaintext() {
        let schema = AttributeSchema::permissive();
        let mut changes = BTreeMap::new();
        changes.insert(path("/anything/at/all"), json!(42));
        changes.insert(path("/another/one"), json!(true));

        let result = classify_changes(&schema, &changes, |_| None);

        assert_eq!(result.plaintext.len(), 2);
        assert!(result.secret_by_subject.is_empty());
        assert!(result.unknown.is_empty());
    }

    // ── namespace pattern ─────────────────────────────────────────────────

    fn persons_namespace_schema() -> AttributeSchema {
        let pattern = NamespacePattern {
            namespace: "persons".to_string(),
            secret_fields: ["firstName", "lastName", "passportNumber"]
                .iter()
                .map(|s| (*s).to_string())
                .collect(),
            plaintext_fields: std::iter::once("passengerType")
                .map(str::to_string)
                .collect(),
        };
        AttributeSchema::permissive().with_namespace_patterns(vec![pattern])
    }

    fn lookup_passenger(subject_path: &PointerBuf) -> Option<Uuid> {
        match subject_path.to_string().as_str() {
            "/persons/passenger_0" => Some(subject_a()),
            "/persons/passenger_1" => Some(subject_b()),
            _ => None,
        }
    }

    #[test]
    fn namespace_pattern_classifies_secret_field_as_secret() {
        let schema = persons_namespace_schema();
        let result = schema.classify(&path("/persons/passenger_0/firstName"));
        assert!(matches!(
            result.as_deref(),
            Some(PiiClass::Secret { subject }) if subject.to_string().as_str() == "/persons/passenger_0"
        ));
    }

    #[test]
    fn namespace_pattern_classifies_plaintext_field_as_plaintext() {
        let schema = persons_namespace_schema();
        let result = schema.classify(&path("/persons/passenger_0/passengerType"));
        assert!(matches!(result.as_deref(), Some(PiiClass::Plaintext)));
    }

    #[test]
    fn namespace_pattern_unknown_field_falls_through_to_permissive() {
        let schema = persons_namespace_schema();
        // "role" is in neither secret_fields nor plaintext_fields,
        // but the schema is permissive so it falls through to Plaintext.
        let result = schema.classify(&path("/persons/passenger_0/role"));
        assert!(matches!(result.as_deref(), Some(PiiClass::Plaintext)));
    }

    #[test]
    fn namespace_pattern_routes_secret_to_correct_subject() {
        let schema = persons_namespace_schema();
        let mut changes = BTreeMap::new();
        changes.insert(path("/persons/passenger_0/firstName"), json!("Alice"));
        changes.insert(path("/persons/passenger_0/passengerType"), json!("adult"));

        let result = classify_changes(&schema, &changes, lookup_passenger);

        assert_eq!(result.plaintext.len(), 1);
        assert_eq!(
            result.plaintext[&path("/persons/passenger_0/passengerType")],
            json!("adult")
        );
        assert_eq!(result.secret_by_subject.len(), 1);
        let slot = result.secret_by_subject.get(&subject_a()).unwrap();
        assert_eq!(
            slot[&path("/persons/passenger_0/firstName")],
            json!("Alice")
        );
        assert!(result.unknown.is_empty());
    }

    #[test]
    fn namespace_pattern_two_subjects_split_correctly() {
        let schema = persons_namespace_schema();
        let mut changes = BTreeMap::new();
        changes.insert(
            path("/persons/passenger_0/passportNumber"),
            json!("AB111111"),
        );
        changes.insert(
            path("/persons/passenger_1/passportNumber"),
            json!("CD222222"),
        );

        let result = classify_changes(&schema, &changes, lookup_passenger);

        assert!(result.plaintext.is_empty());
        assert_eq!(result.secret_by_subject.len(), 2);
        assert!(result.unknown.is_empty());
    }

    #[test]
    fn attribute_schema_config_round_trips_via_json() {
        let config = AttributeSchemaConfig {
            permissive: true,
            plaintext_prefixes: vec!["search".to_string()],
            namespace_patterns: vec![NamespacePatternConfig {
                namespace: "persons".to_string(),
                secret_fields: ["firstName", "lastName"]
                    .iter()
                    .map(|s| (*s).to_string())
                    .collect(),
                plaintext_fields: std::iter::once("passengerType")
                    .map(str::to_string)
                    .collect(),
            }],
        };
        let json = serde_json::to_string(&config).unwrap();
        let decoded: AttributeSchemaConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.permissive, config.permissive);
        assert_eq!(decoded.namespace_patterns.len(), 1);
        assert_eq!(decoded.namespace_patterns[0].namespace, "persons");
    }
}
