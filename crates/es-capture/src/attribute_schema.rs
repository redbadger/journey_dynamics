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

// ── AttributeEntry ────────────────────────────────────────────────────────────

/// Metadata associated with a single known attribute path.
///
/// Currently carries only the privacy/encryption classification, but is
/// designed to hold additional per-attribute information in the future
/// (e.g. field type, validation rules, display name).
///
/// Obtain one via [`AttributeSchema::entry`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttributeEntry {
    /// How the value at this path should be handled for privacy / encryption.
    pub pii_class: PiiClass,
}

impl AttributeEntry {
    /// Construct an entry with the given encryption classification.
    #[must_use]
    pub const fn new(pii_class: PiiClass) -> Self {
        Self { pii_class }
    }
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
    /// Path prefix — one or more segments. The segment immediately after
    /// this prefix in any matching path is the role ref.
    ///
    /// e.g. `"/pax"` matches `/pax/{ref}/…`
    ///      `"/flights/outbound/pax"` matches `/flights/outbound/pax/{ref}/…`
    pub prefix: PointerBuf,
    /// Suffixes (relative to `prefix/{ref}`) that are exempt from encryption,
    /// stored as JSON Pointers with a leading `/` (e.g. `/passengerType`,
    /// `/address/postalCode`). Everything else under the namespace is Secret by default.
    pub plaintext_suffixes: BTreeSet<PointerBuf>,
}

// ── AttributeSchema ───────────────────────────────────────────────────────────

/// Schema that maps known attribute paths to their [`AttributeEntry`].
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
    paths: BTreeMap<PointerBuf, AttributeEntry>,
    json_schema: Option<Value>,
    /// When `true`, [`classify`](Self::classify) returns `Some(Plaintext)` for
    /// any path not matched by an exact entry, namespace pattern, or prefix.
    permissive: bool,
    /// Prefix-based rules applied when the exact path lookup misses.
    namespace_patterns: Vec<NamespacePattern>,
    /// Path prefixes whose entire subtrees are unconditionally `Plaintext`.
    /// Checked after namespace patterns and before the permissive fallback.
    /// Matching is at segment boundaries, so `/booking` covers `/booking/origin`
    /// and `/booking/pricing/totalPrice` but not `/bookingExtra/…`.
    /// Multi-segment prefixes are also supported (e.g. `/flights/outbound`).
    plaintext_prefixes: Vec<PointerBuf>,
}

/// A static `Plaintext` sentinel returned by reference in permissive/plaintext mode.
static PLAINTEXT: PiiClass = PiiClass::Plaintext;

