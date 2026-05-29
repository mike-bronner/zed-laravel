//! Code actions (quick-fixes) for query-chain diagnostics.
//!
//! Two actions, driven entirely by the structured `data` payload on the
//! diagnostics produced by [`super::diagnostics`] — so this layer never
//! re-parses messages or re-resolves schema:
//!
//! - **Rename to `<suggestion>`** — a `TextEdit` over the diagnostic's range.
//!   For a plain column/relation/table the edit inserts the corrected
//!   identifier; for a dynamic `where{Column}` finder the range covers just
//!   the studly column portion of the method, so inserting `Email` turns
//!   `whereEmaaaail` into `whereEmail`.
//! - **Create migration to add column `<col>`** (columns only) — a `CreateFile`
//!   workspace edit writing a timestamped `database/migrations/*.php`. The body
//!   comes from the project's `migration.update.stub` (custom → vendor →
//!   built-in fallback, same as `php artisan make:migration`), so any custom
//!   format is honoured; we fill Laravel's `{{ table }}` placeholder and inject
//!   the column into the `up()`/`down()` bodies (`string()` default).
//!
//! The timestamp helper is pure (takes Unix seconds) so it can be tested
//! deterministically; the call site passes `SystemTime::now()`-derived seconds.

use std::collections::HashMap;
use std::path::Path;
use tower_lsp::lsp_types::{
    CodeAction, CodeActionKind, CodeActionOrCommand, CreateFile, CreateFileOptions, Diagnostic,
    DocumentChangeOperation, DocumentChanges, OneOf, OptionalVersionedTextDocumentIdentifier,
    Position, Range, ResourceOp, TextDocumentEdit, TextEdit, Url, WorkspaceEdit,
};

/// The fields the code-action layer reads from a chain diagnostic's `data`.
struct ChainDiagData {
    kind: String,
    name: String,
    replacement: Option<String>,
    replacement_label: Option<String>,
    table: Option<String>,
}

/// Pull the `data` payload off a chain diagnostic. Returns `None` for any
/// diagnostic that isn't one of ours (wrong source, missing/foreign data).
fn parse(diagnostic: &Diagnostic) -> Option<ChainDiagData> {
    if diagnostic.source.as_deref() != Some("laravel-lsp") {
        return None;
    }
    let data = diagnostic.data.as_ref()?;
    let str_field = |key: &str| data.get(key).and_then(|v| v.as_str()).map(str::to_string);
    Some(ChainDiagData {
        kind: str_field("kind")?,
        name: str_field("name")?,
        replacement: str_field("replacement"),
        replacement_label: str_field("replacementLabel"),
        table: str_field("table"),
    })
}

/// Build the "Rename to `<suggestion>`" quick-fix for a chain diagnostic, if it
/// carries a replacement. The edit targets the diagnostic's own range.
pub fn rename_action(diagnostic: &Diagnostic, doc_uri: &Url) -> Option<CodeActionOrCommand> {
    let data = parse(diagnostic)?;
    let replacement = data.replacement?;
    let label = data
        .replacement_label
        .unwrap_or_else(|| replacement.clone());

    let mut changes = HashMap::new();
    changes.insert(
        doc_uri.clone(),
        vec![TextEdit {
            range: diagnostic.range,
            new_text: replacement,
        }],
    );

    Some(CodeActionOrCommand::CodeAction(CodeAction {
        title: format!("Rename to `{label}`"),
        kind: Some(CodeActionKind::QUICKFIX),
        diagnostics: Some(vec![diagnostic.clone()]),
        edit: Some(WorkspaceEdit {
            changes: Some(changes),
            document_changes: None,
            change_annotations: None,
        }),
        is_preferred: Some(true),
        ..Default::default()
    }))
}

/// Build the "Create migration to add column …" quick-fix for a column
/// diagnostic. `project_root` locates `database/migrations/`; `timestamp` is
/// the `YYYY_MM_DD_HHMMSS` filename prefix (see [`format_migration_timestamp`]).
/// Returns `None` for non-column diagnostics or when the table is unknown.
pub fn migration_action(
    diagnostic: &Diagnostic,
    project_root: &Path,
    timestamp: &str,
) -> Option<CodeActionOrCommand> {
    let data = parse(diagnostic)?;
    if data.kind != "column" {
        return None;
    }
    let table = data.table?;
    let column = data.name;

    let filename = format!("{timestamp}_add_{column}_to_{table}_table.php");
    let path = project_root
        .join("database")
        .join("migrations")
        .join(&filename);
    let uri = Url::from_file_path(&path).ok()?;
    let content = migration_content(project_root, &table, &column);

    let edit = WorkspaceEdit {
        changes: None,
        document_changes: Some(DocumentChanges::Operations(vec![
            DocumentChangeOperation::Op(ResourceOp::Create(CreateFile {
                uri: uri.clone(),
                options: Some(CreateFileOptions {
                    overwrite: Some(false),
                    ignore_if_exists: Some(true),
                }),
                annotation_id: None,
            })),
            DocumentChangeOperation::Edit(TextDocumentEdit {
                text_document: OptionalVersionedTextDocumentIdentifier { uri, version: None },
                edits: vec![OneOf::Left(TextEdit {
                    range: Range {
                        start: Position {
                            line: 0,
                            character: 0,
                        },
                        end: Position {
                            line: 0,
                            character: 0,
                        },
                    },
                    new_text: content,
                })],
            }),
        ])),
        change_annotations: None,
    };

    Some(CodeActionOrCommand::CodeAction(CodeAction {
        title: format!("Create migration: add column `{column}` to `{table}`"),
        kind: Some(CodeActionKind::QUICKFIX),
        diagnostics: Some(vec![diagnostic.clone()]),
        edit: Some(edit),
        // Not preferred — the rename is the likely intent for a typo.
        is_preferred: Some(false),
        ..Default::default()
    }))
}

