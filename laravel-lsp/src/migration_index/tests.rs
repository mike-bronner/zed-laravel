use super::*;
use std::path::Path;
use tempfile::TempDir;

/// Extract the source text a `MigrationSite` points at, to assert the jump
/// target lands exactly on the column/table name.
fn text_at<'a>(content: &'a str, site: &MigrationSite) -> &'a str {
    let line = content.lines().nth(site.line as usize).unwrap();
    &line[site.start_char as usize..site.end_char as usize]
}

const CREATE_USERS: &str = r#"<?php

use Illuminate\Database\Migrations\Migration;
use Illuminate\Database\Schema\Blueprint;
use Illuminate\Support\Facades\Schema;

return new class extends Migration
{
    public function up(): void
    {
        Schema::create('users', function (Blueprint $table) {
            $table->id();
            $table->string('email')->unique();
            $table->string('name');
            $table->boolean('is_active')->default(true);
            $table->index('email');
            $table->timestamps();
        });
    }
};
"#;

#[test]
fn indexes_create_table_columns() {
    let mut index = MigrationIndex::default();
    index_migration_file(&mut index, Path::new("/m/create_users.php"), CREATE_USERS);

    // Table create site points at `users`.
    let table = index.table("users").expect("users table indexed");
    assert_eq!(text_at(CREATE_USERS, table), "users");

    // Columns from `$table->string(...)` / `boolean(...)`.
    let email = index.column("users", "email").expect("email column");
    assert_eq!(text_at(CREATE_USERS, email), "email");
    let name = index.column("users", "name").expect("name column");
    assert_eq!(text_at(CREATE_USERS, name), "name");
    assert!(index.column("users", "is_active").is_some());
}

#[test]
fn email_column_points_at_string_definition_not_the_index_call() {
    // Both `$table->string('email')` and `$table->index('email')` mention
    // email. The definition (string) wins — it's first AND `index` is not an
    // allowlisted column method.
    let mut index = MigrationIndex::default();
    index_migration_file(&mut index, Path::new("/m/create_users.php"), CREATE_USERS);
    let site = index.column("users", "email").unwrap();
    // Line 11 (0-based) is the `$table->string('email')` line.
    let line = CREATE_USERS.lines().nth(site.line as usize).unwrap();
    assert!(line.contains("string('email')"), "wrong line: {line}");
}

#[test]
fn email_and_email_prefixed_columns_are_distinct() {
    // Regression: clicking `email` must resolve to the `email` column, not the
    // prefix-sharing `email_verified_at`. Exact key match, distinct sites.
    let src = "<?php\nSchema::create('users', function ($table) {\n    $table->string('email', 64)->nullable();\n    $table->timestamp('email_verified_at')->nullable();\n});\n";
    let mut index = MigrationIndex::default();
    index_migration_file(&mut index, Path::new("/m/users.php"), src);
    let email = index.column("users", "email").expect("email column");
    let verified = index
        .column("users", "email_verified_at")
        .expect("email_verified_at column");
    assert_eq!(text_at(src, email), "email");
    assert_eq!(text_at(src, verified), "email_verified_at");
    assert_ne!(email.line, verified.line, "must resolve to distinct lines");
}

#[test]
fn does_not_index_reference_or_drop_methods() {
    let src = r#"<?php
Schema::table('users', function (Blueprint $table) {
    $table->dropColumn('legacy');
    $table->renameColumn('old', 'new');
    $table->index('email');
});
"#;
    let mut index = MigrationIndex::default();
    index_migration_file(&mut index, Path::new("/m/x.php"), src);
    assert!(index.column("users", "legacy").is_none());
    assert!(index.column("users", "old").is_none());
    // `index('email')` is a reference, not a definition.
    assert!(index.column("users", "email").is_none());
}

#[test]
fn indexes_add_column_via_schema_table() {
    let src = r#"<?php
Schema::table('posts', function (Blueprint $table) {
    $table->string('slug');
});
"#;
    let mut index = MigrationIndex::default();
    index_migration_file(&mut index, Path::new("/m/add_slug.php"), src);
    let slug = index.column("posts", "slug").expect("slug column");
    assert_eq!(text_at(src, slug), "slug");
    // Schema::table does NOT create the table, so no table create site.
    assert!(index.table("posts").is_none());
}

#[test]
fn matches_fully_qualified_schema_facade() {
    let src = r#"<?php
\Illuminate\Support\Facades\Schema::create('flags', function ($table) {
    $table->boolean('enabled');
});
"#;
    let mut index = MigrationIndex::default();
    index_migration_file(&mut index, Path::new("/m/flags.php"), src);
    assert!(index.table("flags").is_some());
    assert!(index.column("flags", "enabled").is_some());
}

#[test]
fn first_definition_wins() {
    // A later migration re-touching the column shouldn't overwrite the create
    // migration's definition site.
    let create = "<?php\nSchema::create('t', function ($table) {\n    $table->string('c');\n});\n";
    let modify =
        "<?php\nSchema::table('t', function ($table) {\n    $table->text('c')->change();\n});\n";
    let mut index = MigrationIndex::default();
    index_migration_file(&mut index, Path::new("/m/1_create.php"), create);
    index_migration_file(&mut index, Path::new("/m/2_modify.php"), modify);
    let site = index.column("t", "c").unwrap();
    assert_eq!(site.file, Path::new("/m/1_create.php"));
}

#[test]
fn build_index_reads_migrations_dir() {
    let dir = TempDir::new().unwrap();
    let migrations = dir.path().join("database").join("migrations");
    std::fs::create_dir_all(&migrations).unwrap();
    std::fs::write(
        migrations.join("2024_01_01_000000_create_users_table.php"),
        CREATE_USERS,
    )
    .unwrap();

    let index = build_migration_index(dir.path());
    assert!(index.table("users").is_some());
    assert!(index.column("users", "email").is_some());
    assert!(index.column_count() >= 3);
}

#[test]
fn build_index_empty_without_migrations_dir() {
    let dir = TempDir::new().unwrap();
    let index = build_migration_index(dir.path());
    assert_eq!(index.table_count(), 0);
    assert_eq!(index.column_count(), 0);
}
