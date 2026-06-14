// ──────────────────────────────────────────────────────────────────────────────
// Deprecation metadata — used verbatim in every #[deprecated(…)] in this crate.
//
//   since = "0.3.0"
//   note  = "use `SetAttributes` / `AttributesSet` (path-keyed attributes)"
// ──────────────────────────────────────────────────────────────────────────────

pub mod attribute_schema;
pub mod commands;
pub mod events;
pub mod journey;
pub mod json_path;
pub use attribute_schema::{
    AttributeSchema, AttributeSchemaConfig, Classification, NamespacePattern,
    NamespacePatternConfig, PiiClass, classify_changes,
};
pub use json_path::{assign_all, flatten};
