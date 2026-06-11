//! Tests for the inheritance-chain member-read prover.

use super::*;
use std::path::PathBuf;
use tempfile::TempDir;

// ── source_reads_member (pure, no IO) ─────────────────────────────────────

#[test]
fn detects_instance_read_on_this() {
    let src = r#"<?php
class HasTimestamps {
    public function usesTimestamps() { return $this->timestamps; }
}
"#;
    assert!(source_reads_member(src, "timestamps"));
}

#[test]
fn detects_nullsafe_instance_read() {
    let src = "<?php\nclass C { public function f() { return $this?->flag; } }\n";
    assert!(source_reads_member(src, "flag"));
}

#[test]
fn detects_static_scoped_read() {
    let src = r#"<?php
class Model {
    protected static $snakeAttributes = true;
    public function f() { return static::$snakeAttributes; }
}
"#;
    assert!(source_reads_member(src, "snakeAttributes"));
}

#[test]
fn declaration_without_read_is_not_a_read() {
    // The property is declared but never read on $this — not proof of use.
    let src = "<?php\nclass C { public $timestamps = false; }\n";
    assert!(!source_reads_member(src, "timestamps"));
}

#[test]
fn read_on_a_different_receiver_is_not_a_this_read() {
    let src = "<?php\nclass C { public function f($other) { return $other->timestamps; } }\n";
    assert!(!source_reads_member(src, "timestamps"));
}

// ── member_read_in_chain (resolves + parses chain files) ───────────────────

/// Build a Laravel-shaped tempdir from (relpath, body) pairs.
fn project_with_files(files: &[(&str, &str)]) -> (TempDir, PathBuf) {
    let dir = TempDir::new().unwrap();
    for (relpath, body) in files {
        let full = dir.path().join(relpath);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(&full, body).unwrap();
    }
    let root = dir.path().to_path_buf();
    (dir, root)
}

#[test]
fn proves_read_via_vendor_parent_class() {
    // App model declares `public $flag`; the vendor parent reads `$this->flag`.
    let (_dir, root) = project_with_files(&[
        (
            "app/Models/Widget.php",
            "<?php\nnamespace App\\Models;\nuse Acme\\Base\\Thing;\nclass Widget extends Thing { public $flag = true; }\n",
        ),
        (
            "vendor/acme/base/src/Thing.php",
            "<?php\nnamespace Acme\\Base;\nclass Thing { public function check() { return $this->flag; } }\n",
        ),
    ]);
    assert!(member_read_in_chain(&root, "App\\Models\\Widget", "flag"));
}

#[test]
fn proves_read_via_vendor_trait() {
    // App model `use`s a vendor trait that reads the property.
    let (_dir, root) = project_with_files(&[
        (
            "app/Models/Gadget.php",
            "<?php\nnamespace App\\Models;\nuse Acme\\Base\\HasMode;\nclass Gadget { use HasMode; public $mode = 1; }\n",
        ),
        (
            "vendor/acme/base/src/HasMode.php",
            "<?php\nnamespace Acme\\Base;\ntrait HasMode { public function mode() { return $this->mode; } }\n",
        ),
    ]);
    assert!(member_read_in_chain(&root, "App\\Models\\Gadget", "mode"));
}

#[test]
fn unread_property_in_chain_is_not_proven() {
    let (_dir, root) = project_with_files(&[
        (
            "app/Models/Widget.php",
            "<?php\nnamespace App\\Models;\nuse Acme\\Base\\Thing;\nclass Widget extends Thing { public $orphan = true; }\n",
        ),
        (
            "vendor/acme/base/src/Thing.php",
            "<?php\nnamespace Acme\\Base;\nclass Thing { public function check() { return $this->flag; } }\n",
        ),
    ]);
    assert!(!member_read_in_chain(
        &root,
        "App\\Models\\Widget",
        "orphan"
    ));
}

#[test]
fn synthetic_component_key_returns_false_without_io() {
    // A `volt::<path>` key doesn't name a class — must short-circuit, not walk.
    let root = PathBuf::from("/nonexistent-root-should-not-be-touched");
    assert!(!member_read_in_chain(
        &root,
        "volt::/proj/resources/views/x.php",
        "count"
    ));
}

#[test]
fn empty_inputs_return_false() {
    let root = PathBuf::from("/tmp");
    assert!(!member_read_in_chain(&root, "", "x"));
    assert!(!member_read_in_chain(&root, "App\\Models\\Widget", ""));
}

// ── consumer scan (interface package reads the member duck-typed) ──────────

#[test]
fn proves_read_via_interface_package_consumer() {
    // Mike's SyncRedshiftFromQueue case: the job's `$tries` is read by NOTHING
    // in its chain — the framework reads `$job->tries` in the package that
    // owns the ShouldQueue contract the job implements.
    let (_dir, root) = project_with_files(&[
        (
            "app/Jobs/SyncJob.php",
            "<?php\nnamespace App\\Jobs;\nuse Acme\\Queue\\Contracts\\ShouldQueue;\nclass SyncJob implements ShouldQueue { public int $tries = 1; }\n",
        ),
        (
            "vendor/acme/queue/src/Contracts/ShouldQueue.php",
            "<?php\nnamespace Acme\\Queue\\Contracts;\ninterface ShouldQueue {}\n",
        ),
        (
            "vendor/acme/queue/src/Queue.php",
            "<?php\nnamespace Acme\\Queue;\nclass Queue { public function getJobTries($job) { return $job->tries ?? null; } }\n",
        ),
    ]);
    assert!(member_read_in_chain(&root, "App\\Jobs\\SyncJob", "tries"));
}