impl AttributeSchema {
    /// Construct a schema from an explicit path-to-entry map and an optional
    /// JSON Schema used for structural validation.
    #[must_use]
    pub const fn new(
        paths: BTreeMap<PointerBuf, AttributeEntry>,
        json_schema: Option<Value>,
    ) -> Self {
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

    /// Attach plaintext-prefix rules to this schema.
    ///
    /// Any path for which `prefixes` contains a prefix (matched at a segment
    /// boundary) is classified as [`PiiClass::Plaintext`]. Checked after
    /// namespace patterns and before the permissive fallback.
    /// Example: `["/search", "/booking"]` classifies `/search/origin`,
    /// `/booking/pricing/totalPrice`, etc. as plaintext.
    #[must_use]
    pub fn with_plaintext_prefixes(mut self, prefixes: Vec<PointerBuf>) -> Self {
        self.plaintext_prefixes = prefixes;
        self
    }

    /// Classify a single path.
    ///
    /// Resolution order:
    /// 1. Exact match in `paths`.
    /// 2. Namespace pattern match (matches `prefix/{ref}/suffix`).
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
        if let Some(entry) = self.paths.get(path) {
            return Some(Cow::Borrowed(&entry.pii_class));
        }

        // 2. Namespace pattern (matches `prefix/{ref}/suffix...`).
        for pattern in &self.namespace_patterns {
            let path_str = path.as_str();
            let prefix_str = pattern.prefix.as_str();

            // The path must begin with the prefix followed by a `/` so that
            // we match on a full segment boundary (not e.g. `/personsX`).
            let Some(remaining) = path_str.strip_prefix(prefix_str) else {
                continue;
            };
            let Some(remaining) = remaining.strip_prefix('/') else {
                continue;
            };

            // The next segment is the role ref.
            if let Some(first_slash) = remaining.find('/') {
                let ref_part = &remaining[..first_slash];
                let suffix = &remaining[first_slash + 1..];

                // If the suffix is empty, it's not a field, so it doesn't match this pattern.
                if suffix.is_empty() {
                    continue;
                }

                // Plaintext-exempt suffixes pass through; everything else is Secret.
                // Form the suffix as a JSON Pointer ("/field" or "/nested/field")
                // so it can be looked up in the BTreeSet<PointerBuf>.
                if let Ok(suffix_ptr) = PointerBuf::parse(&remaining[first_slash..])
                    && pattern.plaintext_suffixes.contains(&suffix_ptr)
                {
                    return Some(Cow::Borrowed(&PLAINTEXT));
                }
                // Build the subject pointer dynamically: `prefix/{ref}`.
                let subject_str = format!("{prefix_str}/{ref_part}");
                if let Ok(subject) = PointerBuf::parse(&subject_str) {
                    return Some(Cow::Owned(PiiClass::Secret { subject }));
                }
            }
        }

        // 3. Plaintext prefix.
        if self
            .plaintext_prefixes
            .iter()
            .any(|prefix| path.starts_with(prefix))
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

    /// Returns the full [`AttributeEntry`] for an explicitly registered path,
    /// or `None` if the path is not in the exact-match map.
    ///
    /// Unlike [`classify`](Self::classify), this does not consult namespace
    /// patterns, plaintext prefixes, or the permissive fallback.
    #[must_use]
    pub fn entry(&self, path: &PointerBuf) -> Option<&AttributeEntry> {
        self.paths.get(path)
    }

    /// Iterates over all explicitly registered paths and their [`AttributeEntry`].
    pub fn entries(&self) -> impl Iterator<Item = (&PointerBuf, &AttributeEntry)> {
        self.paths.iter()
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
    /// Path prefixes whose entire subtrees are `Plaintext`, as JSON Pointers.
    /// e.g. `["/search", "/booking"]` covers `/search/origin`,
    /// `/booking/pricing/totalPrice`, etc. Bare segment names without a
    /// leading `/` (e.g. `"search"`) are also accepted and normalised.
    #[serde(default, deserialize_with = "deserialize_plaintext_prefix_pointers")]
    pub plaintext_prefixes: Vec<PointerBuf>,
    /// Dynamic namespace-based classification rules.
    #[serde(default)]
    pub namespace_patterns: Vec<NamespacePatternConfig>,
}

/// JSON-serialisable form of [`NamespacePattern`].
///
/// # Backward compatibility
/// The old format used `namespace` (single segment string) and split fields
/// into `secret_fields` / `plaintext_fields`.  Existing configs are still
/// accepted via serde aliases:
/// - `namespace` is an alias for `prefix`
/// - `plaintext_fields` is an alias for `plaintext_suffixes`
/// - `secret_fields` is silently ignored on read (those fields remain secret
///   under the new default-secret rule)
///
/// The `prefix` (and its `namespace` alias) accepts both a bare namespace
/// string (e.g. `"persons"`, the historical format) and a leading-slash JSON
/// pointer (e.g. `"/persons"`).  A missing leading `/` is added on read so the
/// value is always a valid [`PointerBuf`] internally.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NamespacePatternConfig {
    /// Path prefix — one or more segments. The segment immediately after
    /// this prefix in any matching path is the role ref.
    /// Accepts `"namespace"` as an alias for backward compatibility.
    #[serde(alias = "namespace", deserialize_with = "deserialize_prefix_pointer")]
    pub prefix: PointerBuf,
    /// Suffixes (relative to `prefix/{ref}`) that are exempt from encryption,
    /// as JSON Pointers with a leading `/` (e.g. `"/passengerType"`,
    /// `"/address/postalCode"`). Bare names without a leading `/` are also
    /// accepted and normalised. Everything else under the namespace is Secret
    /// by default. Accepts `"plaintext_fields"` as an alias for backward
    /// compatibility.
    #[serde(
        default,
        alias = "plaintext_fields",
        deserialize_with = "deserialize_suffix_pointers"
    )]
    pub plaintext_suffixes: BTreeSet<PointerBuf>,
}

