//! CLI-level tests for the `gymbuddy` subcommands.
//!
//! These drive the real binary rather than calling into the library, because what they are
//! checking is the clap wiring itself: that `serve` stays the default, that `export` reaches the
//! dump module, and that `import` and `migrate` behave on real files on disk — including the two
//! refusals that protect the only copy of the user's data.

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

/// Run a subcommand and return `(success, stderr)`.
fn run(args: &[&str]) -> (bool, String) {
    let output = Command::new(GYMBUDDY).args(args).output().expect("running gymbuddy");
    (output.status.success(), String::from_utf8_lossy(&output.stderr).to_string())
}

/// The migration end to end, through the real binary: a seeded legacy database in, a verified
/// schema v2 database out, and the original untouched.
///
/// `--verify` is on by default and is not passed here on purpose — the default is the safety
/// property, and a test that opted into it would not notice the default flipping.
#[test]
fn migrate_builds_a_verified_v2_database_and_leaves_the_original_alone() {
    let dir = tempfile::tempdir().unwrap();
    let old = dir.path().join("gym.db");
    let new = dir.path().join("gym.v2.db");
    fixtures::seeded_v1_db_at(&old);
    let before = std::fs::metadata(&old).unwrap().len();
    let before_bytes = std::fs::read(&old).unwrap();

    let (ok, stderr) = run(&["migrate", "--db", old.to_str().unwrap(), "--out", new.to_str().unwrap()]);
    assert!(ok, "migrate failed: {stderr}");
    assert!(stderr.contains("Verification passed"), "verification should have run by default, got: {stderr}");

    assert_eq!(std::fs::metadata(&old).unwrap().len(), before, "the source must not have been written");
    assert_eq!(std::fs::read(&old).unwrap(), before_bytes, "the source must be byte-identical — it is the rollback");

    // The new file is a v2 database holding the same rows.
    assert!(!gymbuddy_backend::dump::is_legacy_database(&new).unwrap(), "the output must be schema v2");
    let source = rusqlite::Connection::open(&old).unwrap();
    let migrated = gymbuddy_backend::dump::export_path(&new).unwrap();
    let counts: BTreeMap<&str, usize> = migrated.row_counts().iter().collect();
    let expected: BTreeMap<&str, usize> = fixtures::source_row_counts(&source)
        .into_iter()
        // Schedules and `signal_id` are dropped by schema v2 by design, so a v2 export carries none.
        .map(|(key, count)| (key, if key.starts_with("legacy_") { 0 } else { count }))
        .collect();
    assert_eq!(counts, expected, "every non-archival row must have survived the migration");
}

/// A migration that would clobber an existing file is refused outright. `--out` is a new database,
/// and guessing that an existing one is disposable is not a guess this tool gets to make.
#[test]
fn migrate_refuses_to_overwrite_an_existing_output() {
    let dir = tempfile::tempdir().unwrap();
    let old = dir.path().join("gym.db");
    let new = dir.path().join("taken.db");
    fixtures::seeded_v1_db_at(&old);
    std::fs::write(&new, b"not a database").unwrap();

    let (ok, stderr) = run(&["migrate", "--db", old.to_str().unwrap(), "--out", new.to_str().unwrap()]);
    assert!(!ok, "migrating onto an existing file must fail");
    assert!(stderr.contains("already exists"), "the refusal should say why, got: {stderr}");
    assert_eq!(std::fs::read(&new).unwrap(), b"not a database", "the existing file must be untouched");
}

/// `import` is the restore half of the backup tool: a dump written by `export` loads into a fresh
/// database.
#[test]
fn import_loads_a_dump_into_a_fresh_database() {
    let dir = tempfile::tempdir().unwrap();
    let legacy = dir.path().join("gym.db");
    let dump_file = dir.path().join("dump.json");
    let restored = dir.path().join("restored.db");
    fixtures::seeded_v1_db_at(&legacy);
    let dump = export_via_cli(&legacy, &dump_file);

    let (ok, stderr) = run(&["import", "--db", restored.to_str().unwrap(), "--in", dump_file.to_str().unwrap()]);
    assert!(ok, "import failed: {stderr}");

    let reexported = gymbuddy_backend::dump::export_path(&restored).unwrap();
    assert_eq!(reexported.users.len(), dump.users.len());
    let differences = gymbuddy_backend::dump::compare::compare(&dump, &reexported);
    assert!(differences.is_empty(), "a restored dump must match the one that was written: {differences:?}");
}

/// Importing into a database that already holds data would merge two id spaces with no way to tell
/// the halves apart afterwards.
#[test]
fn import_refuses_a_database_that_already_holds_data() {
    let dir = tempfile::tempdir().unwrap();
    let legacy = dir.path().join("gym.db");
    let dump_file = dir.path().join("dump.json");
    let target = dir.path().join("target.db");
    fixtures::seeded_v1_db_at(&legacy);
    export_via_cli(&legacy, &dump_file);

    let args = ["import", "--db", target.to_str().unwrap(), "--in", dump_file.to_str().unwrap()];
    assert!(run(&args).0, "the first import should succeed");
    let (ok, stderr) = run(&args);
    assert!(!ok, "a second import must be refused");
    assert!(stderr.contains("already holds data"), "the refusal should say why, got: {stderr}");
}

/// `import` must not be a way to trip the trap `serve` refuses to walk into: pointed at a legacy
/// file, `Database::open` would build the v2 tables beside the v1 ones, in place.
#[test]
fn import_refuses_a_legacy_target_database() {
    let dir = tempfile::tempdir().unwrap();
    let legacy = dir.path().join("gym.db");
    let dump_file = dir.path().join("dump.json");
    fixtures::seeded_v1_db_at(&legacy);
    export_via_cli(&legacy, &dump_file);
    let before = std::fs::read(&legacy).unwrap();

    let (ok, stderr) = run(&["import", "--db", legacy.to_str().unwrap(), "--in", dump_file.to_str().unwrap()]);
    assert!(!ok, "importing into a legacy database must fail");
    assert!(stderr.contains("legacy"), "the refusal should name the problem, got: {stderr}");
    assert_eq!(std::fs::read(&legacy).unwrap(), before, "the legacy file must not have been migrated in place");
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

/// The deployment gate has to reach the person running the migration, and that person is at a
/// terminal, not in `dump/migrate.rs`. `--help` is the one place they are certain to look.
#[test]
fn migrate_help_carries_the_deployment_gate() {
    let output = Command::new(GYMBUDDY).args(["migrate", "--help"]).output().expect("running gymbuddy migrate --help");
    assert!(output.status.success());
    let help = String::from_utf8_lossy(&output.stdout);
    assert!(help.contains("DEPLOYMENT GATE"), "the gate should be in the help:\n{help}");
    ["Stop the bot", "Rehearse on a copy", "KEEP THE OLD FILE"].iter().for_each(|step| {
        assert!(help.contains(step), "the gate is missing `{step}`:\n{help}");
    });
}
