//! `AttributePath` — a validated, slash-delimited path key used to address
//! individual attributes within a journey's data bag.
//!
//! # Rules
//! - Non-empty.
//! - No leading or trailing `/`.
//! - No empty segments (i.e. no `//`).
//! - Every character must not be a Unicode control character (i.e.
//!   [`char::is_control`] returns `false`). All printable Unicode — including
//!   non-ASCII scripts — is accepted.
//! - No segment may have leading or trailing whitespace (e.g. `"foo/ bar"` or
//!   `"foo/ "` are rejected, but `"full name"` is fine).
//!
//!   In practice paths are identifiers like `"search/origin"` or
//!   `"persons/0/name"`, so this is very permissive.

use std::{fmt, str::FromStr};

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// A validated, slash-delimited path that uniquely identifies an attribute
/// within a journey's data bag.
///
/// ```
/// # use journey_dynamics::domain::AttributePath;
/// let p: AttributePath = "search/origin".parse().unwrap();
/// assert_eq!(p.as_str(), "search/origin");
/// let segs: Vec<&str> = p.segments().collect();
/// assert_eq!(segs, ["search", "origin"]);
/// ```
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct AttributePath(String);

impl AttributePath {
    /// Construct a new `AttributePath`, validating all invariants.
    ///
    /// # Errors
    /// Returns [`AttributePathError`] if any constraint is violated.
    pub fn new(s: impl Into<String>) -> Result<Self, AttributePathError> {
        let s: String = s.into();

        if s.is_empty() {
            return Err(AttributePathError::Empty);
        }
        if s.starts_with('/') {
            return Err(AttributePathError::LeadingSlash);
        }
        if s.ends_with('/') {
            return Err(AttributePathError::TrailingSlash);
        }
        if s.contains("//") {
            return Err(AttributePathError::EmptySegment);
        }
        // Reject Unicode control characters; all other code-points (including
        // non-ASCII printable characters) are permitted.
        if let Some(bad) = s.chars().find(|c| c.is_control()) {
            return Err(AttributePathError::NonPrintableChar(bad));
        }
        // No segment may have leading or trailing whitespace.
        if s.split('/')
            .any(|seg| seg.starts_with(char::is_whitespace) || seg.ends_with(char::is_whitespace))
        {
            return Err(AttributePathError::SegmentEdgeWhitespace);
        }

        Ok(Self(s))
    }

    /// Returns the raw string representation.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Iterates the path segments split by `/`.
    pub fn segments(&self) -> impl Iterator<Item = &str> {
        self.0.split('/')
    }

    /// Returns `true` if `self` equals `prefix` or starts with `prefix`
    /// followed by a `/`.
    ///
    /// ```
    /// # use journey_dynamics::domain::AttributePath;
    /// let parent: AttributePath = "persons/0".parse().unwrap();
    /// let child:  AttributePath = "persons/0/name".parse().unwrap();
    /// let other:  AttributePath = "persons/01/name".parse().unwrap();
    /// assert!( child.starts_with(&parent));
    /// assert!( parent.starts_with(&parent));   // equal ⇒ true
    /// assert!(!other.starts_with(&parent));    // "persons/01" does not start with "persons/0"
    /// ```
    #[must_use]
    pub fn starts_with(&self, prefix: &Self) -> bool {
        let me = self.0.as_str();
        let pre = prefix.0.as_str();
        me == pre || me.starts_with(&format!("{pre}/"))
    }
}

// ── Conversions ──────────────────────────────────────────────────────────────

impl fmt::Display for AttributePath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for AttributePath {
    type Err = AttributePathError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::new(s)
    }
}

impl From<AttributePath> for String {
    fn from(p: AttributePath) -> Self {
        p.0
    }
}

impl TryFrom<String> for AttributePath {
    type Error = AttributePathError;

    fn try_from(s: String) -> Result<Self, Self::Error> {
        Self::new(s)
    }
}

// ── Errors ───────────────────────────────────────────────────────────────────

