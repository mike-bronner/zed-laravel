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
use std::path::{Path, PathBuf};
use tower_lsp::jsonrpc;
use tower_lsp::lsp_types::{
    AnnotatedTextEdit, ChangeAnnotation, DocumentChangeOperation, DocumentChanges, OneOf,
    OptionalVersionedTextDocumentIdentifier, Position, Range, RenameFile, RenameFileOptions,
    ResourceOp, TextDocumentEdit, TextEdit, Url, WorkspaceEdit,
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
/// - View (file move via [`crate::view_declaration_locator`])
/// - Component (Blade `<x-...>` — file move + optional class file +
///   class declaration + namespace declaration via
///   [`crate::component_declaration_locator`])
/// - Livewire (`<livewire:...>` / `@livewire(...)` — kind-dispatched
///   file moves over V4 SFC / V4 MFC / V3 Class / Volt via
///   [`crate::livewire_declaration_locator`])
/// - Middleware (alias-string rewrite at registration + call sites via
///   [`crate::middleware_binding_locator`])
/// - Binding (container binding name rewrite at registration + call
///   sites via [`crate::middleware_binding_locator`])
pub fn can_rename(symbol: &SymbolRef) -> bool {
    matches!(
        symbol,
        SymbolRef::Route(_)
            | SymbolRef::Config(_)
            | SymbolRef::Translation(_)
            | SymbolRef::Env(_)
            | SymbolRef::View(_)
            | SymbolRef::Component(_)
            | SymbolRef::Livewire(_)
            | SymbolRef::Middleware(_)
            | SymbolRef::Binding(_)
    )
}

/// Build a generic rename-related LSP error with the message a Zed toast
/// will display. Wraps the verbose `jsonrpc::Error` boilerplate for the
/// rename handler's many short-circuit cases (invalid new name, vendor
/// file, missing target, etc.). Use [`unsupported_rename_error`] instead
/// when the issue is specifically "this symbol kind isn't implemented yet".
pub fn rename_error(message: impl Into<std::borrow::Cow<'static, str>>) -> jsonrpc::Error {
    jsonrpc::Error {
        code: jsonrpc::ErrorCode::ServerError(1),
        message: message.into(),
        data: None,
    }
}

/// Build the LSP error returned to the client when a rename was attempted
/// on a symbol kind the server understands but hasn't implemented yet.
///
/// Returning `Ok(None)` from `prepare_rename` for an unsupported-kind case
/// makes Zed silently drop the request — the user presses F2 and nothing
/// happens, no feedback. Returning a `jsonrpc::Error` instead surfaces in
/// Zed as a toast/status notification so the user knows the server received
/// the request and declined it on purpose.
///
/// The message intentionally does NOT prefix itself with `laravel-lsp:` —
/// Zed already attributes the error to this server in its own framing
/// ("Error: Prepare rename via laravel-lsp failed: <our message>").
///
/// Reserved for the "we know what this is, we just can't rename it yet"
/// case. Cursor positions that don't classify as any Laravel pattern at
/// all still return `Ok(None)` — silent is correct UX for F2 on whitespace.
pub fn unsupported_rename_error(_symbol: &SymbolRef) -> jsonrpc::Error {
    // Phase 3e wired up Middleware + Binding, so every SymbolRef variant
    // we classify is renameable today. This branch survives as a
    // defensive fallback for the (impossible-by-`can_rename`-gating)
    // case where a future symbol kind is added without updating the
    // gate. Generic message so we don't claim a specific kind is
    // unsupported when in fact it's just unknown.
    jsonrpc::Error {
        code: jsonrpc::ErrorCode::ServerError(1),
        message: format!(
            "renaming this symbol is not yet implemented. If you'd like \
             to see this, please open a feature request at {}.",
            FEATURE_REQUEST_URL
        )
        .into(),
        data: None,
    }
}

/// GitHub issues URL the unsupported-rename toast points users to. Lives
/// here so adding similar "not implemented" toasts elsewhere only needs to
/// import the constant — no hand-copied URL drift.
pub const FEATURE_REQUEST_URL: &str = "https://github.com/mike-bronner/zed-laravel/issues";

/// A file move emitted alongside text edits.
///
/// Phase 3 uses this for class-backed kinds where renaming the symbol also
/// moves the backing file(s) — view `.blade.php`, anonymous and class-based
/// Blade components, Livewire view + class, the children of a Livewire MFC
/// directory. Phase 2's string-keyed kinds (route/config/translation/env)
/// never produce file renames.
///
/// Emitted with `overwrite: false, ignore_if_exists: false` so the client
/// errors loudly if the target already exists rather than silently clobbering.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileRename {
    pub old_path: PathBuf,
    pub new_path: PathBuf,
}

