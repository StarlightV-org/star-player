//! Append migrations to MIGRATIONS; never edit a released migration.
use super::{History, MediaState, read_legacy_history, validate_gains, validate_resume_position};
use rusqlite::{Connection, TransactionBehavior, params};
use std::{fs, path::Path, time::Duration};

const MIGRATIONS: &[&str] = &["CREATE TABLE media (
        sha256 TEXT PRIMARY KEY NOT NULL CHECK(length(sha256) = 64),
        identity_version INTEGER NOT NULL CHECK(identity_version IN (1, 2)),
        watched_at TEXT NOT NULL,
        resume_position REAL CHECK(resume_position > 0),
        track_gains TEXT NOT NULL,
        file_settings TEXT NOT NULL
    );"];

fn validate(key: &str, state: &MediaState) -> Result<(), String> {
    if key.len() != 64
        || !key
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        || !matches!(state.identity_version, 1 | 2)
        || (state.watched_at.is_empty() && state.identity_version == 1)
        || (!state.watched_at.is_empty()
            && chrono::DateTime::parse_from_rfc3339(&state.watched_at).is_err())
    {
        return Err(
            "Invalid media identity or watched timestamp; library.db left unchanged".into(),
        );
    }
    validate_gains(&state.track_gains)?;
    validate_resume_position(state.resume_position)?;
    state.file_settings.validate()
}

fn load(db: &Connection) -> Result<History, String> {
    let mut statement = db.prepare(
        "SELECT sha256, identity_version, watched_at, resume_position, track_gains, file_settings FROM media",
    ).map_err(|e| e.to_string())?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, u32>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<f64>>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
            ))
        })
        .map_err(|e| e.to_string())?;
    let mut history = History::new();
    for row in rows {
        let (key, identity_version, watched_at, resume_position, gains, settings) =
            row.map_err(|e| e.to_string())?;
        let state = MediaState {
            identity_version,
            watched_at,
            resume_position,
            track_gains: serde_json::from_str(&gains).map_err(|e| e.to_string())?,
            file_settings: serde_json::from_str(&settings).map_err(|e| e.to_string())?,
        };
        validate(&key, &state)?;
        history.insert(key, state);
    }
    Ok(history)
}

fn put(db: &Connection, key: &str, state: &MediaState) -> Result<(), String> {
    db.execute(
        "INSERT INTO media (sha256, identity_version, watched_at, resume_position, track_gains, file_settings)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(sha256) DO UPDATE SET identity_version=excluded.identity_version,
         watched_at=excluded.watched_at, resume_position=excluded.resume_position,
         track_gains=excluded.track_gains, file_settings=excluded.file_settings",
        params![key, state.identity_version, state.watched_at, state.resume_position,
            serde_json::to_string(&state.track_gains).map_err(|e| e.to_string())?,
            serde_json::to_string(&state.file_settings).map_err(|e| e.to_string())?],
    ).map_err(|e| e.to_string())?;
    Ok(())
}

fn migrate(db: &mut Connection, dir: &Path, migrations: &[&str]) -> Result<(), String> {
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|e| e.to_string())?;
    let version: u32 = tx
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .map_err(|e| e.to_string())?;
    let version = version as usize;
    if version > migrations.len() {
        return Err("library.db was created by a newer app; update StarPlayer to open it".into());
    }
    let legacy = if version == 0 {
        let tables: i64 = tx
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE name NOT LIKE 'sqlite_%'",
                [],
                |r| r.get(0),
            )
            .map_err(|e| e.to_string())?;
        if tables != 0 {
            return Err("Unrecognized library.db schema; database left unchanged".into());
        }
        Some(read_legacy_history(dir)?)
    } else {
        None
    };
    for (index, sql) in migrations.iter().enumerate().skip(version) {
        tx.execute_batch(sql).map_err(|e| e.to_string())?;
        tx.pragma_update(None, "user_version", (index + 1) as u32)
            .map_err(|e| e.to_string())?;
    }
    if let Some(history) = legacy {
        for (key, state) in &history {
            validate(key, state)?;
            put(&tx, key, state)?;
        }
    }
    // Validate before committing an import or schema upgrade.
    if version < migrations.len() {
        load(&tx)?;
    }
    tx.commit().map_err(|e| e.to_string())
}

