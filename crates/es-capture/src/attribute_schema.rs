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

    /// Replace the exact path-to-entry map.
    ///
    /// Exact entries are consulted first by [`classify`](Self::classify), so
    /// they take precedence over namespace patterns, plaintext prefixes, and
    /// the permissive fallback.
    #[must_use]
    pub fn with_exact_paths(mut self, paths: BTreeMap<PointerBuf, AttributeEntry>) -> Self {
        self.paths = paths;
        self
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
    /// Exact paths classified as `Secret`, each under an explicitly named
    /// subject. Used by fixed-subject aggregates (e.g. `/self/firstName`
    /// secret under subject `/self`) that the dynamic `namespace_patterns`
    /// model cannot express. Applied as exact-match entries, so they take
    /// precedence over prefixes and patterns.
    #[serde(default)]
    pub secret_paths: Vec<SecretPathConfig>,
    /// Exact paths classified as `Plaintext`. Lets a strict (non-permissive)
    /// schema enumerate every known plaintext leaf so unlisted paths are
    /// rejected rather than silently stored.
    #[serde(default, deserialize_with = "deserialize_plaintext_prefix_pointers")]
    pub plaintext_paths: Vec<PointerBuf>,
}

/// JSON-serialisable form of an exact-path `Secret` classification: the
/// attribute `path` and the `subject` (role path) whose DEK encrypts it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecretPathConfig {
    /// The exact attribute path, as a JSON Pointer (e.g. `/self/firstName`).
    #[serde(deserialize_with = "deserialize_prefix_pointer")]
    pub path: PointerBuf,
    /// The subject (role path) whose DEK encrypts this value
    /// (e.g. `/self`). Accepts a bare segment or a leading-slash pointer.
    #[serde(deserialize_with = "deserialize_prefix_pointer")]
    pub subject: PointerBuf,
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
        // Exact-match entries: explicit secret paths (each under its named
        // subject) plus explicit plaintext paths.
        let paths: BTreeMap<PointerBuf, AttributeEntry> = config
            .secret_paths
            .into_iter()
            .map(|s| {
                (
                    s.path,
                    AttributeEntry::new(PiiClass::Secret { subject: s.subject }),
                )
            })
            .chain(
                config
                    .plaintext_paths
                    .into_iter()
                    .map(|p| (p, AttributeEntry::new(PiiClass::Plaintext))),
            )
            .collect();

        let base = if config.permissive {
            Self::permissive().with_exact_paths(paths)
        } else {
            Self::new(paths, None)
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

// ── Derivation from an annotated JSON Schema ──────────────────────────────────

/// Escape a single JSON-pointer reference token per RFC 6901.
fn escape_token(token: &str) -> String {
    token.replace('~', "~0").replace('/', "~1")
}

fn parse_pointer(s: &str) -> PointerBuf {
    PointerBuf::parse(s).expect("constructed JSON pointer is valid")
}

/// Resolve a schema node to the schema that determines its shape: follow a
/// `$ref` into `$defs`, and unwrap an `anyOf` Option wrapper (the non-`null`
/// branch). Returns the node unchanged when neither applies.
fn resolve<'a>(node: &'a Value, defs: Option<&'a serde_json::Map<String, Value>>) -> &'a Value {
    if let Some(reference) = node.get("$ref").and_then(Value::as_str) {
        let name = reference.strip_prefix("#/$defs/").unwrap_or(reference);
        if let Some(target) = defs.and_then(|d| d.get(name)) {
            return resolve(target, defs);
        }
    }
    if let Some(branches) = node.get("anyOf").and_then(Value::as_array) {
        if let Some(non_null) = branches
            .iter()
            .find(|b| b.get("type").and_then(Value::as_str) != Some("null"))
        {
            return resolve(non_null, defs);
        }
    }
    node
}

/// Whether `node` or any of its descendants (through `$ref`s, object
/// `properties`, map `additionalProperties`, and `anyOf` branches) carries an
/// `x-subject` marker. `seen` guards against cyclic `$ref`s.
fn subtree_has_x_subject(
    node: &Value,
    defs: Option<&serde_json::Map<String, Value>>,
    seen: &mut BTreeSet<String>,
) -> bool {
    if node.get("x-subject").is_some() {
        return true;
    }
    if let Some(reference) = node.get("$ref").and_then(Value::as_str) {
        if !seen.insert(reference.to_string()) {
            return false;
        }
    }
    let resolved = resolve(node, defs);
    if resolved.get("x-subject").is_some() {
        return true;
    }
    if let Some(props) = resolved.get("properties").and_then(Value::as_object) {
        if props.values().any(|c| subtree_has_x_subject(c, defs, seen)) {
            return true;
        }
    }
    if let Some(extra) = resolved.get("additionalProperties") {
        if extra.is_object() && subtree_has_x_subject(extra, defs, seen) {
            return true;
        }
    }
    false
}

/// If `node` resolves to a map (`additionalProperties` is itself an object
/// schema), return that entry schema resolved; otherwise `None`.
fn map_entry_schema<'a>(
    node: &'a Value,
    defs: Option<&'a serde_json::Map<String, Value>>,
) -> Option<&'a Value> {
    let resolved = resolve(node, defs);
    if resolved.get("properties").is_some() {
        return None;
    }
    let extra = resolved.get("additionalProperties")?;
    if extra.get("$ref").is_some() || extra.get("properties").is_some() {
        Some(resolve(extra, defs))
    } else {
        None
    }
}

