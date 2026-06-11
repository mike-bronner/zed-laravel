//! Test submodules for `crate::queries`. Tests live in their own files
//! to keep `queries.rs` business-logic-only, while remaining logically
//! part of the `queries` module so they can exercise private helpers
//! (e.g. `calculate_string_column_range`) via `use super::*`.

mod blade_pattern_extraction;
mod call_site_variants;
mod column_positions;
mod extraction_performance;
mod feature_pattern_extraction;
mod interpolated_strings;
mod member_access_extraction;
mod middleware_extraction;
mod php_pattern_extraction;