/// Deserialize a namespace `prefix` from either a bare namespace string
/// (e.g. `"persons"`) or a leading-slash JSON pointer (e.g. `"/persons"`),
/// normalising the result to a valid [`PointerBuf`].
fn deserialize_prefix_pointer<'de, D>(deserializer: D) -> Result<PointerBuf, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let raw = String::deserialize(deserializer)?;
    let normalized = if raw.starts_with('/') {
        raw
    } else {
        format!("/{raw}")
    };
    PointerBuf::parse(&normalized).map_err(serde::de::Error::custom)
}

/// Deserialize `plaintext_prefixes` from a list of strings, accepting both
/// bare segment names (e.g. `"search"`) and leading-slash JSON pointers
/// (e.g. `"/search"`), normalising each to a valid [`PointerBuf`].
fn deserialize_plaintext_prefix_pointers<'de, D>(
    deserializer: D,
) -> Result<Vec<PointerBuf>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Vec::<String>::deserialize(deserializer)?
        .into_iter()
        .map(|raw| {
            let normalized = if raw.starts_with('/') {
                raw
            } else {
                format!("/{raw}")
            };
            PointerBuf::parse(&normalized).map_err(serde::de::Error::custom)
        })
        .collect()
}

/// Deserialize `plaintext_suffixes` from a list of strings, accepting both
/// bare field names (e.g. `"passengerType"`, `"address/postalCode"`) and
/// leading-slash JSON pointers (e.g. `"/passengerType"`), normalising each
/// by prepending `/` when absent.
fn deserialize_suffix_pointers<'de, D>(deserializer: D) -> Result<BTreeSet<PointerBuf>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    BTreeSet::<String>::deserialize(deserializer)?
        .into_iter()
        .map(|raw| {
            let normalized = if raw.starts_with('/') {
                raw
            } else {
                format!("/{raw}")
            };
            PointerBuf::parse(&normalized).map_err(serde::de::Error::custom)
        })
        .collect()
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
                        prefix: p.prefix,
                        plaintext_suffixes: p.plaintext_suffixes,
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
    /// Changes grouped by *role path* (e.g. `"/persons/passenger_0"`).
    ///
    /// The key is the `subject` `PointerBuf` produced by `PiiClass::Secret`
    /// — one entry per distinct role path, not per subject UUID.  The tuple
    /// value carries `(subject_uuid, changes)`: the UUID is needed by the
    /// encryption layer to look up the DEK, while the role path is used as the
    /// crypto label (AAD) so that the partition identity is meaningful on the
    /// read path.
    ///
    /// Two entries may share the same UUID when a subject occupies multiple
    /// roles; the crypto layer must handle encrypting them under the same key
    /// with different labels.
    pub secret_by_subject: BTreeMap<PointerBuf, (Uuid, BTreeMap<PointerBuf, Value>)>,
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
    let mut secret_by_subject: BTreeMap<PointerBuf, (Uuid, BTreeMap<PointerBuf, Value>)> =
        BTreeMap::new();
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
                        // Key by role path (subject PointerBuf); carry UUID
                        // as first tuple element for the encryption layer.
                        let (_, bucket) = secret_by_subject
                            .entry(subject.clone())
                            .or_insert_with(|| (uuid, BTreeMap::new()));
                        bucket.insert(path.clone(), value.clone());
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
        paths.insert(
            path("/search/origin"),
            AttributeEntry::new(PiiClass::Plaintext),
        );
        paths.insert(
            path("/search/destination"),
            AttributeEntry::new(PiiClass::Plaintext),
        );
        paths.insert(
            path("/persons/0/passport"),
            AttributeEntry::new(PiiClass::Secret {
                subject: path("/persons/0"),
            }),
        );
        paths.insert(
            path("/persons/1/passport"),
            AttributeEntry::new(PiiClass::Secret {
                subject: path("/persons/1"),
            }),
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
        let (uuid, changes) = result.secret_by_subject.get(&path("/persons/0")).unwrap();
        assert_eq!(*uuid, subject_a());
        assert_eq!(changes[&path("/persons/0/passport")], json!("AB123456"));
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
        // Keyed by role path, not UUID.
        assert!(result.secret_by_subject.contains_key(&path("/persons/0")));
        assert!(result.secret_by_subject.contains_key(&path("/persons/1")));
        // UUIDs are carried in the tuple.
        assert_eq!(result.secret_by_subject[&path("/persons/0")].0, subject_a());
        assert_eq!(result.secret_by_subject[&path("/persons/1")].0, subject_b());
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
            prefix: "/persons".parse().unwrap(),
            plaintext_suffixes: std::iter::once(path("/passengerType")).collect(),
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
    fn namespace_pattern_unknown_field_is_secret_by_default() {
        let schema = persons_namespace_schema();
        // "role" is not in plaintext_suffixes, so the new "secret by default" rule applies.
        let result = schema.classify(&path("/persons/passenger_0/role"));
        assert!(matches!(
            result.as_deref(),
            Some(PiiClass::Secret { subject }) if subject.as_str() == "/persons/passenger_0"
        ));
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
        let (uuid, slot) = result
            .secret_by_subject
            .get(&path("/persons/passenger_0"))
            .unwrap();
        assert_eq!(*uuid, subject_a());
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

    // ── classification precedence ──────────────────────────────────────────

    #[test]
    fn namespace_pattern_beats_plaintext_prefix() {
        // The critical rule from the design doc: a namespace pattern (step 2)
        // is evaluated before a plaintext prefix (step 3), so a secret field
        // inside an otherwise-plaintext subtree is still encrypted.
        //
        // Schema: `booking` is a plaintext prefix, but
        //         `booking/passengers` is a namespace pattern.
        let schema = AttributeSchema::new(BTreeMap::new(), None)
            .with_plaintext_prefixes(vec![path("/booking")])
            .with_namespace_patterns(vec![NamespacePattern {
                prefix: "/booking/passengers".parse().unwrap(),
                plaintext_suffixes: BTreeSet::new(),
            }]);

        // A plain booking field → Plaintext (step 3).
        assert!(matches!(
            schema.classify(&path("/booking/origin")).as_deref(),
            Some(PiiClass::Plaintext)
        ));

        // A passenger secret field → Secret (step 2 wins over step 3).
        assert!(matches!(
            schema
                .classify(&path("/booking/passengers/alice-ref/passportNumber"))
                .as_deref(),
            Some(PiiClass::Secret { subject })
                if subject.as_str() == "/booking/passengers/alice-ref"
        ));
    }

    #[test]
    fn exact_entry_overrides_namespace_pattern_default() {
        // An exact path entry (step 1) must override a namespace pattern's
        // default-secret classification (step 2).
        let mut paths = BTreeMap::new();
        paths.insert(
            path("/persons/0/role"),
            AttributeEntry::new(PiiClass::Plaintext), // explicit one-off exemption
        );
        let schema =
            AttributeSchema::new(paths, None).with_namespace_patterns(vec![NamespacePattern {
                prefix: "/persons".parse().unwrap(),
                plaintext_suffixes: BTreeSet::new(),
            }]);

        // The exact entry wins — Plaintext, not Secret.
        assert!(matches!(
            schema.classify(&path("/persons/0/role")).as_deref(),
            Some(PiiClass::Plaintext)
        ));

        // Other fields still follow the namespace pattern → Secret.
        assert!(matches!(
            schema.classify(&path("/persons/0/firstName")).as_deref(),
            Some(PiiClass::Secret { subject }) if subject.as_str() == "/persons/0"
        ));
    }

    #[test]
    fn path_without_attribute_segment_falls_through() {
        // `prefix/ref` with no attribute segment must not match a namespace
        // pattern and should fall through to the next rule.
        let schema =
            AttributeSchema::permissive().with_namespace_patterns(vec![NamespacePattern {
                prefix: "/pax".parse().unwrap(),
                plaintext_suffixes: BTreeSet::new(),
            }]);

        // `pax/alice-ref` — no attribute segment — falls through to permissive.
        assert!(matches!(
            schema.classify(&path("/pax/alice-ref")).as_deref(),
            Some(PiiClass::Plaintext)
        ));

        // `pax` alone — too short — falls through to permissive.
        assert!(matches!(
            schema.classify(&path("/pax")).as_deref(),
            Some(PiiClass::Plaintext)
        ));
    }

    #[test]
    fn multi_segment_suffix_is_classified_as_secret() {
        // A deep field like `pax/alice-ref/address/line1` has a multi-segment
        // suffix (`address/line1`); it should still be classified as Secret
        // unless that exact suffix is in plaintext_suffixes.
        let schema =
            AttributeSchema::permissive().with_namespace_patterns(vec![NamespacePattern {
                prefix: "/pax".parse().unwrap(),
                plaintext_suffixes: [path("/address/postalCode")].into(),
            }]);

        // Multi-segment suffix not in plaintext_suffixes → Secret.
        assert!(matches!(
            schema
                .classify(&path("/pax/alice-ref/address/line1"))
                .as_deref(),
            Some(PiiClass::Secret { subject }) if subject.as_str() == "/pax/alice-ref"
        ));

        // Multi-segment suffix that IS in plaintext_suffixes → Plaintext.
        assert!(matches!(
            schema
                .classify(&path("/pax/alice-ref/address/postalCode"))
                .as_deref(),
            Some(PiiClass::Plaintext)
        ));
    }

    #[test]
    fn multi_segment_prefix_pattern_matches_correctly() {
        // A namespace pattern with a multi-segment prefix such as
        // `flights/outbound/pax` should match paths like
        // `flights/outbound/pax/{ref}/{field}` but NOT shorter paths.
        let schema =
            AttributeSchema::permissive().with_namespace_patterns(vec![NamespacePattern {
                prefix: "/flights/outbound/pax".parse().unwrap(),
                plaintext_suffixes: BTreeSet::new(),
            }]);

        // Correct depth → Secret.
        assert!(matches!(
            schema
                .classify(&path("/flights/outbound/pax/alice-ref/passportNumber"))
                .as_deref(),
            Some(PiiClass::Secret { subject })
                if subject.as_str() == "/flights/outbound/pax/alice-ref"
        ));

        // A different top-level namespace → falls through to permissive.
        assert!(matches!(
            schema
                .classify(&path("/flights/inbound/pax/alice-ref/passportNumber"))
                .as_deref(),
            Some(PiiClass::Plaintext)
        ));

        // Path stops at the prefix itself — too short → falls through.
        assert!(matches!(
            schema.classify(&path("/flights/outbound/pax")).as_deref(),
            Some(PiiClass::Plaintext)
        ));
    }

    #[test]
    fn attribute_schema_config_round_trips_via_json() {
        let config = AttributeSchemaConfig {
            permissive: true,
            plaintext_prefixes: vec![path("/search")],
            namespace_patterns: vec![NamespacePatternConfig {
                prefix: "/persons".parse().unwrap(),
                plaintext_suffixes: std::iter::once(path("/passengerType")).collect(),
            }],
        };
        let json = serde_json::to_string(&config).unwrap();
        let decoded: AttributeSchemaConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.permissive, config.permissive);
        assert_eq!(decoded.namespace_patterns.len(), 1);
        assert_eq!(
            decoded.namespace_patterns[0].prefix,
            config.namespace_patterns[0].prefix
        );
    }

    #[test]
    fn old_namespace_pattern_config_format_deserialises() {
        // Configs written before the NamespacePattern refactor used `namespace`,
        // `secret_fields`, and `plaintext_fields`.  They must still parse.
        let old_json = r#"{
            "permissive": true,
            "namespace_patterns": [{
                "namespace": "persons",
                "secret_fields": ["firstName", "lastName"],
                "plaintext_fields": ["passengerType"]
            }]
        }"#;
        let config: AttributeSchemaConfig = serde_json::from_str(old_json).unwrap();
        assert_eq!(config.namespace_patterns.len(), 1);
        // `namespace` aliased to `prefix`, normalised to a leading-slash pointer
        assert_eq!(config.namespace_patterns[0].prefix.as_str(), "/persons");
        // `plaintext_fields` aliased to `plaintext_suffixes`
        assert!(
            config.namespace_patterns[0]
                .plaintext_suffixes
                .contains(&path("/passengerType"))
        );
        // `secret_fields` silently ignored — not stored anywhere
    }
}