/// Errors that can occur when constructing an [`AttributePath`].
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum AttributePathError {
    #[error("attribute path must not be empty")]
    Empty,

    #[error("attribute path must not start with '/'")]
    LeadingSlash,

    #[error("attribute path must not end with '/'")]
    TrailingSlash,

    #[error("attribute path must not contain empty segments ('//')")]
    EmptySegment,

    #[error("attribute path segment must not have leading or trailing whitespace")]
    SegmentEdgeWhitespace,

    #[error("attribute path contains non-printable character: {0:?}")]
    NonPrintableChar(char),
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── valid construction ────────────────────────────────────────────────

    #[test]
    fn single_segment_roundtrip() {
        let p: AttributePath = "origin".parse().unwrap();
        assert_eq!(p.to_string(), "origin");
    }

    #[test]
    fn multi_segment_roundtrip() {
        let p: AttributePath = "search/origin".parse().unwrap();
        assert_eq!(p.to_string(), "search/origin");
    }

    #[test]
    fn deep_path_roundtrip() {
        let raw = "persons/0/passport/number";
        let p: AttributePath = raw.parse().unwrap();
        assert_eq!(p.as_str(), raw);
        assert_eq!(p.to_string(), raw);
    }

    #[test]
    fn segments_iterator() {
        let p: AttributePath = "a/b/c".parse().unwrap();
        let segs: Vec<&str> = p.segments().collect();
        assert_eq!(segs, ["a", "b", "c"]);
    }

    #[test]
    fn single_segment_iterator() {
        let p: AttributePath = "hello".parse().unwrap();
        let segs: Vec<&str> = p.segments().collect();
        assert_eq!(segs, ["hello"]);
    }

    // ── rejection ────────────────────────────────────────────────────────

    #[test]
    fn rejects_empty_string() {
        assert_eq!(AttributePath::new(""), Err(AttributePathError::Empty));
    }

    #[test]
    fn rejects_leading_slash() {
        assert_eq!(
            AttributePath::new("/foo"),
            Err(AttributePathError::LeadingSlash)
        );
    }

    #[test]
    fn rejects_trailing_slash() {
        assert_eq!(
            AttributePath::new("foo/"),
            Err(AttributePathError::TrailingSlash)
        );
    }

    #[test]
    fn unicode_segment_roundtrip() {
        // Non-ASCII scripts should be accepted.
        let p: AttributePath = "lugares/origen".parse().unwrap();
        assert_eq!(p.as_str(), "lugares/origen");

        let p: AttributePath = "人物/0/名前".parse().unwrap();
        assert_eq!(p.as_str(), "人物/0/名前");
    }

    #[test]
    fn rejects_segment_leading_whitespace() {
        assert_eq!(
            AttributePath::new(" foo/bar"),
            Err(AttributePathError::SegmentEdgeWhitespace)
        );
        assert_eq!(
            AttributePath::new("foo/ bar"),
            Err(AttributePathError::SegmentEdgeWhitespace)
        );
    }

    #[test]
    fn rejects_segment_trailing_whitespace() {
        assert_eq!(
            AttributePath::new("foo /bar"),
            Err(AttributePathError::SegmentEdgeWhitespace)
        );
        assert_eq!(
            AttributePath::new("foo/ "),
            Err(AttributePathError::SegmentEdgeWhitespace)
        );
    }

    #[test]
    fn allows_interior_whitespace_in_segment() {
        // Space within a segment (not at the edges) is fine.
        let p: AttributePath = "full name/first".parse().unwrap();
        assert_eq!(p.as_str(), "full name/first");
    }

    #[test]
    fn rejects_control_character() {
        // Tab and newline are control characters and must be rejected.
        assert!(matches!(
            AttributePath::new("a\tb"),
            Err(AttributePathError::NonPrintableChar('\t'))
        ));
        assert!(matches!(
            AttributePath::new("a\nb"),
            Err(AttributePathError::NonPrintableChar('\n'))
        ));
    }

    #[test]
    fn rejects_double_slash() {
        assert_eq!(
            AttributePath::new("a//b"),
            Err(AttributePathError::EmptySegment)
        );
    }

    // ── starts_with ──────────────────────────────────────────────────────

    #[test]
    fn starts_with_equal() {
        let a: AttributePath = "persons/0".parse().unwrap();
        assert!(a.starts_with(&a));
    }

    #[test]
    fn starts_with_child() {
        let parent: AttributePath = "persons/0".parse().unwrap();
        let child: AttributePath = "persons/0/name".parse().unwrap();
        assert!(child.starts_with(&parent));
    }

    #[test]
    fn starts_with_false_same_prefix_different_segment() {
        // "persons/01" must NOT match prefix "persons/0"
        let parent: AttributePath = "persons/0".parse().unwrap();
        let other: AttributePath = "persons/01/name".parse().unwrap();
        assert!(!other.starts_with(&parent));
    }

    #[test]
    fn starts_with_false_unrelated() {
        let a: AttributePath = "search/origin".parse().unwrap();
        let b: AttributePath = "persons/0/name".parse().unwrap();
        assert!(!b.starts_with(&a));
    }

    // ── serde ────────────────────────────────────────────────────────────

    #[test]
    fn serde_roundtrip() {
        let p: AttributePath = "search/destination".parse().unwrap();
        let json = serde_json::to_string(&p).unwrap();
        assert_eq!(json, r#""search/destination""#);
        let back: AttributePath = serde_json::from_str(&json).unwrap();
        assert_eq!(p, back);
    }

    #[test]
    fn serde_rejects_invalid() {
        let result: Result<AttributePath, _> = serde_json::from_str(r#""/bad""#);
        assert!(result.is_err());
    }
}
