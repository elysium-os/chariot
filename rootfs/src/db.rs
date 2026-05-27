use std::{collections::BTreeSet, path::Path, time::Duration};

use rusqlite::{Connection, ToSql, params, types::ToSqlOutput};

use crate::pkgset::PkgSetState;

pub struct Database(Connection);

impl ToSql for PkgSetState {
    fn to_sql(&self) -> Result<ToSqlOutput<'_>, rusqlite::Error> {
        Ok(ToSqlOutput::from(match self {
            Self::Unknown => 0,
            Self::Cached => 1,
            Self::Deduplicated => 2,
        }))
    }
}

impl From<i64> for PkgSetState {
    fn from(value: i64) -> Self {
        match value {
            2 => Self::Deduplicated,
            1 => Self::Cached,
            0 | _ => Self::Unknown,
        }
    }
}

impl Database {
    pub fn connect(path: impl AsRef<Path>) -> Result<Self, rusqlite::Error> {
        let conn = Connection::open(path)?;

        conn.busy_timeout(Duration::from_secs(5))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;

        conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS package_set (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                state INTEGER NOT NULL,
                size INTEGER NOT NULL DEFAULT 0
            ) STRICT;

            CREATE TABLE IF NOT EXISTS package_set_entry (
                set_id INTEGER NOT NULL,
                package TEXT NOT NULL,
                PRIMARY KEY(set_id, package),
                FOREIGN KEY(set_id) REFERENCES package_set(id) ON DELETE CASCADE
            ) STRICT;
            ",
        )?;

        Ok(Database(conn))
    }

    pub fn get_pkgset_id(&self, pkgset: &BTreeSet<&str>) -> Result<i64, rusqlite::Error> {
        let length = pkgset.len();
        let lookup = if length == 0 {
            None
        } else {
            Some(pkgset.iter().copied().collect::<Vec<&str>>().join(","))
        };

        let tx = self.0.unchecked_transaction()?;

        let result = tx.query_one(
            "
            SELECT pkgset.id
            FROM package_set pkgset
            WHERE (
                SELECT GROUP_CONCAT(package, ',' ORDER BY package)
                FROM package_set_entry
                WHERE set_id = pkgset.id
            ) IS ?
            AND (
                SELECT COUNT(*)
                FROM package_set_entry
                WHERE set_id = pkgset.id
            ) = ?
            ",
            params![lookup, length as i64],
            |row| row.get::<usize, i64>(0),
        );

        let id = match result {
            Ok(id) => id,
            Err(rusqlite::Error::QueryReturnedNoRows) => {
                let state = PkgSetState::Unknown;
                let id = tx.query_one("INSERT INTO package_set (state) VALUES (?) RETURNING id", params![state], |row| {
                    row.get::<usize, i64>(0)
                })?;

                for pkg in pkgset {
                    tx.execute("INSERT INTO package_set_entry (set_id, package) VALUES (?, ?)", params![id, pkg])?;
                }

                id
            }
            Err(err) => return Err(err.into()),
        };

        tx.commit()?;

        Ok(id)
    }

    pub fn get_pkgset(&self, pkgset_id: i64) -> Result<(PkgSetState, u64), rusqlite::Error> {
        let result = self
            .0
            .query_one("SELECT state, size FROM package_set WHERE id = ?", params![pkgset_id], |row| {
                Ok((PkgSetState::from(row.get::<usize, i64>(0)?), row.get::<usize, i64>(1)? as u64))
            })?;
        Ok(result)
    }

    pub fn update_pkgset(&self, id: i64, state: &PkgSetState, size: Option<u64>) -> Result<(), rusqlite::Error> {
        let rows_changed = match size {
            None => self.0.execute("UPDATE package_set SET state = ? WHERE id = ?", params![state, id])?,
            Some(size) => self
                .0
                .execute("UPDATE package_set SET state = ?, size = ? WHERE id = ?", params![state, size as i64, id])?,
        };

        assert!(rows_changed == 1);

        Ok(())
    }
}