/// Laravel's `migration.update.stub` (10.x/11.x). Used as the fallback when the
/// project hasn't published a stub and vendor can't be located, so generated
/// migrations match `php artisan make:migration --table=` byte-for-byte.
const FALLBACK_UPDATE_STUB: &str = r#"<?php

use Illuminate\Database\Migrations\Migration;
use Illuminate\Database\Schema\Blueprint;
use Illuminate\Support\Facades\Schema;

return new class extends Migration
{
    /**
     * Run the migrations.
     */
    public function up(): void
    {
        Schema::table('{{ table }}', function (Blueprint $table) {
            //
        });
    }

    /**
     * Reverse the migrations.
     */
    public function down(): void
    {
        Schema::table('{{ table }}', function (Blueprint $table) {
            //
        });
    }
};
"#;

/// Resolve the `migration.update.stub`, honouring project customisation the
/// same way `php artisan make:migration` does (and the same priority the rest
/// of this extension's stub-backed code actions use):
///
/// 1. `stubs/migration.update.stub` — published & customised by the project.
/// 2. `vendor/laravel/framework/.../migrations/stubs/migration.update.stub`.
///
/// Returns `None` when neither exists; the caller falls back to
/// [`FALLBACK_UPDATE_STUB`] so output stays consistent regardless.
fn resolve_migration_stub(project_root: &Path) -> Option<String> {
    let custom = project_root.join("stubs").join("migration.update.stub");
    if let Ok(content) = std::fs::read_to_string(&custom) {
        return Some(content);
    }
    let framework = project_root.join(
        "vendor/laravel/framework/src/Illuminate/Database/Migrations/stubs/migration.update.stub",
    );
    std::fs::read_to_string(&framework).ok()
}

/// Build the "add column" migration from the resolved stub: honour the
/// project's format, fill Laravel's placeholders, and inject the column into
/// the `up()` / `down()` bodies. Column type defaults to `string()`.
fn migration_content(project_root: &Path, table: &str, column: &str) -> String {
    let stub =
        resolve_migration_stub(project_root).unwrap_or_else(|| FALLBACK_UPDATE_STUB.to_string());
    render_migration(&stub, table, column)
}

/// Apply Laravel's stub substitutions to `stub` and inject the column. Pure —
/// the stub text is supplied by the caller, so this is fully testable against
/// both the default stub and arbitrary custom formats.
fn render_migration(stub: &str, table: &str, column: &str) -> String {
    // Old (≤7.x) named-class stubs carry a class placeholder; modern
    // anonymous-class stubs don't. Fill it when present.
    let class_name = format!(
        "Add{}To{}Table",
        crate::laravel_introspector::snake_to_studly(column),
        crate::laravel_introspector::snake_to_studly(table),
    );
    let substituted = stub
        .replace("{{ table }}", table)
        .replace("{{table}}", table)
        .replace("DummyTable", table)
        .replace("{{ class }}", &class_name)
        .replace("{{class}}", &class_name)
        .replace("DummyClass", &class_name);
    inject_columns(&substituted, column)
}

/// Replace the first two standalone `//` body placeholders (Laravel's empty
/// closure markers — `up()` then `down()`) with the column add / drop,
/// preserving each line's indentation. Only whole-line `//` markers are
/// touched, so inline `// comments` in a custom stub are left alone. If the
/// stub has fewer than two such markers, whatever's present is filled and the
/// rest left as-is (the stub is still honoured).
fn inject_columns(content: &str, column: &str) -> String {
    let inserts = [
        format!("$table->string('{column}');"),
        format!("$table->dropColumn('{column}');"),
    ];
    let mut next = 0;
    let trailing_newline = content.ends_with('\n');
    let body: Vec<String> = content
        .lines()
        .map(|line| {
            if next < inserts.len() && line.trim() == "//" {
                let indent_len = line.len() - line.trim_start().len();
                let rendered = format!("{}{}", &line[..indent_len], inserts[next]);
                next += 1;
                rendered
            } else {
                line.to_string()
            }
        })
        .collect();
    let mut out = body.join("\n");
    if trailing_newline {
        out.push('\n');
    }
    out
}

/// Format Unix epoch seconds as Laravel's migration filename prefix
/// `YYYY_MM_DD_HHMMSS` (UTC). Pure — the caller supplies the seconds.
pub fn format_migration_timestamp(unix_secs: u64) -> String {
    let days = (unix_secs / 86_400) as i64;
    let tod = unix_secs % 86_400;
    let (hh, mm, ss) = (tod / 3600, (tod % 3600) / 60, tod % 60);
    let (y, m, d) = civil_from_days(days);
    format!("{y:04}_{m:02}_{d:02}_{hh:02}{mm:02}{ss:02}")
}

/// Convert days-since-Unix-epoch to a `(year, month, day)` civil date.
/// Howard Hinnant's `civil_from_days` algorithm — proleptic Gregorian, exact,
/// no leap-second handling needed (migration ordering only needs monotonicity).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    (y + if m <= 2 { 1 } else { 0 }, m, d)
}

#[cfg(test)]
mod tests;
