//! Tests for the column rename engine (M8). All positions 0-based.

use super::*;

// ── chain_column_literals ─────────────────────────────────────────────────

/// Helper: assert a single site at the given 0-based coords and that it
/// brackets exactly `expected_text` in `source`.
fn assert_site(source: &str, site: &ColumnArgSite, line: u32, expected_text: &str) {
    assert_eq!(site.line, line, "line mismatch");
    let line_text = source.lines().nth(line as usize).expect("line exists");
    let start = site.start_column as usize;
    let end = site.end_column as usize;
    assert!(end <= line_text.len(), "end column past line length");
    assert_eq!(&line_text[start..end], expected_text, "bracketed text");
}

#[test]
fn finds_first_arg_column_in_where() {
    let src = "<?php\n$q = User::where('email', $value);\n";
    let lits = chain_column_literals(src, "email");
    assert_eq!(lits.len(), 1);
    assert_site(src, &lits[0].site, 1, "email");
}

#[test]
fn ignores_value_args_in_where() {
    // `where('status', '=', 'email')` — the third arg is a VALUE, not a column.
    // Renaming column `email` must NOT touch it.
    let src = "<?php\n$q = User::where('status', '=', 'email');\n";
    let lits = chain_column_literals(src, "email");
    assert!(
        lits.is_empty(),
        "value arg 'email' must not be treated as a column"
    );
}

#[test]
fn finds_all_args_in_select() {
    // select() is a multi-column method — every matching string arg counts.
    let src = "<?php\n$q = User::select('id', 'email', 'name')->where('email', 1);\n";
    let lits = chain_column_literals(src, "email");
    // One in select(), one as the first arg of where().
    assert_eq!(lits.len(), 2);
    for lit in &lits {
        assert_site(src, &lit.site, 1, "email");
    }
}

#[test]
fn finds_orderby_and_pluck_and_groupby() {
    let src = "<?php\n$a = User::orderBy('email');\n$b = User::pluck('email');\n$c = User::groupBy('email');\n";
    let lits = chain_column_literals(src, "email");
    assert_eq!(lits.len(), 3);
    assert_site(src, &lits[0].site, 1, "email");
    assert_site(src, &lits[1].site, 2, "email");
    assert_site(src, &lits[2].site, 3, "email");
}

#[test]
fn qualified_column_brackets_only_the_name() {
    // `'users.email'` → rewrite only the `email` segment, keep `users.`.
    let src = "<?php\n$q = User::where('users.email', 1);\n";
    let lits = chain_column_literals(src, "email");
    assert_eq!(lits.len(), 1);
    assert_site(src, &lits[0].site, 1, "email");
    // The site must start AFTER the `users.` prefix.
    let line_text = src.lines().nth(1).unwrap();
    let dot = line_text.find("users.email").unwrap() + "users.".len();
    assert_eq!(lits[0].site.start_column as usize, dot);
}

#[test]
fn qualified_column_with_other_table_still_matches_on_name() {
    // The pure finder matches by column name regardless of qualifier — the
    // integration layer's table filter decides whether to keep it.
    let src = "<?php\n$q = DB::table('users')->where('orders.email', 1);\n";
    let lits = chain_column_literals(src, "email");
    assert_eq!(lits.len(), 1);
    assert_site(src, &lits[0].site, 1, "email");
}

#[test]
fn no_match_for_different_column() {
    let src = "<?php\n$q = User::where('email', 1);\n";
    let lits = chain_column_literals(src, "name");
    assert!(lits.is_empty());
}

#[test]
fn double_quoted_column_arg_matches() {
    let src = "<?php\n$q = User::where(\"email\", 1);\n";
    let lits = chain_column_literals(src, "email");
    assert_eq!(lits.len(), 1);
    assert_site(src, &lits[0].site, 1, "email");
}

