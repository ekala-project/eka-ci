// SQLite database generator for search indexes.
//
// Takes package, file, and option entries and writes them into a
// single SQLite database with FTS5 virtual tables for full-text
// search. The database is written to a temporary file and returned
// as bytes for upload.

use std::path::Path;

use anyhow::{Context, Result};
use rusqlite::Connection;
use tracing::info;

use super::super::types::{FileEntry, Manifest, OptionEntry, PackageEntry};

/// Build a SQLite database from the provided entries and return the
/// raw bytes of the `.db` file.
pub fn build_database(
    packages: &[PackageEntry],
    files: &[FileEntry],
    options: &[OptionEntry],
    manifest: &Manifest,
) -> Result<Vec<u8>> {
    let tmp =
        tempfile::NamedTempFile::new().context("failed to create temp file for search index db")?;
    let db_path = tmp.path();

    build_database_at(db_path, packages, files, options, manifest)?;

    let data = std::fs::read(db_path).context("failed to read generated search index database")?;

    info!(
        event = "search_db_built",
        packages = packages.len(),
        files = files.len(),
        options = options.len(),
        db_bytes = data.len(),
        "built search index SQLite database"
    );

    Ok(data)
}

/// Build the SQLite database at the given path.
fn build_database_at(
    path: &Path,
    packages: &[PackageEntry],
    files: &[FileEntry],
    options: &[OptionEntry],
    manifest: &Manifest,
) -> Result<()> {
    let conn = Connection::open(path)
        .with_context(|| format!("failed to open SQLite at {}", path.display()))?;

    // Performance pragmas for bulk insert.
    conn.execute_batch(
        "PRAGMA journal_mode = OFF;
         PRAGMA synchronous = OFF;
         PRAGMA locking_mode = EXCLUSIVE;
         PRAGMA page_size = 4096;",
    )
    .context("failed to set SQLite pragmas")?;

    create_schema(&conn)?;
    insert_packages(&conn, packages)?;
    insert_files(&conn, files)?;
    insert_options(&conn, options)?;
    insert_metadata(&conn, manifest)?;

    // Optimize FTS indexes after bulk insert.
    conn.execute_batch(
        "INSERT INTO packages_fts(packages_fts) VALUES('optimize');
         INSERT INTO files_fts(files_fts) VALUES('optimize');",
    )
    .context("failed to optimize FTS indexes")?;

    // Shrink the file.
    conn.execute_batch("VACUUM;")
        .context("failed to VACUUM database")?;

    Ok(())
}

/// Create all tables, indexes, and FTS virtual tables.
fn create_schema(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE packages (
            attr         TEXT PRIMARY KEY,
            pname        TEXT NOT NULL,
            version      TEXT NOT NULL,
            description  TEXT,
            outputs      TEXT,
            main_program TEXT
        );

        CREATE TABLE files (
            file    TEXT NOT NULL,
            package TEXT NOT NULL,
            output  TEXT NOT NULL
        );

        CREATE INDEX idx_files_file    ON files(file);
        CREATE INDEX idx_files_package ON files(package);

        CREATE TABLE options (
            name         TEXT PRIMARY KEY,
            description  TEXT,
            type         TEXT,
            default_val  TEXT,
            example      TEXT,
            declarations TEXT,
            read_only    INTEGER NOT NULL DEFAULT 0
        );

        CREATE TABLE metadata (
            key   TEXT PRIMARY KEY,
            value TEXT NOT NULL
        );

        CREATE VIRTUAL TABLE packages_fts USING fts5(
            attr, pname, description,
            content='packages',
            content_rowid='rowid'
        );

        CREATE VIRTUAL TABLE files_fts USING fts5(
            file, package,
            content='files',
            content_rowid='rowid'
        );",
    )
    .context("failed to create search index schema")?;

    Ok(())
}

