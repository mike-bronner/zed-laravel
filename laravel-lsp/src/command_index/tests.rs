//! Unit tests for the project-wide Artisan command index.

use super::*;
use std::path::Path;

/// A minimal Artisan command class declaring `signature`.
fn command_class(class: &str, signature: &str) -> String {
    format!(
        "<?php\n\nnamespace App\\Console\\Commands;\n\nuse Illuminate\\Console\\Command;\n\nclass {class} extends Command\n{{\n    protected $signature = '{signature}';\n\n    public function handle()\n    {{\n        //\n    }}\n}}\n"
    )
}

#[test]
fn class_name_extracted_from_declaration() {
    assert_eq!(
        class_name_from_content("<?php\nclass SendEmails extends Command {}").as_deref(),
        Some("SendEmails")
    );
    assert_eq!(class_name_from_content("<?php\n$x = 1;").as_deref(), None);
}

#[test]
fn priority_classifies_by_path() {
    assert_eq!(
        classify_priority(Path::new("/app/app/Console/Commands/SendEmails.php")),
        CommandPriority::App
    );
    assert_eq!(
        classify_priority(Path::new(
            "/app/vendor/spatie/backup/src/Commands/Backup.php"
        )),
        CommandPriority::Package
    );
    assert_eq!(
        classify_priority(Path::new(
            "/app/vendor/laravel/framework/src/Illuminate/Queue/Console/WorkCommand.php"
        )),
        CommandPriority::Framework
    );
}

#[test]
fn indexes_a_command_and_resolves_it() {
    let mut index = CommandIndex::default();
    let src = command_class("SendEmails", "emails:send {user} {--force}");
    index_command_file(
        &mut index,
        Path::new("/app/app/Console/Commands/SendEmails.php"),
        &src,
    );

    let entry = index
        .resolve("emails:send")
        .expect("command should resolve");
    assert_eq!(entry.name, "emails:send");
    assert_eq!(entry.class_name, "SendEmails");
    assert_eq!(entry.raw_signature, "emails:send {user} {--force}");
    assert_eq!(entry.priority, CommandPriority::App);
    assert_eq!(index.len(), 1);
}

#[test]
fn non_command_files_are_ignored() {
    let mut index = CommandIndex::default();
    index_command_file(
        &mut index,
        Path::new("/app/app/Models/User.php"),
        "<?php\nclass User extends Model {\n    protected $table = 'users';\n}",
    );
    assert!(index.is_empty());
}

#[test]
fn command_without_signature_is_ignored() {
    let mut index = CommandIndex::default();
    index_command_file(
        &mut index,
        Path::new("/app/app/Console/Commands/Dynamic.php"),
        "<?php\nclass Dynamic extends Command {\n    public function handle() {}\n}",
    );
    assert!(index.is_empty());
}

#[test]
fn app_command_overrides_package_with_same_name() {
    let mut index = CommandIndex::default();
    // Package declares queue:work first…
    index_command_file(
        &mut index,
        Path::new("/app/vendor/laravel/horizon/src/Console/WorkCommand.php"),
        &command_class("PackageWork", "queue:work"),
    );
    // …then the app overrides it.
    index_command_file(
        &mut index,
        Path::new("/app/app/Console/Commands/WorkCommand.php"),
        &command_class("AppWork", "queue:work"),
    );

    let entry = index.resolve("queue:work").expect("should resolve");
    assert_eq!(entry.class_name, "AppWork");
    assert_eq!(entry.priority, CommandPriority::App);
}

#[test]
fn lower_priority_does_not_clobber_higher() {
    let mut index = CommandIndex::default();
    // App declared first…
    index_command_file(
        &mut index,
        Path::new("/app/app/Console/Commands/WorkCommand.php"),
        &command_class("AppWork", "queue:work"),
    );
    // …a later package declaration must NOT replace it.
    index_command_file(
        &mut index,
        Path::new("/app/vendor/laravel/framework/src/WorkCommand.php"),
        &command_class("FrameworkWork", "queue:work"),
    );

    let entry = index.resolve("queue:work").expect("should resolve");
    assert_eq!(entry.class_name, "AppWork");
    assert_eq!(entry.priority, CommandPriority::App);
}

#[test]
fn build_index_walks_project_and_vendor() {
    let dir = std::env::temp_dir().join(format!("cmd-index-test-{}", std::process::id()));
    let app_cmds = dir.join("app/Console/Commands");
    let vendor_cmds = dir.join("vendor/acme/pkg/src/Commands");
    std::fs::create_dir_all(&app_cmds).unwrap();
    std::fs::create_dir_all(&vendor_cmds).unwrap();
    std::fs::write(
        app_cmds.join("SendEmails.php"),
        command_class("SendEmails", "emails:send"),
    )
    .unwrap();
    std::fs::write(
        vendor_cmds.join("Backup.php"),
        command_class("Backup", "backup:run"),
    )
    .unwrap();
    // A non-command file should be skipped.
    std::fs::write(
        app_cmds.join("NotACommand.php"),
        "<?php\nclass NotACommand {}",
    )
    .unwrap();

    let index = build_command_index(&dir);

    assert_eq!(index.len(), 2);
    assert_eq!(
        index.resolve("emails:send").unwrap().priority,
        CommandPriority::App
    );
    assert_eq!(
        index.resolve("backup:run").unwrap().priority,
        CommandPriority::Package
    );

    std::fs::remove_dir_all(&dir).ok();
}
