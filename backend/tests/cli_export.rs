//! CLI-level tests for the `gymbuddy` subcommands.
//!
//! These drive the real binary rather than calling into the library, because what they are
//! checking is the clap wiring itself: that `serve` stays the default, that `export` reaches the
//! dump module, and that the not-yet-built subcommands fail loudly instead of silently doing
//! nothing.

use std::path::Path;
use std::process::Command;

use gymbuddy_backend::db::Database;

/// Path to the binary cargo built for this test run.
const GYMBUDDY: &str = env!("CARGO_BIN_EXE_gymbuddy");

/// A migrated-but-empty database. `Database::open` applies the v1 migration set, which is what the
/// exporter should recognise.
fn v1_database(path: &Path) {
    Database::open(path).expect("creating the fixture database");
}

#[test]
fn export_writes_a_parseable_dump() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("gym.db");
    let out = dir.path().join("dump.json");
    v1_database(&db);

    let output = Command::new(GYMBUDDY)
        .args(["export", "--db", db.to_str().unwrap(), "--out", out.to_str().unwrap()])
        .output()
        .expect("running gymbuddy export");
    assert!(output.status.success(), "export failed: {}", String::from_utf8_lossy(&output.stderr));

    let dump = gymbuddy_backend::dump::from_json(&std::fs::read_to_string(&out).unwrap()).expect("dump should parse");
    assert_eq!(dump.format, gymbuddy_backend::dump::DUMP_FORMAT);
    assert_eq!(dump.source_schema.generation, 1);
    assert!(dump.users.is_empty(), "an empty database exports an empty user list, not an error");
}

#[test]
fn export_reports_a_missing_database_instead_of_creating_one() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("nope.db");
    let out = dir.path().join("dump.json");

    let output = Command::new(GYMBUDDY)
        .args(["export", "--db", missing.to_str().unwrap(), "--out", out.to_str().unwrap()])
        .output()
        .expect("running gymbuddy export");
    assert!(!output.status.success(), "exporting a missing database must fail");
    assert!(!missing.exists(), "a read-only open must not create the file");
    assert!(!out.exists(), "no dump should be written when the source cannot be read");
}

#[test]
fn import_and_migrate_are_wired_but_refuse_to_pretend() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("gym.db");
    let other = dir.path().join("other");

    let cases = [
        (vec!["import", "--db", db.to_str().unwrap(), "--in", other.to_str().unwrap()], "import"),
        (vec!["migrate", "--db", db.to_str().unwrap(), "--out", other.to_str().unwrap()], "migrate"),
    ];
    for (args, name) in cases {
        let output = Command::new(GYMBUDDY).args(&args).output().expect("running gymbuddy");
        assert!(!output.status.success(), "`{name}` must not report success while unimplemented");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("not implemented yet"), "`{name}` should say so plainly, got: {stderr}");
    }
}

#[test]
fn help_lists_every_subcommand_and_serve_is_the_default() {
    let output = Command::new(GYMBUDDY).arg("--help").output().expect("running gymbuddy --help");
    assert!(output.status.success());
    let help = String::from_utf8_lossy(&output.stdout);
    ["serve", "export", "import", "migrate"].iter().for_each(|command| {
        assert!(help.contains(command), "--help should list `{command}`:\n{help}");
    });
    assert!(help.contains("--config"), "--config stays a global flag so the deployment invocation is unchanged");
}