#[test]
fn chain_index_identifies_owning_chain() {
    let src = "<?php\n$a = User::where('email', 1);\n$b = Order::where('email', 2);\n";
    let lits = chain_column_literals(src, "email");
    assert_eq!(lits.len(), 2);
    // Two distinct chains → two distinct indices.
    assert_ne!(lits[0].chain_index, lits[1].chain_index);
}

#[test]
fn parse_failure_yields_empty() {
    // Garbage that still "parses" to a tree with no chains → empty, not panic.
    let src = "not php at all {{{";
    let lits = chain_column_literals(src, "email");
    assert!(lits.is_empty());
}

// ── model_array_sites ─────────────────────────────────────────────────────

#[test]
fn fillable_flat_array_site() {
    let src = "<?php\nclass User extends Model {\n    protected $fillable = ['name', 'email', 'role'];\n}\n";
    let sites = model_array_sites(src, "email");
    assert_eq!(sites.len(), 1);
    assert_site(src, &sites[0], 2, "email");
}

#[test]
fn casts_key_site_only_not_value() {
    // `$casts = ['email' => 'string']` — rewrite the KEY `email`, never a value
    // that happens to read `email`.
    let src = "<?php\nclass User extends Model {\n    protected $casts = ['email' => 'string', 'verified' => 'email'];\n}\n";
    let sites = model_array_sites(src, "email");
    // Only the key on the first entry — the second entry's value 'email' is a
    // cast type, not a column.
    assert_eq!(sites.len(), 1);
    assert_site(src, &sites[0], 2, "email");
    let line_text = src.lines().nth(2).unwrap();
    // The matched key is the FIRST 'email' occurrence.
    assert_eq!(
        sites[0].start_column as usize,
        line_text.find("email").unwrap()
    );
}

#[test]
fn hidden_and_guarded_and_dates_arrays() {
    let src = "<?php\nclass User extends Model {\n    protected $hidden = ['email'];\n    protected $guarded = ['email'];\n    protected $dates = ['email'];\n}\n";
    let sites = model_array_sites(src, "email");
    assert_eq!(sites.len(), 3);
    assert_site(src, &sites[0], 2, "email");
    assert_site(src, &sites[1], 3, "email");
    assert_site(src, &sites[2], 4, "email");
}

#[test]
fn unrelated_property_array_ignored() {
    // A random `$something` array with 'email' must not be touched.
    let src = "<?php\nclass User extends Model {\n    protected $appends = ['email'];\n}\n";
    let sites = model_array_sites(src, "email");
    assert!(sites.is_empty());
}

#[test]
fn model_array_no_match_for_other_column() {
    let src = "<?php\nclass User extends Model {\n    protected $fillable = ['name', 'role'];\n}\n";
    let sites = model_array_sites(src, "email");
    assert!(sites.is_empty());
}

// ── migration generation ──────────────────────────────────────────────────

#[test]
fn migration_filename_shape() {
    let name = rename_migration_filename("2026_06_09_120000", "email", "email_address", "users");
    assert_eq!(
        name,
        "2026_06_09_120000_rename_email_to_email_address_in_users_table.php"
    );
}

#[test]
fn migration_content_renames_forward_and_reverses() {
    let content = rename_migration_content("users", "email", "email_address");
    // up(): old → new.
    assert!(content.contains("Schema::table('users'"));
    assert!(content.contains("$table->renameColumn('email', 'email_address');"));
    // down(): new → old (reverse).
    assert!(content.contains("$table->renameColumn('email_address', 'email');"));
    // Well-formed anonymous-class migration.
    assert!(content.starts_with("<?php"));
    assert!(content.contains("return new class extends Migration"));
    assert!(content.contains("public function up(): void"));
    assert!(content.contains("public function down(): void"));
}

#[test]
fn migration_content_orders_up_before_down() {
    let content = rename_migration_content("users", "email", "login");
    let fwd = content.find("renameColumn('email', 'login')").unwrap();
    let rev = content.find("renameColumn('login', 'email')").unwrap();
    assert!(
        fwd < rev,
        "up() (forward) must come before down() (reverse)"
    );
}