/// Walk a fixed (non-map) object subtree, recording each leaf as an exact
/// secret path (when it carries `x-subject`, under that named subject) or an
/// exact plaintext path.
fn walk_fixed(
    node: &Value,
    pointer: &str,
    defs: Option<&serde_json::Map<String, Value>>,
    secret_paths: &mut Vec<SecretPathConfig>,
    plaintext_paths: &mut Vec<PointerBuf>,
) {
    if let Some(subject) = node.get("x-subject").and_then(Value::as_str) {
        secret_paths.push(SecretPathConfig {
            path: parse_pointer(pointer),
            subject: parse_pointer(subject),
        });
        return;
    }
    let resolved = resolve(node, defs);
    if let Some(props) = resolved.get("properties").and_then(Value::as_object) {
        for (field, child) in props {
            let child_pointer = format!("{pointer}/{}", escape_token(field));
            walk_fixed(child, &child_pointer, defs, secret_paths, plaintext_paths);
        }
    } else {
        plaintext_paths.push(parse_pointer(pointer));
    }
}

impl AttributeSchemaConfig {
    /// Derive a config from an annotated JSON Schema (e.g. schemars output),
    /// treating the `x-subject` extension keyword as the single source of truth
    /// for PII classification: a leaf carrying `x-subject` is secret, a leaf
    /// without it is plaintext.
    ///
    /// The `x-subject` **value names the subject** the field is encrypted under:
    /// - a **fixed** subject is a concrete pointer (e.g. `"/self"`), used
    ///   verbatim;
    /// - a **dynamic** subject (a field inside a map) is the map's pointer
    ///   followed by a `*` ref placeholder (e.g. `"/persons/*"`), resolving at
    ///   runtime to the concrete entry (`/persons/passenger_0`).
    ///
    /// Each top-level property is classified as:
    /// - **all-plaintext** (no `x-subject` anywhere in its subtree) → a
    ///   [`plaintext prefix`](Self::plaintext_prefixes), covering the whole
    ///   subtree without enumerating it;
    /// - a **map** (`additionalProperties`) whose entries contain `x-subject`
    ///   fields → a [`NamespacePatternConfig`] keyed on the property
    ///   (`prefix/{ref}/field`), with the non-secret entry fields recorded as
    ///   plaintext suffixes (everything else under the entry is secret). Each
    ///   secret field's `x-subject` must be `"<prefix>/*"`, matching the map.
    /// - a **fixed object** mixing secret and plaintext leaves → exact
    ///   [`secret_paths`](Self::secret_paths) (each under the subject its
    ///   `x-subject` names) and exact [`plaintext_paths`](Self::plaintext_paths).
    ///
    /// # Panics
    /// Panics if a secret field inside a map declares an `x-subject` that is not
    /// `"<prefix>/*"` for that map — a schema-authoring error.
    #[must_use]
    pub fn from_annotated_schema(schema: &Value) -> Self {
        let defs = schema.get("$defs").and_then(Value::as_object);
        let mut config = Self {
            permissive: false,
            plaintext_prefixes: Vec::new(),
            namespace_patterns: Vec::new(),
            secret_paths: Vec::new(),
            plaintext_paths: Vec::new(),
        };

        let Some(props) = schema.get("properties").and_then(Value::as_object) else {
            return config;
        };

        for (key, node) in props {
            let pointer = format!("/{}", escape_token(key));
            if !subtree_has_x_subject(node, defs, &mut BTreeSet::new()) {
                config.plaintext_prefixes.push(parse_pointer(&pointer));
            } else if let Some(entry) = map_entry_schema(node, defs) {
                // A dynamic namespace: each map entry `<pointer>/{ref}` is a
                // subject. A secret field must declare that subject explicitly
                // as `x-subject = "<pointer>/*"` (the `*` standing in for the
                // entry's ref); fields without `x-subject` are plaintext.
                let expected_subject = format!("{pointer}/*");
                let mut plaintext_suffixes = BTreeSet::new();
                if let Some(fields) = entry.get("properties").and_then(Value::as_object) {
                    for (field, child) in fields {
                        if let Some(subject) = child.get("x-subject") {
                            assert!(
                                subject.as_str() == Some(expected_subject.as_str()),
                                "x-subject {subject} on `{pointer}/*/{field}` must be \
                                 {expected_subject:?} — the dynamic subject of the enclosing \
                                 `{pointer}` map",
                            );
                        } else {
                            plaintext_suffixes
                                .insert(parse_pointer(&format!("/{}", escape_token(field))));
                        }
                    }
                }
                config.namespace_patterns.push(NamespacePatternConfig {
                    prefix: parse_pointer(&pointer),
                    plaintext_suffixes,
                });
            } else {
                walk_fixed(
                    node,
                    &pointer,
                    defs,
                    &mut config.secret_paths,
                    &mut config.plaintext_paths,
                );
            }
        }

        config.plaintext_prefixes.sort();
        config.secret_paths.sort_by(|a, b| a.path.cmp(&b.path));
        config.plaintext_paths.sort();
        config
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
            secret_paths: vec![SecretPathConfig {
                path: path("/self/firstName"),
                subject: path("/self"),
            }],
            plaintext_paths: vec![path("/self/country")],
        };
        let json = serde_json::to_string(&config).unwrap();
        let decoded: AttributeSchemaConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.permissive, config.permissive);
        assert_eq!(decoded.namespace_patterns.len(), 1);
        assert_eq!(
            decoded.namespace_patterns[0].prefix,
            config.namespace_patterns[0].prefix
        );
        assert_eq!(decoded.secret_paths.len(), 1);
        assert_eq!(decoded.plaintext_paths, vec![path("/self/country")]);
    }

    #[test]
    fn exact_secret_paths_classify_under_named_subject() {
        // Fixed-subject aggregate: `/self/firstName` secret under `/self`,
        // `/self/country` plaintext, everything else unknown (strict).
        let config = AttributeSchemaConfig {
            permissive: false,
            plaintext_prefixes: vec![],
            namespace_patterns: vec![],
            secret_paths: vec![SecretPathConfig {
                path: path("/self/firstName"),
                subject: path("/self"),
            }],
            plaintext_paths: vec![path("/self/country")],
        };
        let schema = AttributeSchema::from(config);

        assert!(matches!(
            schema.classify(&path("/self/firstName")).as_deref(),
            Some(PiiClass::Secret { subject }) if subject.as_str() == "/self"
        ));
        assert!(matches!(
            schema.classify(&path("/self/country")).as_deref(),
            Some(PiiClass::Plaintext)
        ));
        // Unlisted path is rejected (strict, non-permissive).
        assert!(schema.classify(&path("/self/unknown")).is_none());
    }

    #[test]
    fn from_annotated_schema_derives_namespace_and_plaintext_prefix() {
        // A dynamic `/persons/{ref}` namespace (map of passengers) plus a
        // non-PII `/search` subtree — mirrors the flight-booking example.
        let schema = json!({
            "type": "object",
            "properties": {
                "search": { "$ref": "#/$defs/Search" },
                "persons": {
                    "type": "object",
                    "additionalProperties": { "$ref": "#/$defs/Passenger" }
                }
            },
            "$defs": {
                "Search": {
                    "type": "object",
                    "properties": { "origin": { "type": "string" } }
                },
                "Passenger": {
                    "type": "object",
                    "properties": {
                        "firstName": { "type": ["string", "null"], "x-subject": "/persons/*" },
                        "passengerType": { "type": "string" }
                    }
                }
            }
        });
        let config = AttributeSchemaConfig::from_annotated_schema(&schema);

        assert_eq!(config.plaintext_prefixes, vec![path("/search")]);
        assert_eq!(config.namespace_patterns.len(), 1);
        assert_eq!(config.namespace_patterns[0].prefix, path("/persons"));
        assert!(
            config.namespace_patterns[0]
                .plaintext_suffixes
                .contains(&path("/passengerType"))
        );

        // The resulting schema classifies a passenger's fields correctly.
        let schema = AttributeSchema::from(config);
        assert!(matches!(
            schema.classify(&path("/persons/p0/firstName")).as_deref(),
            Some(PiiClass::Secret { subject }) if subject.as_str() == "/persons/p0"
        ));
        assert!(matches!(
            schema
                .classify(&path("/persons/p0/passengerType"))
                .as_deref(),
            Some(PiiClass::Plaintext)
        ));
        assert!(matches!(
            schema.classify(&path("/search/origin")).as_deref(),
            Some(PiiClass::Plaintext)
        ));
    }

    #[test]
    fn from_annotated_schema_derives_fixed_subject_exact_paths() {
        // A fixed-subject `/self` group mixing secret and plaintext leaves —
        // mirrors the HR Person aggregate.
        let schema = json!({
            "type": "object",
            "properties": { "self": { "$ref": "#/$defs/SelfAttrs" } },
            "$defs": {
                "SelfAttrs": {
                    "type": "object",
                    "properties": {
                        "firstName": { "type": ["string", "null"], "x-subject": "/self" },
                        "country": { "type": "string" }
                    }
                }
            }
        });
        let config = AttributeSchemaConfig::from_annotated_schema(&schema);

        assert_eq!(config.secret_paths.len(), 1);
        assert_eq!(config.secret_paths[0].path, path("/self/firstName"));
        assert_eq!(config.secret_paths[0].subject, path("/self"));
        assert_eq!(config.plaintext_paths, vec![path("/self/country")]);
        assert!(config.namespace_patterns.is_empty());
        assert!(config.plaintext_prefixes.is_empty());
    }

    #[test]
    #[should_panic(expected = "must be")]
    fn from_annotated_schema_rejects_mismatched_namespace_subject() {
        // A secret field inside the `/persons` map names a different subject —
        // a schema-authoring error the deriver must reject.
        let schema = json!({
            "type": "object",
            "properties": {
                "persons": {
                    "type": "object",
                    "additionalProperties": { "$ref": "#/$defs/Passenger" }
                }
            },
            "$defs": {
                "Passenger": {
                    "type": "object",
                    "properties": {
                        "firstName": { "type": ["string", "null"], "x-subject": "/elephants/*" }
                    }
                }
            }
        });
        let _ = AttributeSchemaConfig::from_annotated_schema(&schema);
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
