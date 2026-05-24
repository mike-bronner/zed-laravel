//! Owned, Salsa-free pattern extraction. Mirrors the work
//! [`crate::salsa_impl::parse_file_patterns`] + [`crate::salsa_impl::SalsaActor::handle_get_patterns`]
//! do, but produces a plain [`ParsedPatternsData`] without going through
//! the single-threaded Salsa actor.
//!
//! Existing for ONE reason: parallel cache warming. Doing the parse +
//! extract inside the actor forces every project file through a sequential
//! queue while the user's interactive requests pile up behind it. The
//! warming task drives this module across many CPU cores via tokio's
//! blocking thread pool, then bulk-imports the results in a single actor
//! request.
//!
//! Lives separately from `salsa_impl` because the actor's version weaves
//! Salsa interning (`ViewName::new(db, …)`) through the conversion — that
//! interning is a perf optimization for incremental recomputation and
//! isn't useful for a cold bulk parse. We accept some duplication of the
//! extraction-to-owned-data translation for the win.

use std::path::Path;
use std::sync::Arc;

use crate::blade_embedded_php::{adjust_inner_position, extract_php_regions};
use crate::parser::{language_blade, language_php, parse_blade, parse_php};
use crate::queries::{
    extract_all_blade_patterns, extract_all_php_patterns, AssetHelperType as QAssetHelperType,
    ExtractedPhpPatterns,
};
use crate::salsa_impl::{
    ActionReferenceData, AssetHelperType, AssetReferenceData, BindingReferenceData,
    ComponentReferenceData, ConfigReferenceData, DirectiveReferenceData, EnvReferenceData,
    FeatureReferenceData, LivewireReferenceData, MiddlewareReferenceData, ParsedPatternsData,
    RouteReferenceData, TranslationReferenceData, UrlReferenceData, ViewReferenceData,
};

/// Parse a file and return its `ParsedPatternsData` directly. Detects Blade
/// vs plain PHP from the path extension. Errors during parsing yield empty
/// data for the affected pass; never panics.
pub fn parse_owned(path: &Path, text: &str) -> Arc<ParsedPatternsData> {
    let is_blade = path.to_string_lossy().ends_with(".blade.php");
    let mut data = ParsedPatternsData::default();

    // Blade-specific pattern extraction (components, <livewire:…>, directives,
    // and the existing echo→translation special case).
    if is_blade {
        let lang = language_blade();
        if let Ok(tree) = parse_blade(text) {
            if let Ok(bp) = extract_all_blade_patterns(&tree, text, &lang) {
                for c in bp.components {
                    data.components.push(Arc::new(ComponentReferenceData {
                        name: c.component_name.to_string(),
                        tag_name: c.tag_name.to_string(),
                        line: c.row as u32,
                        column: c.column as u32,
                        end_column: c.end_column as u32,
                    }));
                }
                for l in bp.livewire {
                    data.livewire_refs.push(Arc::new(LivewireReferenceData {
                        name: l.component_name.to_string(),
                        line: l.row as u32,
                        column: l.column as u32,
                        end_column: l.end_column as u32,
                    }));
                }
                for d in bp.directives {
                    let args = d.arguments.map(|s| s.to_string());
                    data.directives.push(Arc::new(DirectiveReferenceData {
                        name: d.directive_name.to_string(),
                        arguments: args,
                        line: d.row as u32,
                        column: d.column as u32,
                        end_column: d.end_column as u32,
                        string_column: d.string_column as u32,
                        string_end_column: d.string_end_column as u32,
                    }));
                }
                for echo in &bp.echo_php {
                    if let Some((key, start, end)) =
                        crate::salsa_impl::extract_translation_from_echo(echo.php_content)
                    {
                        data.translation_refs
                            .push(Arc::new(TranslationReferenceData {
                                key,
                                line: echo.row as u32,
                                column: (echo.column + start) as u32,
                                end_column: (echo.column + end) as u32,
                            }));
                    }
                }
            }
        }

        // Blade-embedded PHP regions: re-parse each {{ }} / {!! !!} / @php
        // region as PHP and accumulate its patterns. Same logic as the
        // Phase 1.5 fix in parse_file_patterns/handle_get_patterns, but
        // here it runs against owned data.
        let lang_php = language_php();
        for region in extract_php_regions(text) {
            let wrapped = format!("<?php {}", region.content);
            let Ok(tree) = parse_php(&wrapped) else {
                continue;
            };
            let Ok(snippet) = extract_all_php_patterns(&tree, &wrapped, &lang_php) else {
                continue;
            };
            push_php_patterns(&snippet, &mut data, Some((region.row, region.column)));
        }
    }

    // Full-file PHP parse. For .php files this is the main pass; for Blade
    // files tree-sitter-php can't see through the Blade syntax, so this
    // usually returns nothing — but it's cheap to attempt.
    let lang_php = language_php();
    if let Ok(tree) = parse_php(text) {
        if let Ok(patterns) = extract_all_php_patterns(&tree, text, &lang_php) {
            push_php_patterns(&patterns, &mut data, None);
        }
    }

    data.build_position_index();
    Arc::new(data)
}

