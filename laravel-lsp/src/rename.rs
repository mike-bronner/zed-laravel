//! `textDocument/rename` engine for Laravel patterns.
//!
//! Phase 2 ships rename for the purely string-keyed patterns where the
//! identity is the string itself (Laravel resolves them by string-in-typed-
//! position at runtime): route names, config keys, translation keys. Each
//! rename touches *every* parser-classified call site PLUS the corresponding
//! declaration site, so the codebase is internally consistent after the
//! operation completes.
//!
//! Patterns that can resolve to a backing PHP class (views, blade components,
//! livewire) deliberately do NOT participate — see Phase 3 of the
//! implementation plan. Shipping rename for the file-only case while the
//! class-backed case quietly skips the class would create an asymmetric UX.
//!
//! The "instance chain" guarantee enforced by [`crate::references`]
//! flows through here: every `TextEdit` emitted by this module targets a
//! position the tree-sitter parser classified as the matching pattern kind
//! with the matching name. Random PHP strings sharing the shape are never
//! touched.

use std::collections::HashMap;
use std::path::PathBuf;
use tower_lsp::lsp_types::{
    OneOf, OptionalVersionedTextDocumentIdentifier, Position, Range, TextDocumentEdit, TextEdit,
    Url, WorkspaceEdit,
};

use crate::references::SymbolRef;

/// One physical source position to be rewritten as part of a rename, plus
/// the exact text to write. Aggregated from call-site references (via Salsa)
/// and declaration-site walks (via the per-kind locators).
///
/// `new_text` differs by site for some kinds. Renaming `app.name` →
/// `app.label`, for instance, writes the *leaf segment* `label` at the
/// declaration position in `config/app.php` but writes the *full dotted
/// form* `app.label` at every `config('app.name')` call site.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditTarget {
    pub file_path: PathBuf,
    pub line: u32,
    pub start_column: u32,
    pub end_column: u32,
    pub new_text: String,
}

/// Decide whether a classified symbol participates in rename right now.
///
/// Expanded incrementally as each kind's declaration-site finder lands.
/// A rename is enabled only after BOTH the call-site collector (already in
/// place via `crate::references`) AND the declaration-site collector for the
/// matching kind are wired up — never call-sites-only, since that leaves
/// the declaration out of sync.
///
/// Enabled kinds:
/// - Route (decl via [`crate::route_name_locator`])
/// - Config (decl via [`crate::config_key_locator`])
/// - Translation (decl across locales via [`crate::translation_key_locator`])
/// - Env (decl across `.env*` files via [`crate::env_key_locator`])
pub fn can_rename(symbol: &SymbolRef) -> bool {
    matches!(
        symbol,
        SymbolRef::Route(_) | SymbolRef::Config(_) | SymbolRef::Translation(_) | SymbolRef::Env(_)
    )
}

/// Build a single `WorkspaceEdit` that rewrites every `target` to its
/// `new_text`. Returns `None` if there is nothing to rewrite (defensive —
/// a rename with zero edits is a no-op and editors generally treat the
/// missing response as "cancelled").
///
/// Targets are grouped by file URI; clients that don't support the modern
/// `documentChanges` form still receive a usable `changes` map.
pub fn build_rename_edit(targets: &[EditTarget]) -> Option<WorkspaceEdit> {
    if targets.is_empty() {
        return None;
    }

    let mut grouped: HashMap<Url, Vec<TextEdit>> = HashMap::new();
    for t in targets {
        let Ok(uri) = Url::from_file_path(&t.file_path) else {
            continue;
        };
        let edit = TextEdit {
            range: Range {
                start: Position {
                    line: t.line,
                    character: t.start_column,
                },
                end: Position {
                    line: t.line,
                    character: t.end_column,
                },
            },
            new_text: t.new_text.clone(),
        };
        grouped.entry(uri).or_default().push(edit);
    }

    if grouped.is_empty() {
        return None;
    }

    // Modern `documentChanges` form (preferred by Zed and recent editors).
    // Versions are `None` because rename is best-effort across both saved and
    // unsaved buffers; clients reconcile via their own undo stacks.
    let document_changes: Vec<TextDocumentEdit> = grouped
        .iter()
        .map(|(uri, edits)| TextDocumentEdit {
            text_document: OptionalVersionedTextDocumentIdentifier {
                uri: uri.clone(),
                version: None,
            },
            edits: edits.iter().cloned().map(OneOf::Left).collect(),
        })
        .collect();

    Some(WorkspaceEdit {
        changes: Some(grouped),
        document_changes: Some(tower_lsp::lsp_types::DocumentChanges::Edits(
            document_changes,
        )),
        change_annotations: None,
    })
}

#[cfg(test)]
mod tests;