#[test]
fn proves_read_via_interface_implemented_by_vendor_parent() {
    // The interface comes from a vendor PARENT in the chain, not the app class
    // itself — interfaces must be collected across the whole walk.
    let (_dir, root) = project_with_files(&[
        (
            "app/Jobs/SyncJob.php",
            "<?php\nnamespace App\\Jobs;\nuse Acme\\Queue\\BaseJob;\nclass SyncJob extends BaseJob { public int $timeout = 30; }\n",
        ),
        (
            "vendor/acme/queue/src/BaseJob.php",
            "<?php\nnamespace Acme\\Queue;\nuse Acme\\Queue\\Contracts\\ShouldQueue;\nabstract class BaseJob implements ShouldQueue {}\n",
        ),
        (
            "vendor/acme/queue/src/Contracts/ShouldQueue.php",
            "<?php\nnamespace Acme\\Queue\\Contracts;\ninterface ShouldQueue {}\n",
        ),
        (
            "vendor/acme/queue/src/Worker.php",
            "<?php\nnamespace Acme\\Queue;\nclass Worker { public function run($job) { return $job?->timeout; } }\n",
        ),
    ]);
    assert!(member_read_in_chain(&root, "App\\Jobs\\SyncJob", "timeout"));
}

#[test]
fn member_unread_by_interface_package_is_not_proven() {
    // The package owns the interface but never touches the member — the scan
    // must not turn "implements a vendor interface" into blanket immunity.
    let (_dir, root) = project_with_files(&[
        (
            "app/Jobs/SyncJob.php",
            "<?php\nnamespace App\\Jobs;\nuse Acme\\Queue\\Contracts\\ShouldQueue;\nclass SyncJob implements ShouldQueue { public $orphan = true; }\n",
        ),
        (
            "vendor/acme/queue/src/Contracts/ShouldQueue.php",
            "<?php\nnamespace Acme\\Queue\\Contracts;\ninterface ShouldQueue {}\n",
        ),
        (
            "vendor/acme/queue/src/Queue.php",
            "<?php\nnamespace Acme\\Queue;\nclass Queue { public function getJobTries($job) { return $job->tries ?? null; } }\n",
        ),
    ]);
    assert!(!member_read_in_chain(&root, "App\\Jobs\\SyncJob", "orphan"));
}

#[test]
fn method_call_in_consumer_is_not_a_property_read() {
    // `$job->retry()` is a method call, not a property access — it must not
    // prove a PROPERTY named `retry`.
    let (_dir, root) = project_with_files(&[
        (
            "app/Jobs/SyncJob.php",
            "<?php\nnamespace App\\Jobs;\nuse Acme\\Queue\\Contracts\\ShouldQueue;\nclass SyncJob implements ShouldQueue { public $retry = 3; }\n",
        ),
        (
            "vendor/acme/queue/src/Contracts/ShouldQueue.php",
            "<?php\nnamespace Acme\\Queue\\Contracts;\ninterface ShouldQueue {}\n",
        ),
        (
            "vendor/acme/queue/src/Queue.php",
            "<?php\nnamespace Acme\\Queue;\nclass Queue { public function run($job) { return $job->retry(); } }\n",
        ),
    ]);
    assert!(!member_read_in_chain(&root, "App\\Jobs\\SyncJob", "retry"));
}

#[test]
fn app_side_interface_does_not_trigger_consumer_scan() {
    // The interface resolves into app/, not vendor/ — app-side consumer reads
    // are ordinary project references the index already counts, so the prover
    // must not claim them.
    let (_dir, root) = project_with_files(&[
        (
            "app/Jobs/SyncJob.php",
            "<?php\nnamespace App\\Jobs;\nuse App\\Contracts\\Trackable;\nclass SyncJob implements Trackable { public $steps = 5; }\n",
        ),
        (
            "app/Contracts/Trackable.php",
            "<?php\nnamespace App\\Contracts;\ninterface Trackable {}\n",
        ),
        (
            "app/Services/Tracker.php",
            "<?php\nnamespace App\\Services;\nclass Tracker { public function f($t) { return $t->steps; } }\n",
        ),
    ]);
    assert!(!member_read_in_chain(&root, "App\\Jobs\\SyncJob", "steps"));
}

#[test]
fn extends_cycle_terminates() {
    // A extends B, B extends A — the visited-set must break the cycle and the
    // walk must return (this test failing = an infinite loop / hang).
    let (_dir, root) = project_with_files(&[
        (
            "app/Models/A.php",
            "<?php\nnamespace App\\Models;\nuse App\\Models\\B;\nclass A extends B { public $x = 1; }\n",
        ),
        (
            "app/Models/B.php",
            "<?php\nnamespace App\\Models;\nuse App\\Models\\A;\nclass B extends A {}\n",
        ),
    ]);
    assert!(!member_read_in_chain(&root, "App\\Models\\A", "x"));
}