/// Append every PHP-side pattern from `snippet` into `data`. When `offset`
/// is `Some((base_row, base_col))`, snippet positions are mapped back via
/// [`adjust_inner_position`] (the snippet was a `<?php `-wrapped Blade region).
/// `None` means positions are already absolute (a full-file PHP parse).
fn push_php_patterns(
    snippet: &ExtractedPhpPatterns,
    data: &mut ParsedPatternsData,
    offset: Option<(u32, u32)>,
) {
    let xform = |row: usize, col: usize, end_col: usize| -> (u32, u32, u32) {
        match offset {
            Some((base_row, base_col)) => {
                let (line, c) = adjust_inner_position(row as u32, col as u32, base_row, base_col);
                let (_, ec) = adjust_inner_position(row as u32, end_col as u32, base_row, base_col);
                (line, c, ec)
            }
            None => (row as u32, col as u32, end_col as u32),
        }
    };

    for v in &snippet.views {
        let (line, col, end_col) = xform(v.row, v.column, v.end_column);
        data.views.push(Arc::new(ViewReferenceData {
            name: v.view_name.to_string(),
            line,
            column: col,
            end_column: end_col,
            is_route_view: v.is_route_view,
        }));
    }
    for e in &snippet.env_calls {
        let (line, col, end_col) = xform(e.row, e.column, e.end_column);
        data.env_refs.push(Arc::new(EnvReferenceData {
            name: e.var_name.to_string(),
            has_fallback: e.has_fallback,
            line,
            column: col,
            end_column: end_col,
        }));
    }
    for c in &snippet.config_calls {
        let (line, col, end_col) = xform(c.row, c.column, c.end_column);
        data.config_refs.push(Arc::new(ConfigReferenceData {
            key: c.config_key.to_string(),
            line,
            column: col,
            end_column: end_col,
        }));
    }
    for m in &snippet.middleware_calls {
        let (line, col, end_col) = xform(m.row, m.column, m.end_column);
        data.middleware_refs.push(Arc::new(MiddlewareReferenceData {
            name: m.middleware_name.to_string(),
            line,
            column: col,
            end_column: end_col,
        }));
    }
    for t in &snippet.translation_calls {
        let (line, col, end_col) = xform(t.row, t.column, t.end_column);
        data.translation_refs
            .push(Arc::new(TranslationReferenceData {
                key: t.translation_key.to_string(),
                line,
                column: col,
                end_column: end_col,
            }));
    }
    for a in &snippet.asset_calls {
        let (line, col, end_col) = xform(a.row, a.column, a.end_column);
        let helper_type = match a.helper_type {
            QAssetHelperType::Asset => AssetHelperType::Asset,
            QAssetHelperType::PublicPath => AssetHelperType::PublicPath,
            QAssetHelperType::BasePath => AssetHelperType::BasePath,
            QAssetHelperType::AppPath => AssetHelperType::AppPath,
            QAssetHelperType::StoragePath => AssetHelperType::StoragePath,
            QAssetHelperType::DatabasePath => AssetHelperType::DatabasePath,
            QAssetHelperType::LangPath => AssetHelperType::LangPath,
            QAssetHelperType::ConfigPath => AssetHelperType::ConfigPath,
            QAssetHelperType::ResourcePath => AssetHelperType::ResourcePath,
            QAssetHelperType::Mix => AssetHelperType::Mix,
            QAssetHelperType::ViteAsset => AssetHelperType::ViteAsset,
        };
        data.asset_refs.push(Arc::new(AssetReferenceData {
            path: a.path.to_string(),
            helper_type,
            line,
            column: col,
            end_column: end_col,
        }));
    }
    for b in &snippet.binding_calls {
        let (line, col, end_col) = xform(b.row, b.column, b.end_column);
        data.binding_refs.push(Arc::new(BindingReferenceData {
            name: b.binding_name.to_string(),
            is_class_reference: b.is_class_reference,
            line,
            column: col,
            end_column: end_col,
        }));
    }
    for r in &snippet.route_calls {
        let (line, col, end_col) = xform(r.row, r.column, r.end_column);
        data.route_refs.push(Arc::new(RouteReferenceData {
            name: r.route_name.to_string(),
            line,
            column: col,
            end_column: end_col,
        }));
    }
    for u in &snippet.url_calls {
        let (line, col, end_col) = xform(u.row, u.column, u.end_column);
        data.url_refs.push(Arc::new(UrlReferenceData {
            path: u.url_path.to_string(),
            line,
            column: col,
            end_column: end_col,
        }));
    }
    for a in &snippet.action_calls {
        let (line, col, end_col) = xform(a.row, a.column, a.end_column);
        data.action_refs.push(Arc::new(ActionReferenceData {
            action: a.action_name.to_string(),
            line,
            column: col,
            end_column: end_col,
        }));
    }
    for f in &snippet.feature_calls {
        let (line, col, end_col) = xform(f.row, f.column, f.end_column);
        data.feature_refs.push(Arc::new(FeatureReferenceData {
            feature_name: f.feature_name.to_string(),
            method_name: f.method_name.to_string(),
            is_class_reference: f.is_class_reference,
            line,
            column: col,
            end_column: end_col,
        }));
    }
}

#[cfg(test)]
mod tests;
