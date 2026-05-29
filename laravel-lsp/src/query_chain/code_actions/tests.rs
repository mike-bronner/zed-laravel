use super::*;
use serde_json::json;
use tempfile::TempDir;
use tower_lsp::lsp_types::{CodeActionOrCommand, Diagnostic, NumberOrString, Range};

fn diag(code: &str, data: serde_json::Value) -> Diagnostic {
    Diagnostic {
        range: Range {
            start: Position {
                line: 2,
                character: 12,
            },
            end: Position {
                line: 2,
                character: 18,
            },
        },
        source: Some("laravel-lsp".to_string()),
        code: Some(NumberOrString::String(code.to_string())),
        data: Some(data),
        ..Default::default()
    }
}

fn uri() -> Url {
    Url::parse("file:///app/Http/Controllers/UserController.php").unwrap()
}

// ---- format_migration_timestamp / civil_from_days -------------------------

#[test]
fn timestamp_epoch_zero() {
    assert_eq!(format_migration_timestamp(0), "1970_01_01_000000");
}

#[test]
fn timestamp_end_of_first_day() {
    assert_eq!(format_migration_timestamp(86_399), "1970_01_01_235959");
    assert_eq!(format_migration_timestamp(86_400), "1970_01_02_000000");
}

#[test]
fn timestamp_known_modern_date() {
    // 2020-01-01 00:00:00 UTC
    assert_eq!(
        format_migration_timestamp(1_577_836_800),
        "2020_01_01_000000"
    );
}

#[test]
fn timestamp_handles_leap_day() {
    // 2020-02-29 00:00:00 UTC (2020 is a leap year)
    assert_eq!(
        format_migration_timestamp(1_582_934_400),
        "2020_02_29_000000"
    );
}

// ---- render_migration / stub handling -------------------------------------

const STD_STUB: &str = r#"<?php

use Illuminate\Database\Migrations\Migration;
use Illuminate\Database\Schema\Blueprint;
use Illuminate\Support\Facades\Schema;

return new class extends Migration
{
    public function up(): void
    {
        Schema::table('{{ table }}', function (Blueprint $table) {
            //
        });
    }

    public function down(): void
    {
        Schema::table('{{ table }}', function (Blueprint $table) {
            //
        });
    }
};
"#;

#[test]
fn render_fills_table_and_injects_column_with_indentation() {
    let c = render_migration(STD_STUB, "users", "phone");
    // `{{ table }}` substituted in both closures.
    assert!(!c.contains("{{ table }}"));
    assert_eq!(c.matches("Schema::table('users'").count(), 2);
    // Column injected at the placeholder's own indentation (12 spaces), and the
    // `//` markers are gone.
    assert!(
        c.contains("\n            $table->string('phone');\n"),
        "up column at 12 spaces; got:\n{c}"
    );
    assert!(
        c.contains("\n            $table->dropColumn('phone');\n"),
        "down column at 12 spaces"
    );
    assert!(!c.contains("//"), "placeholders consumed");
    assert!(c.contains("\n    public function up(): void\n"));
}

#[test]
fn render_honours_custom_stub_and_skips_inline_comments() {
    // A custom stub with a license header (inline `//`) and an extra comment.
    // The inline comments must survive; only the standalone `//` body markers
    // get the column injected.
    let custom = "<?php\n// Acme Corp — proprietary.\n\nreturn new class extends Migration\n{\n    public function up(): void\n    {\n        Schema::table('{{ table }}', function (Blueprint $table) {\n            //\n        });\n    }\n    public function down(): void\n    {\n        Schema::table('{{ table }}', function (Blueprint $table) {\n            //\n        });\n    }\n};\n";
    let c = render_migration(custom, "orders", "ref");
    assert!(
        c.contains("// Acme Corp — proprietary."),
        "inline comment kept"
    );
    assert!(c.contains("$table->string('ref');"));
    assert!(c.contains("$table->dropColumn('ref');"));
    assert_eq!(c.matches("Schema::table('orders'").count(), 2);
}

#[test]
fn render_fills_legacy_named_class_placeholder() {
    let legacy = "<?php\nclass {{ class }} extends Migration {\n    public function up() { Schema::table('DummyTable', function ($t) { // }); }\n}\n";
    let c = render_migration(legacy, "users", "phone");
    assert!(c.contains("class AddPhoneToUsersTable extends Migration"));
    assert!(c.contains("Schema::table('users'"));
    assert!(!c.contains("DummyTable"));
}

#[test]
fn inject_columns_only_touches_standalone_markers() {
    let src = "// header\n    //\n  not // a marker\n        //\n";
    let out = inject_columns(src, "x");
    assert!(out.starts_with("// header\n")); // inline header untouched
    assert!(out.contains("\n    $table->string('x');\n")); // first standalone marker
    assert!(out.contains("not // a marker")); // inline `//` untouched
    assert!(out.contains("\n        $table->dropColumn('x');\n")); // second marker
}

#[test]
fn migration_content_prefers_published_project_stub() {
    let dir = TempDir::new().unwrap();
    let stubs = dir.path().join("stubs");
    std::fs::create_dir_all(&stubs).unwrap();
    std::fs::write(
        stubs.join("migration.update.stub"),
        "<?php // CUSTOM STUB\nSchema::table('{{ table }}', function ($t) {\n    //\n});\n",
    )
    .unwrap();
    let c = migration_content(dir.path(), "users", "phone");
    assert!(
        c.contains("// CUSTOM STUB"),
        "should use the project's stub"
    );
    assert!(c.contains("Schema::table('users'"));
    assert!(c.contains("$table->string('phone');"));
}