/// Build a `WorkspaceEdit` containing only text rewrites — Phase 2's shape.
///
/// Returns `None` if `targets` is empty (a rename with zero edits is a no-op
/// and editors generally treat the missing response as "cancelled"). Targets
/// are grouped by file URI; clients that don't support the modern
/// `documentChanges` form still receive a usable `changes` map.
///
/// Phase 3 work routes through [`build_rename_workspace_edit`] instead so it
/// can also carry [`FileRename`] operations.
pub fn build_rename_edit(targets: &[EditTarget]) -> Option<WorkspaceEdit> {
    build_rename_workspace_edit(targets, &[])
}

/// Build a `WorkspaceEdit` carrying both text rewrites and file moves.
///
/// Returns `None` only when BOTH inputs are empty. With a non-empty mix the
/// returned edit always populates `document_changes` with the modern
/// `Operations` variant (text edits + `RenameFile` ops interleaved). The
/// legacy `changes` map is populated with the text-edit portion when present
/// so clients without `documentChanges` support still apply the text portion
/// — the file portion silently no-ops on those clients, which is the
/// expected degradation for an LSP feature gated on a capability they don't
/// advertise.
///
/// `RenameFile` ops are emitted with `overwrite: false, ignore_if_exists:
/// false`. If a Phase 3 rename would clobber an existing file the client
/// surfaces the error rather than the server silently overwriting.
pub fn build_rename_workspace_edit(
    text_targets: &[EditTarget],
    file_renames: &[FileRename],
) -> Option<WorkspaceEdit> {
    if text_targets.is_empty() && file_renames.is_empty() {
        return None;
    }

    let mut grouped: HashMap<Url, Vec<TextEdit>> = HashMap::new();
    for t in text_targets {
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

    // File renames trigger the "needs confirmation" annotation — the LSP
    // spec mechanism that asks the client to surface a preview before
    // applying the edit. VSCode honors it and shows a refactor preview;
    // Zed currently does NOT honor it for WorkspaceEdits containing
    // resource operations like `RenameFile` (it applies silently, even
    // though it does open a multi-buffer for text-only renames). Filed
    // upstream for Zed to consider; the annotation stays here either way
    // because (a) it's the spec-compliant signal, (b) other clients
    // already use it, and (c) Zed may add support later — at which point
    // this Just Works with no code change.
    let needs_preview = !file_renames.is_empty();
    let annotation_id = if needs_preview {
        Some(RENAME_ANNOTATION_ID.to_string())
    } else {
        None
    };

    let rename_ops: Vec<DocumentChangeOperation> = file_renames
        .iter()
        .filter_map(|r| file_rename_to_op(r, annotation_id.clone()))
        .collect();

    if grouped.is_empty() && rename_ops.is_empty() {
        // Every input path failed `Url::from_file_path` — nothing to emit.
        return None;
    }

    let text_doc_edits: Vec<TextDocumentEdit> = grouped
        .iter()
        .map(|(uri, edits)| TextDocumentEdit {
            text_document: OptionalVersionedTextDocumentIdentifier {
                uri: uri.clone(),
                version: None,
            },
            edits: edits
                .iter()
                .cloned()
                .map(|edit| match &annotation_id {
                    Some(id) => OneOf::Right(AnnotatedTextEdit {
                        text_edit: edit,
                        annotation_id: id.clone(),
                    }),
                    None => OneOf::Left(edit),
                })
                .collect(),
        })
        .collect();

    // Wire-shape preservation: when only text edits are present (the Phase 2
    // shape), emit the simpler `Edits` variant exactly as before. When a file
    // rename is in the mix, switch to `Operations` so both can be carried in
    // one workspace edit. Operations apply in array order on the client —
    // text edits land first (rewriting source while the file is still at its
    // old path), then the file move relocates it.
    let document_changes = if rename_ops.is_empty() {
        DocumentChanges::Edits(text_doc_edits)
    } else {
        let mut ops: Vec<DocumentChangeOperation> = text_doc_edits
            .into_iter()
            .map(DocumentChangeOperation::Edit)
            .collect();
        ops.extend(rename_ops);
        DocumentChanges::Operations(ops)
    };

    let change_annotations = if needs_preview {
        let mut map = HashMap::new();
        map.insert(
            RENAME_ANNOTATION_ID.to_string(),
            ChangeAnnotation {
                label: "Rename".to_string(),
                needs_confirmation: Some(true),
                description: Some(
                    "Review the file move and updated references before applying.".to_string(),
                ),
            },
        );
        Some(map)
    } else {
        None
    };

    Some(WorkspaceEdit {
        changes: if grouped.is_empty() {
            None
        } else {
            Some(grouped)
        },
        document_changes: Some(document_changes),
        change_annotations,
    })
}

/// Annotation ID used for every edit and resource op in a Phase 3+ rename.
/// One shared ID per workspace edit — clients render it as a single
/// reviewable change rather than N separate "do you want to apply this?"
/// prompts.
const RENAME_ANNOTATION_ID: &str = "laravel-lsp:rename";

fn file_rename_to_op(
    rename: &FileRename,
    annotation_id: Option<String>,
) -> Option<DocumentChangeOperation> {
    let old_uri = path_to_uri(&rename.old_path)?;
    let new_uri = path_to_uri(&rename.new_path)?;
    Some(DocumentChangeOperation::Op(ResourceOp::Rename(
        RenameFile {
            old_uri,
            new_uri,
            options: Some(RenameFileOptions {
                overwrite: Some(false),
                ignore_if_exists: Some(false),
            }),
            annotation_id,
        },
    )))
}

fn path_to_uri(path: &Path) -> Option<Url> {
    Url::from_file_path(path).ok()
}

// ── Magic-member rename (M7) ──────────────────────────────────────────────

/// The new *declaring method* name when renaming a magic member's usage name.
///
/// Call sites use the usage name verbatim (`->active()`, `$u->posts`), but the
/// declaring method often differs and must move in lockstep:
/// - **Scope** → `scope{Pascal(new)}` (the method was `scope{Pascal(old)}`).
/// - **Accessor**, old style (`get{Pascal}Attribute`) → `get{Pascal(new)}Attribute`.
/// - **Accessor**, new style (camelCase method returning `Attribute`) → `{camel(new)}`.
/// - **Relationship / dynamic finder** → the method name *is* the usage name → `new`.
///
/// `current_method` is the actual declared method name (used to detect the
/// accessor style); `kind` selects the affix scheme.
pub fn magic_member_decl_name(
    kind: crate::salsa_impl::MagicMemberKind,
    current_method: &str,
    new_member: &str,
) -> String {
    use crate::salsa_impl::MagicMemberKind;
    let pascal = crate::naming::snake_to_pascal(new_member);
    match kind {
        MagicMemberKind::Scope => format!("scope{pascal}"),
        MagicMemberKind::Accessor => {
            if current_method.starts_with("get") && current_method.ends_with("Attribute") {
                format!("get{pascal}Attribute")
            } else {
                // New-style accessor: a camelCase method returning `Attribute`.
                let mut chars = pascal.chars();
                match chars.next() {
                    Some(first) => first.to_ascii_lowercase().to_string() + chars.as_str(),
                    None => String::new(),
                }
            }
        }
        // Relationship / dynamic finder / column / plain: method == usage name.
        _ => new_member.to_string(),
    }
}

/// Locate the name token of method `method_name` in `source`, as 0-based
/// `(line, start_column, end_column)`. Used to rewrite a magic member's
/// declaration during rename — we rewrite just the name token, not the line.
/// First match wins (PSR-4 puts one class per file).
pub fn locate_method_name(source: &str, method_name: &str) -> Option<(u32, u32, u32)> {
    let tree = crate::parser::parse_php(source).ok()?;
    let bytes = source.as_bytes();
    let mut stack = vec![tree.root_node()];
    while let Some(n) = stack.pop() {
        if n.kind() == "method_declaration" {
            if let Some(name) = n.child_by_field_name("name") {
                if name.utf8_text(bytes).ok() == Some(method_name) {
                    let s = name.start_position();
                    let e = name.end_position();
                    return Some((s.row as u32, s.column as u32, e.column as u32));
                }
            }
        }
        let mut c = n.walk();
        for ch in n.children(&mut c) {
            stack.push(ch);
        }
    }
    None
}

/// Locate the *declaring method's* name token for a magic-member usage name,
/// as 0-based `(line, start_column, end_column)`. Tries each kind-aware
/// candidate name in order (`$user->posts` → `posts()`, `full_name` →
/// `getFullNameAttribute()` then new-style `fullName()`, `active` →
/// `scopeActive()`). Goto-definition's counterpart to the declaration rewrite
/// above — same locator, driven by the usage name instead of the method name.
pub fn locate_magic_member_declaration(
    source: &str,
    kind: crate::salsa_impl::MagicMemberKind,
    member: &str,
) -> Option<(u32, u32, u32)> {
    crate::hover::candidate_method_names(kind, member)
        .into_iter()
        .find_map(|name| locate_method_name(source, &name))
}

#[cfg(test)]
mod tests;
