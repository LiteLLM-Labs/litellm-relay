use std::{path::Path, sync::Mutex};

use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use serde_json::Value;
use uuid::Uuid;

use super::protocol::{digest, token, Failure, Outcome, Result};

pub struct Store {
    connection: Mutex<Connection>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Identity {
    pub subject: String,
    pub label: String,
    pub method: &'static str,
    pub expires_at: i64,
}

#[derive(Clone, Debug)]
pub struct Device {
    pub id: Uuid,
    pub subject: String,
    pub name: String,
    pub last_seen: i64,
}

pub enum RequestState {
    New,
    Cached(Outcome),
    Unconfirmed,
}

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        if path != Path::new(":memory:") {
            let parent = path
                .parent()
                .filter(|path| !path.as_os_str().is_empty())
                .unwrap_or(Path::new("."));
            std::fs::create_dir_all(parent)
                .map_err(|_| Failure::new(503, "Cannot create the remote state directory."))?;
            if !path.exists() {
                crate::system::create_private(path)
                    .map_err(|_| Failure::new(503, "Cannot create the remote state file."))?;
            }
        }
        let connection =
            Connection::open(path).map_err(|_| Failure::new(503, "Cannot open remote state."))?;
        connection.execute_batch("PRAGMA journal_mode=WAL;
            CREATE TABLE IF NOT EXISTS sessions (token_hash TEXT PRIMARY KEY, subject TEXT NOT NULL, label TEXT NOT NULL, expires_at INTEGER NOT NULL);
            CREATE TABLE IF NOT EXISTS catalogs (token_hash TEXT PRIMARY KEY, models TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS devices (id TEXT PRIMARY KEY, subject TEXT NOT NULL, name TEXT NOT NULL, token_hash TEXT NOT NULL UNIQUE, last_seen INTEGER NOT NULL);
            CREATE INDEX IF NOT EXISTS devices_subject ON devices(subject);
            CREATE TABLE IF NOT EXISTS requests (subject TEXT NOT NULL, request_id TEXT NOT NULL, fingerprint TEXT NOT NULL, response TEXT, expires_at INTEGER NOT NULL, PRIMARY KEY(subject, request_id));")
            .map_err(|_| Failure::new(503, "Cannot initialize remote state."))?;
        Ok(Self {
            connection: Mutex::new(connection),
        })
    }

    fn db<T>(&self, operation: impl FnOnce(&mut Connection) -> rusqlite::Result<T>) -> Result<T> {
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| Failure::new(503, "Remote state is unavailable."))?;
        operation(&mut connection)
            .map_err(|_| Failure::new(503, "The remote state operation failed."))
    }

    pub fn issue(&self, subject: &str, models: &[String], now: i64) -> Result<(String, Identity)> {
        let token = token();
        let identity = Identity {
            subject: subject.into(),
            label: subject.into(),
            method: "sso",
            expires_at: now + 7 * 86_400_000,
        };
        let catalog = serde_json::to_string(models)
            .map_err(|_| Failure::new(500, "Cannot save the model catalog."))?;
        self.db(|connection| {
            let transaction = connection.transaction()?;
            transaction.execute("DELETE FROM sessions WHERE expires_at <= ?", [now])?;
            transaction.execute(
                "DELETE FROM catalogs WHERE token_hash NOT IN (SELECT token_hash FROM sessions)",
                [],
            )?;
            transaction.execute(
                "INSERT INTO sessions VALUES (?, ?, ?, ?)",
                params![digest(&token), subject, subject, identity.expires_at],
            )?;
            transaction.execute(
                "INSERT INTO catalogs VALUES (?, ?)",
                params![digest(&token), catalog],
            )?;
            transaction.commit()
        })?;
        Ok((token, identity))
    }

    pub fn identity(&self, token: &str, now: i64) -> Result<Option<Identity>> {
        self.db(|connection| connection.query_row("SELECT subject, label, expires_at FROM sessions WHERE token_hash = ? AND expires_at > ?",
            params![digest(token), now], |row| Ok(Identity { subject: row.get(0)?, label: row.get(1)?, method: "sso", expires_at: row.get(2)? })).optional())
    }

    pub fn models(&self, token: &str) -> Result<Vec<String>> {
        let catalog: Option<String> = self.db(|connection| {
            connection
                .query_row(
                    "SELECT models FROM catalogs WHERE token_hash = ?",
                    [digest(token)],
                    |row| row.get(0),
                )
                .optional()
        })?;
        catalog
            .map(|value| {
                serde_json::from_str(&value)
                    .map_err(|_| Failure::new(503, "The saved model catalog is unreadable."))
            })
            .unwrap_or(Ok(Vec::new()))
    }