/// Bulk-insert package entries.
fn insert_packages(conn: &Connection, packages: &[PackageEntry]) -> Result<()> {
    let mut stmt = conn
        .prepare(
            "INSERT INTO packages (attr, pname, version, description, outputs, main_program)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        )
        .context("failed to prepare packages insert")?;

    let mut fts_stmt = conn
        .prepare(
            "INSERT INTO packages_fts (rowid, attr, pname, description)
             VALUES (?1, ?2, ?3, ?4)",
        )
        .context("failed to prepare packages_fts insert")?;

    for pkg in packages {
        let outputs = if pkg.outputs.is_empty() {
            None
        } else {
            Some(serde_json::to_string(&pkg.outputs).unwrap_or_default())
        };

        let rowid = stmt
            .insert(rusqlite::params![
                pkg.attr,
                pkg.pname,
                pkg.version,
                if pkg.description.is_empty() {
                    None
                } else {
                    Some(&pkg.description)
                },
                outputs,
                pkg.main_program,
            ])
            .with_context(|| format!("failed to insert package: {}", pkg.attr))?;

        fts_stmt
            .execute(rusqlite::params![
                rowid,
                pkg.attr,
                pkg.pname,
                pkg.description,
            ])
            .with_context(|| format!("failed to insert package into FTS index: {}", pkg.attr))?;
    }

    Ok(())
}

/// Bulk-insert file entries.
fn insert_files(conn: &Connection, files: &[FileEntry]) -> Result<()> {
    let mut stmt = conn
        .prepare("INSERT INTO files (file, package, output) VALUES (?1, ?2, ?3)")
        .context("failed to prepare files insert")?;

    let mut fts_stmt = conn
        .prepare("INSERT INTO files_fts (rowid, file, package) VALUES (?1, ?2, ?3)")
        .context("failed to prepare files_fts insert")?;

    for f in files {
        let rowid = stmt
            .insert(rusqlite::params![f.file, f.package, f.output])
            .context("failed to insert file entry")?;

        fts_stmt
            .execute(rusqlite::params![rowid, f.file, f.package])
            .context("failed to insert file into FTS index")?;
    }

    Ok(())
}

/// Bulk-insert option entries.
fn insert_options(conn: &Connection, options: &[OptionEntry]) -> Result<()> {
    if options.is_empty() {
        return Ok(());
    }

    let mut stmt = conn
        .prepare(
            "INSERT INTO options (name, description, type, default_val, example, declarations, \
             read_only)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        )
        .context("failed to prepare options insert")?;

    for opt in options {
        let declarations = if opt.declarations.is_empty() {
            None
        } else {
            Some(serde_json::to_string(&opt.declarations).unwrap_or_default())
        };

        stmt.execute(rusqlite::params![
            opt.name,
            if opt.description.is_empty() {
                None
            } else {
                Some(&opt.description)
            },
            if opt.type_name.is_empty() {
                None
            } else {
                Some(&opt.type_name)
            },
            opt.default,
            opt.example,
            declarations,
            opt.read_only as i32,
        ])
        .with_context(|| format!("failed to insert option: {}", opt.name))?;
    }

    Ok(())
}

/// Store manifest metadata as key-value pairs.
fn insert_metadata(conn: &Connection, manifest: &Manifest) -> Result<()> {
    let mut stmt = conn
        .prepare("INSERT INTO metadata (key, value) VALUES (?1, ?2)")
        .context("failed to prepare metadata insert")?;

    stmt.execute(rusqlite::params!["generated_at", manifest.generated_at])?;
    stmt.execute(rusqlite::params!["channel_name", manifest.channel_name])?;
    stmt.execute(rusqlite::params!["nixpkgs_rev", manifest.nixpkgs_rev])?;
    stmt.execute(rusqlite::params![
        "package_count",
        manifest.package_count.to_string()
    ])?;
    stmt.execute(rusqlite::params![
        "file_count",
        manifest.file_count.to_string()
    ])?;
    stmt.execute(rusqlite::params![
        "option_count",
        manifest.option_count.to_string()
    ])?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_database_roundtrip() {
        let packages = vec![
            PackageEntry {
                attr: "hello".to_string(),
                pname: "hello".to_string(),
                version: "2.12.1".to_string(),
                description: "A greeting".to_string(),
                outputs: vec!["out".to_string()],
                main_program: Some("hello".to_string()),
            },
            PackageEntry {
                attr: "bat".to_string(),
                pname: "bat".to_string(),
                version: "0.24.0".to_string(),
                description: "cat clone with wings".to_string(),
                outputs: Vec::new(),
                main_program: None,
            },
        ];

        let files = vec![
            FileEntry {
                file: "bin/hello".to_string(),
                package: "hello".to_string(),
                output: "out".to_string(),
            },
            FileEntry {
                file: "bin/bat".to_string(),
                package: "bat".to_string(),
                output: "out".to_string(),
            },
        ];

        let manifest = Manifest {
            generated_at: "2026-09-27T00:00:00Z".to_string(),
            channel_name: "unstable".to_string(),
            nixpkgs_rev: "abc123".to_string(),
            package_count: packages.len(),
            file_count: files.len(),
            option_count: 0,
        };

        let data = build_database(&packages, &files, &[], &manifest).unwrap();
        assert!(!data.is_empty());

        // Verify the database is valid by opening it.
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), &data).unwrap();
        let conn = Connection::open(tmp.path()).unwrap();

        // Check package count.
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM packages", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 2);

        // Check FTS search works.
        let found: String = conn
            .query_row(
                "SELECT attr FROM packages_fts WHERE packages_fts MATCH 'greeting'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(found, "hello");

        // Check files.
        let file_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0))
            .unwrap();
        assert_eq!(file_count, 2);

        // Check files FTS.
        let found_pkg: String = conn
            .query_row(
                "SELECT package FROM files_fts WHERE files_fts MATCH 'bat'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(found_pkg, "bat");

        // Check metadata.
        let rev: String = conn
            .query_row(
                "SELECT value FROM metadata WHERE key = 'nixpkgs_rev'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(rev, "abc123");
    }
}