fn open(dir: &Path) -> Result<Connection, String> {
    fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let mut db = Connection::open(dir.join("library.db")).map_err(|e| e.to_string())?;
    db.busy_timeout(Duration::from_secs(5))
        .map_err(|e| e.to_string())?;
    migrate(&mut db, dir, MIGRATIONS)?;
    Ok(db)
}

pub(super) fn read(dir: &Path) -> Result<History, String> {
    open(dir)
        .and_then(|db| load(&db))
        .map_err(|e| format!("library.db: {e}"))
}

pub(super) fn save(dir: &Path, history: &History) -> Result<(), String> {
    for (key, state) in history {
        validate(key, state)?;
    }
    let mut db = open(dir)?;
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|e| e.to_string())?;
    let previous = load(&tx)?;
    for key in previous.keys().filter(|key| !history.contains_key(*key)) {
        tx.execute("DELETE FROM media WHERE sha256=?1", [key])
            .map_err(|e| e.to_string())?;
    }
    for (key, state) in history {
        if previous.get(key) != Some(state) {
            put(&tx, key, state)?;
        }
    }
    tx.commit().map_err(|e| format!("Save library.db: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn imports_complete_media_settings() {
        let dir = tempfile::tempdir().unwrap();
        let key = "d".repeat(64);
        let state = MediaState {
            identity_version: 2,
            watched_at: "2026-10-14T18:42:00Z".into(),
            resume_position: Some(42.5),
            track_gains: std::collections::BTreeMap::from([(1, 0.4), (2, 0.0)]),
            file_settings: crate::chapters::FileSettings {
                chapters: crate::chapters::import("0:10 A chapter, with commas", 100.0, &[]).points,
                markers: vec![crate::chapters::Point {
                    id: 1,
                    time: 35.0,
                    title: "Bookmark".into(),
                }],
                embedded_disabled: true,
                ..Default::default()
            },
        };
        let mut writer = csv::Writer::from_writer(Vec::new());
        writer
            .serialize(super::super::WatchedRow {
                sha256: key.to_uppercase(),
                identity_version: state.identity_version,
                watched_at: state.watched_at.clone(),
                resume_position: state.resume_position,
                track_gains: serde_json::to_string(&state.track_gains).unwrap(),
                file_settings: serde_json::to_string(&state.file_settings).unwrap(),
            })
            .unwrap();
        let csv = writer.into_inner().unwrap();
        fs::write(dir.path().join("watched.csv"), &csv).unwrap();
        assert_eq!(read(dir.path()).unwrap(), History::from([(key, state)]));
        assert_eq!(fs::read(dir.path().join("watched.csv")).unwrap(), csv);
    }

    #[test]
    fn imports_once_and_preserves_csv_and_legacy_identity() {
        let dir = tempfile::tempdir().unwrap();
        let csv = format!(
            "sha256,watched_at\n{},2026-10-14T18:42:00Z\n",
            "a".repeat(64)
        );
        fs::write(dir.path().join("watched.csv"), &csv).unwrap();
        let history = read(dir.path()).unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[&"a".repeat(64)].identity_version, 1);
        assert_eq!(
            fs::read_to_string(dir.path().join("watched.csv")).unwrap(),
            csv
        );
        // Once imported, even a broken CSV cannot affect the database.
        fs::write(dir.path().join("watched.csv"), b"broken").unwrap();
        assert_eq!(read(dir.path()).unwrap(), history);
        save(dir.path(), &History::new()).unwrap();
        assert!(read(dir.path()).unwrap().is_empty());
    }

    #[test]
    fn invalid_import_rolls_back_and_can_be_retried() {
        let dir = tempfile::tempdir().unwrap();
        let csv = format!(
            "sha256,watched_at\n{},2026-10-14T18:42:00Z\nbroken,date\n",
            "a".repeat(64)
        );
        fs::write(dir.path().join("watched.csv"), &csv).unwrap();
        assert!(read(dir.path()).is_err());
        assert!(save(dir.path(), &History::new()).is_err());
        assert_eq!(
            fs::read_to_string(dir.path().join("watched.csv")).unwrap(),
            csv
        );
        let db = Connection::open(dir.path().join("library.db")).unwrap();
        assert_eq!(
            db.pragma_query_value(None, "user_version", |r| r.get::<_, u32>(0))
                .unwrap(),
            0
        );
        let count: i64 = db
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE type='table'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 0);
        fs::write(dir.path().join("watched.csv"), "sha256,watched_at\n").unwrap();
        assert!(read(dir.path()).unwrap().is_empty());
    }

    #[test]
    fn upgrades_skipped_versions_once_and_preserves_data() {
        let dir = tempfile::tempdir().unwrap();
        let history = History::from([(
            "b".repeat(64),
            MediaState {
                identity_version: 2,
                resume_position: Some(42.0),
                ..Default::default()
            },
        )]);
        save(dir.path(), &history).unwrap();
        let mut db = Connection::open(dir.path().join("library.db")).unwrap();
        let migrations = [
            MIGRATIONS[0],
            "ALTER TABLE media ADD COLUMN favorite INTEGER NOT NULL DEFAULT 0;",
            "CREATE INDEX media_watched_at ON media(watched_at);",
        ];
        migrate(&mut db, dir.path(), &migrations).unwrap();
        migrate(&mut db, dir.path(), &migrations).unwrap();
        assert_eq!(load(&db).unwrap(), history);
        assert_eq!(
            db.pragma_query_value(None, "user_version", |r| r.get::<_, u32>(0))
                .unwrap(),
            3
        );
        assert_eq!(
            db.query_row("SELECT favorite FROM media", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            0
        );
        // The older application must refuse this database.
        assert!(read(dir.path()).unwrap_err().contains("newer app"));
        assert!(save(dir.path(), &History::new()).is_err());
        assert_eq!(load(&db).unwrap(), history);
    }

    #[test]
    fn failed_upgrade_rolls_back_schema_and_version() {
        let dir = tempfile::tempdir().unwrap();
        read(dir.path()).unwrap();
        let mut db = Connection::open(dir.path().join("library.db")).unwrap();
        let migrations = [
            MIGRATIONS[0],
            "ALTER TABLE media ADD COLUMN favorite INTEGER DEFAULT 0;",
            "THIS IS NOT SQL;",
        ];
        assert!(migrate(&mut db, dir.path(), &migrations).is_err());
        assert_eq!(
            db.pragma_query_value(None, "user_version", |r| r.get::<_, u32>(0))
                .unwrap(),
            1
        );
        assert!(db.prepare("SELECT favorite FROM media").is_err());
        assert!(read(dir.path()).unwrap().is_empty());
    }

    #[test]
    fn malformed_or_unrecognized_database_is_not_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("library.db");
        fs::write(&path, b"broken").unwrap();
        assert!(read(dir.path()).is_err());
        assert!(save(dir.path(), &History::new()).is_err());
        assert_eq!(fs::read(&path).unwrap(), b"broken");
        let other = tempfile::tempdir().unwrap();
        let db = Connection::open(other.path().join("library.db")).unwrap();
        db.execute_batch("CREATE TABLE unrelated (value TEXT);")
            .unwrap();
        assert!(read(other.path()).is_err());
        assert!(db.prepare("SELECT * FROM unrelated").is_ok());
    }

    #[test]
    fn unchanged_records_are_not_rewritten() {
        let dir = tempfile::tempdir().unwrap();
        let key = "c".repeat(64);
        let mut history = History::from([(
            key.clone(),
            MediaState {
                identity_version: 2,
                ..Default::default()
            },
        )]);
        save(dir.path(), &history).unwrap();
        let db = Connection::open(dir.path().join("library.db")).unwrap();
        db.execute_batch("CREATE TRIGGER forbid_update BEFORE UPDATE ON media BEGIN SELECT RAISE(ABORT, 'unexpected update'); END;").unwrap();
        save(dir.path(), &history).unwrap();
        history.get_mut(&key).unwrap().resume_position = Some(30.0);
        assert!(save(dir.path(), &history).is_err());
        assert_eq!(read(dir.path()).unwrap()[&key].resume_position, None);
    }
}