    pub fn logout(&self, token: &str) -> Result<()> {
        self.db(|connection| {
            let transaction = connection.transaction()?;
            transaction.execute("DELETE FROM sessions WHERE token_hash = ?", [digest(token)])?;
            transaction.execute("DELETE FROM catalogs WHERE token_hash = ?", [digest(token)])?;
            transaction.commit()
        })
    }

    pub fn enroll(&self, subject: &str, name: &str, now: i64) -> Result<(Device, String)> {
        let device = Device {
            id: Uuid::new_v4(),
            subject: subject.into(),
            name: name.into(),
            last_seen: now,
        };
        let token = token();
        self.db(|connection| {
            connection.execute(
                "INSERT INTO devices VALUES (?, ?, ?, ?, ?)",
                params![device.id.to_string(), subject, name, digest(&token), now],
            )
        })?;
        Ok((device, token))
    }

    pub fn devices(&self, subject: &str) -> Result<Vec<Device>> {
        self.db(|connection| {
            let mut statement = connection.prepare(
                "SELECT id, subject, name, last_seen FROM devices WHERE subject = ? ORDER BY name",
            )?;
            let rows = statement.query_map([subject], device_row)?;
            rows.collect()
        })
    }

    pub fn authenticate_device(&self, token: &str) -> Result<Option<Device>> {
        self.db(|connection| {
            connection
                .query_row(
                    "SELECT id, subject, name, last_seen FROM devices WHERE token_hash = ?",
                    [digest(token)],
                    device_row,
                )
                .optional()
        })
    }

    pub fn device(&self, id: Uuid, subject: &str) -> Result<Device> {
        self.devices(subject)?
            .into_iter()
            .find(|device| device.id == id)
            .ok_or_else(|| Failure::new(404, "Device not found."))
    }

    pub fn seen(&self, id: Uuid, now: i64) -> Result<()> {
        self.db(|connection| {
            connection.execute(
                "UPDATE devices SET last_seen = ? WHERE id = ?",
                params![now, id.to_string()],
            )
        })
        .map(|_| ())
    }

    pub fn revoke(&self, id: Uuid, subject: &str) -> Result<()> {
        self.db(|connection| {
            connection.execute(
                "DELETE FROM devices WHERE id = ? AND subject = ?",
                params![id.to_string(), subject],
            )
        })
        .map(|_| ())
    }

    pub fn begin(
        &self,
        subject: &str,
        request: Uuid,
        fingerprint: &str,
        now: i64,
    ) -> Result<RequestState> {
        let previous: Option<(String, Option<String>)> = self.db(|connection| {
            let transaction = connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            transaction.execute("DELETE FROM requests WHERE expires_at <= ?", [now])?;
            let previous = transaction.query_row("SELECT fingerprint, response FROM requests WHERE subject = ? AND request_id = ?",
                params![subject, request.to_string()], |row| Ok((row.get(0)?, row.get(1)?))).optional()?;
            if previous.is_none() {
                transaction.execute("INSERT INTO requests VALUES (?, ?, ?, NULL, ?)", params![subject, request.to_string(), fingerprint, now + 86_400_000])?;
            }
            transaction.commit()?;
            Ok(previous)
        })?;
        match previous {
            None => Ok(RequestState::New),
            Some((before, _)) if before != fingerprint => {
                Err(Failure::new(409, "This request ID was already used."))
            }
            Some((_, None)) => Ok(RequestState::Unconfirmed),
            Some((_, Some(value))) => serde_json::from_str(&value)
                .map(RequestState::Cached)
                .map_err(|_| Failure::new(503, "The saved request outcome is unreadable.")),
        }
    }

    pub fn finish(&self, subject: &str, request: Uuid, result: Result<Value>) -> Result<()> {
        let result = serde_json::to_string(&Outcome::from_result(result))
            .map_err(|_| Failure::new(500, "Cannot save the request outcome."))?;
        self.db(|connection| {
            connection.execute(
                "UPDATE requests SET response = ? WHERE subject = ? AND request_id = ?",
                params![result, subject, request.to_string()],
            )
        })
        .map(|_| ())
    }
}

fn device_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Device> {
    let id: String = row.get(0)?;
    let id = Uuid::parse_str(&id).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(error))
    })?;
    Ok(Device {
        id,
        subject: row.get(1)?,
        name: row.get(2)?,
        last_seen: row.get(3)?,
    })
}
