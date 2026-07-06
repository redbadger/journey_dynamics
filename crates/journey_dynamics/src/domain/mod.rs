// ──────────────────────────────────────────────────────────────────────────────
// Deprecation metadata — used verbatim in every #[deprecated(…)] in this crate.
//
//   since = "0.3.0"
//   note  = "use `SetAttributes` / `AttributesSet` (path-keyed attributes)"
// ──────────────────────────────────────────────────────────────────────────────

pub mod commands;
pub mod events;
pub mod journey;
pub use attribute_schema::{
    AttributeEntry, AttributeSchema, AttributeSchemaConfig, Classification, NamespacePattern,
    NamespacePatternConfig, PiiClass, classify_changes,
};
pub use es_capture::attribute_schema;
pub use es_capture::json_path;
pub use json_path::{assign_all, flatten};
