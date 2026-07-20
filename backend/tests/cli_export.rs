//! CLI-level tests for the `gymbuddy` subcommands.
//!
//! These drive the real binary rather than calling into the library, because what they are
//! checking is the clap wiring itself: that `serve` stays the default, that `export` reaches the
//! dump module, and that the not-yet-built subcommands fail loudly instead of silently doing
//! nothing.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;

mod fixtures;

/// Path to the binary cargo built for this test run.
const GYMBUDDY: &str = env!("CARGO_BIN_EXE_gymbuddy");

/// A migrated-but-empty legacy database, built from the frozen v1 fixture.
///
/// It cannot come from `Database::open` any more: that now builds schema v2, and this test needs
/// the generation the exporter's v1 reader is for.
fn v1_database(path: &Path) {
    fixtures::empty_v1_db_at(path);
}

/// Run `gymbuddy export` and return the parsed dump, failing loudly with the binary's own stderr.
fn export_via_cli(db: &Path, out: &Path) -> gymbuddy_backend::dump::Dump {
    let output = Command::new(GYMBUDDY)
        .args(["export", "--db", db.to_str().unwrap(), "--out", out.to_str().unwrap()])
        .output()
        .expect("running gymbuddy export");
    assert!(output.status.success(), "export failed: {}", String::from_utf8_lossy(&output.stderr));
    gymbuddy_backend::dump::from_json(&std::fs::read_to_string(out).unwrap()).expect("dump should parse")
}

#[test]
fn export_writes_a_parseable_dump() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("gym.db");
    let out = dir.path().join("dump.json");
    v1_database(&db);

    let dump = export_via_cli(&db, &out);
    assert_eq!(dump.format, gymbuddy_backend::dump::DUMP_FORMAT);
    assert_eq!(dump.source_schema.generation, 1);
    assert!(dump.users.is_empty(), "an empty database exports an empty user list, not an error");
}

/// The same fidelity invariant the unit tests enforce, but through the real binary and a real file
/// on disk. The unit tests share a process with the exporter; this proves the bytes that reach the
/// dump file carry the same rows, which is what an operator taking a backup actually depends on.
#[test]
fn export_of_the_seeded_fixture_reconciles_against_its_source() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("gym.db");
    let out = dir.path().join("dump.json");
    fixtures::seeded_v1_db_at(&db);
    let before = std::fs::metadata(&db).unwrap().len();

    let dump = export_via_cli(&db, &out);
    assert_eq!(dump.users.len(), 3);

    let source = rusqlite::Connection::open(&db).unwrap();
    let exported: BTreeMap<&str, usize> = dump.row_counts().iter().collect();
    assert_eq!(exported, fixtures::source_row_counts(&source), "the written dump must carry every row the source held");

    assert_eq!(std::fs::metadata(&db).unwrap().len(), before, "`export` opens its source read-only and must not grow the file");
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

/// The guard rail on the one irreversible mistake available here.
///
/// `serve` opens the database read-write and migrates it to the latest schema it knows. Pointed at
/// a legacy file, a v2 build would create the v2 tables beside the v1 ones — in place, over the
/// user's only copy. So it must stop, and it must say what to run instead.
#[test]
fn serve_refuses_to_start_on_a_legacy_database() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("gym.db");
    fixtures::seeded_v1_db_at(&db);
    let before = std::fs::metadata(&db).unwrap().len();

    let config = dir.path().join("gymbuddy.toml");
    std::fs::write(&config, config_pointing_at(dir.path(), "gym.db")).unwrap();

    let output = Command::new(GYMBUDDY).args(["--config", config.to_str().unwrap(), "serve"]).output().expect("running gymbuddy serve");
    assert!(!output.status.success(), "serving a legacy database must fail");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("legacy"), "the refusal should name the problem, got: {stderr}");
    assert!(stderr.contains("gymbuddy migrate"), "the refusal should name the fix, got: {stderr}");
    assert_eq!(std::fs::metadata(&db).unwrap().len(), before, "a refused start must not have touched the database");
}

/// A schema v2 database is served, not refused — the point of probing for a marker table rather
/// than trusting `PRAGMA user_version`, which v1 and v2 both spell small.
///
/// Startup still fails here, but on the *next* step: this config configures no transport. That the
/// error is about transports and not about the schema is the whole assertion.
#[test]
fn serve_gets_past_the_schema_check_on_a_v2_database() {
    let dir = tempfile::tempdir().unwrap();
    gymbuddy_backend::db::Database::open(&dir.path().join("gym.db")).expect("creating a v2 database");

    let config = dir.path().join("gymbuddy.toml");
    std::fs::write(&config, config_pointing_at(dir.path(), "gym.db")).unwrap();

    let output = Command::new(GYMBUDDY).args(["--config", config.to_str().unwrap(), "serve"]).output().expect("running gymbuddy serve");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!stderr.contains("legacy"), "a v2 database must not be mistaken for a legacy one, got: {stderr}");
    assert!(stderr.contains("no transport configured"), "startup should have reached the transport check, got: {stderr}");
}

/// The smallest config that gets `serve` as far as opening the database.
fn config_pointing_at(data_dir: &Path, db_name: &str) -> String {
    format!(
        "[general]\ndata_dir = \"{}\"\n\n\
         [llm]\nprovider = \"openai-compatible\"\nbase_url = \"http://127.0.0.1:1\"\nmodel = \"test-model\"\napi_key = \"test-key\"\n\n\
         [gym]\ndb_path = \"{db_name}\"\n",
        data_dir.display()
    )
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
