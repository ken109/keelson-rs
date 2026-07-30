use std::borrow::Cow;

use super::raw::Raw;

/// `AND`, the [`Join`](super::Join) separator inside `BETWEEN`.
pub const AND: Raw = Raw(Cow::Borrowed("AND"));

/// `NOT`, prefixed by [`not`](super::not).
pub const NOT: Raw = Raw(Cow::Borrowed("NOT"));

/// `NULL`, what an empty [`Group`](super::Group) renders as.
pub const NULL: Raw = Raw(Cow::Borrowed("NULL"));

/// `IS NULL`.
pub const IS_NULL: Raw = Raw(Cow::Borrowed("IS NULL"));

/// `IS NOT NULL`.
pub const IS_NOT_NULL: Raw = Raw(Cow::Borrowed("IS NOT NULL"));

/// `BETWEEN`.
pub const BETWEEN: Raw = Raw(Cow::Borrowed("BETWEEN"));

/// `NOT BETWEEN`.
pub const NOT_BETWEEN: Raw = Raw(Cow::Borrowed("NOT BETWEEN"));

/// `IS DISTINCT FROM`.
pub const IS_DISTINCT_FROM: Raw = Raw(Cow::Borrowed("IS DISTINCT FROM"));

/// `IS NOT DISTINCT FROM`.
pub const IS_NOT_DISTINCT_FROM: Raw = Raw(Cow::Borrowed("IS NOT DISTINCT FROM"));