#[test]
fn migration_content_falls_back_without_stub() {
    let dir = TempDir::new().unwrap(); // no stubs/, no vendor/
    let c = migration_content(dir.path(), "users", "phone");
    assert!(c.contains("return new class extends Migration"));
    assert!(c.contains("Schema::table('users'"));
    assert!(c.contains("$table->string('phone');"));
    assert!(c.contains("$table->dropColumn('phone');"));
    assert!(!c.contains("{{ table }}"));
}

// ---- rename_action --------------------------------------------------------

fn into_action(a: CodeActionOrCommand) -> tower_lsp::lsp_types::CodeAction {
    match a {
        CodeActionOrCommand::CodeAction(ca) => ca,
        CodeActionOrCommand::Command(_) => panic!("expected a CodeAction, got a Command"),
    }
}

#[test]
fn rename_action_replaces_diagnostic_range() {
    let d = diag(
        "laravel-lsp.unknown-column",
        json!({"kind": "column", "name": "emial", "replacement": "email", "replacementLabel": "email", "table": "users"}),
    );
    let action = into_action(rename_action(&d, &uri()).expect("a rename action"));
    assert_eq!(action.title, "Rename to `email`");
    assert_eq!(action.kind, Some(CodeActionKind::QUICKFIX));
    assert_eq!(action.is_preferred, Some(true));

    let changes = action.edit.unwrap().changes.unwrap();
    let edits = &changes[&uri()];
    assert_eq!(edits.len(), 1);
    assert_eq!(edits[0].new_text, "email");
    assert_eq!(edits[0].range, d.range);
}

#[test]
fn rename_action_for_dynamic_uses_studly_replacement_and_method_label() {
    // Dynamic where{Column}: the range covers the studly portion, so the edit
    // inserts `Email`, but the title shows the whole corrected method.
    let d = diag(
        "laravel-lsp.unknown-column",
        json!({"kind": "column", "name": "emaaaail", "dynamic": true,
               "replacement": "Email", "replacementLabel": "whereEmail", "table": "users"}),
    );
    let action = into_action(rename_action(&d, &uri()).expect("a rename action"));
    assert_eq!(action.title, "Rename to `whereEmail`");
    let changes = action.edit.unwrap().changes.unwrap();
    assert_eq!(changes[&uri()][0].new_text, "Email");
}

#[test]
fn rename_action_none_without_replacement() {
    // No suggestion was close enough → no replacement → no rename action.
    let d = diag(
        "laravel-lsp.unknown-column",
        json!({"kind": "column", "name": "zzzzz", "replacement": null, "table": "users"}),
    );
    assert!(rename_action(&d, &uri()).is_none());
}

#[test]
fn rename_action_none_for_foreign_source() {
    let mut d = diag(
        "laravel-lsp.unknown-column",
        json!({"kind": "column", "name": "emial", "replacement": "email"}),
    );
    d.source = Some("intelephense".to_string());
    assert!(rename_action(&d, &uri()).is_none());
}

// ---- migration_action -----------------------------------------------------

fn create_file_uri(action: &tower_lsp::lsp_types::CodeAction) -> Url {
    match &action.edit.as_ref().unwrap().document_changes {
        Some(DocumentChanges::Operations(ops)) => match &ops[0] {
            DocumentChangeOperation::Op(ResourceOp::Create(cf)) => cf.uri.clone(),
            _ => panic!("first op should be a CreateFile"),
        },
        _ => panic!("expected document_changes operations"),
    }
}

fn create_file_content(action: &tower_lsp::lsp_types::CodeAction) -> String {
    match &action.edit.as_ref().unwrap().document_changes {
        Some(DocumentChanges::Operations(ops)) => match &ops[1] {
            DocumentChangeOperation::Edit(te) => match &te.edits[0] {
                OneOf::Left(edit) => edit.new_text.clone(),
                _ => panic!("expected a plain TextEdit"),
            },
            _ => panic!("second op should be the content edit"),
        },
        _ => panic!("expected document_changes operations"),
    }
}

#[test]
fn migration_action_creates_timestamped_file_with_stub() {
    let d = diag(
        "laravel-lsp.unknown-column",
        json!({"kind": "column", "name": "phone", "table": "users"}),
    );
    let root = Path::new("/srv/app");
    let action =
        into_action(migration_action(&d, root, "2026_05_29_120000").expect("a migration action"));
    assert_eq!(
        action.title,
        "Create migration: add column `phone` to `users`"
    );
    assert_eq!(action.is_preferred, Some(false));

    let file_uri = create_file_uri(&action);
    assert!(
        file_uri
            .path()
            .ends_with("/database/migrations/2026_05_29_120000_add_phone_to_users_table.php"),
        "got: {}",
        file_uri.path()
    );

    let content = create_file_content(&action);
    assert!(content.contains("$table->string('phone');"));
    assert!(content.contains("Schema::table('users'"));
}

#[test]
fn migration_action_none_for_relation() {
    let d = diag(
        "laravel-lsp.unknown-relation",
        json!({"kind": "relation", "name": "postss", "replacement": "posts"}),
    );
    assert!(migration_action(&d, Path::new("/srv/app"), "2026_05_29_120000").is_none());
}

#[test]
fn migration_action_none_without_table() {
    let d = diag(
        "laravel-lsp.unknown-column",
        json!({"kind": "column", "name": "phone"}), // no table
    );
    assert!(migration_action(&d, Path::new("/srv/app"), "2026_05_29_120000").is_none());
}
