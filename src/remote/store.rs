use std::{path::Path, sync::Mutex};

use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde::Serialize;
use serde_json::Value;
use uuid::Uuid;

use super::protocol::{digest, token, Failure, Outcome, Platform, Result};

pub const ACCESS_TTL_MS: i64 = 15 * 60 * 1000;
pub const REFRESH_ABSOLUTE_MS: i64 = 30 * 86_400_000;
pub const REFRESH_IDLE_MS: i64 = 7 * 86_400_000;

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
    pub platform: Platform,
}

pub enum RequestState {
    New,
    Cached(Outcome),
    Unconfirmed,
}

pub struct Issued {
    pub access_token: String,
    pub refresh_token: String,
    pub identity: Identity,
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
        let mut connection =
            Connection::open(path).map_err(|_| Failure::new(503, "Cannot open remote state."))?;
        connection.execute_batch("PRAGMA journal_mode=WAL;
            CREATE TABLE IF NOT EXISTS sessions (token_hash TEXT PRIMARY KEY, subject TEXT NOT NULL, label TEXT NOT NULL, expires_at INTEGER NOT NULL);
            CREATE TABLE IF NOT EXISTS catalogs (token_hash TEXT PRIMARY KEY, models TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS devices (id TEXT PRIMARY KEY, subject TEXT NOT NULL, name TEXT NOT NULL, token_hash TEXT NOT NULL UNIQUE, last_seen INTEGER NOT NULL, platform TEXT NOT NULL DEFAULT 'macOS');
            CREATE INDEX IF NOT EXISTS devices_subject ON devices(subject);
            CREATE TABLE IF NOT EXISTS requests (subject TEXT NOT NULL, request_id TEXT NOT NULL, fingerprint TEXT NOT NULL, response TEXT, expires_at INTEGER NOT NULL, PRIMARY KEY(subject, request_id));
            CREATE TABLE IF NOT EXISTS session_family (token_hash TEXT PRIMARY KEY, family TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS family_catalogs (family TEXT PRIMARY KEY, models TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS refresh_tokens (token_hash TEXT PRIMARY KEY, family TEXT NOT NULL, subject TEXT NOT NULL, absolute_expires_at INTEGER NOT NULL, idle_expires_at INTEGER NOT NULL, replaced INTEGER NOT NULL);
            CREATE INDEX IF NOT EXISTS refresh_family ON refresh_tokens(family);
            CREATE TABLE IF NOT EXISTS device_access (token_hash TEXT PRIMARY KEY, device_id TEXT NOT NULL, expires_at INTEGER NOT NULL);
            CREATE INDEX IF NOT EXISTS device_access_device ON device_access(device_id);")
            .map_err(|_| Failure::new(503, "Cannot initialize remote state."))?;
        Self::migrate_devices(&mut connection)
            .map_err(|_| Failure::new(503, "Cannot migrate remote device state."))?;
        Ok(Self {
            connection: Mutex::new(connection),
        })
    }

    fn migrate_devices(connection: &mut Connection) -> rusqlite::Result<()> {
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let has_platform: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM pragma_table_info('devices') WHERE name = 'platform')",
            [],
            |row| row.get(0),
        )?;
        if !has_platform {
            transaction.execute_batch(
                "ALTER TABLE devices ADD COLUMN platform TEXT NOT NULL DEFAULT 'macOS';",
            )?;
        }
        transaction.commit()
    }

    fn db<T>(&self, operation: impl FnOnce(&mut Connection) -> rusqlite::Result<T>) -> Result<T> {
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| Failure::new(503, "Remote state is unavailable."))?;
        operation(&mut connection)
            .map_err(|_| Failure::new(503, "The remote state operation failed."))
    }

    pub fn issue(&self, subject: &str, models: &[String], now: i64) -> Result<Issued> {
        let access_token = token();
        let refresh_token = token();
        let identity = Identity {
            subject: subject.into(),
            label: subject.into(),
            method: "sso",
            expires_at: now + ACCESS_TTL_MS,
        };
        let family = Uuid::new_v4().to_string();
        let catalog = serde_json::to_string(models)
            .map_err(|_| Failure::new(500, "Cannot save the model catalog."))?;
        let access_hash = digest(&access_token);
        self.db(|connection| {
            let transaction = connection.transaction()?;
            purge(&transaction, now)?;
            transaction.execute(
                "INSERT INTO sessions VALUES (?, ?, ?, ?)",
                params![access_hash, subject, subject, identity.expires_at],
            )?;
            transaction.execute(
                "INSERT INTO session_family VALUES (?, ?)",
                params![access_hash, family],
            )?;
            transaction.execute(
                "INSERT INTO family_catalogs VALUES (?, ?)",
                params![family, catalog],
            )?;
            transaction.execute(
                "INSERT INTO refresh_tokens VALUES (?, ?, ?, ?, ?, 0)",
                params![
                    digest(&refresh_token),
                    family,
                    subject,
                    now + REFRESH_ABSOLUTE_MS,
                    now + REFRESH_IDLE_MS
                ],
            )?;
            transaction.commit()
        })?;
        Ok(Issued {
            access_token,
            refresh_token,
            identity,
        })
    }

    pub fn refresh(&self, presented: &str, now: i64) -> Result<Issued> {
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| Failure::new(503, "Remote state is unavailable."))?;
        let transaction = connection
            .transaction()
            .map_err(|_| Failure::new(503, "The remote state operation failed."))?;
        purge(&transaction, now)
            .map_err(|_| Failure::new(503, "The remote state operation failed."))?;
        let current: Option<(String, String, i64, i64, i64)> = transaction
            .query_row(
                "SELECT family, subject, absolute_expires_at, idle_expires_at, replaced FROM refresh_tokens WHERE token_hash = ?",
                [digest(presented)],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
            )
            .optional()
            .map_err(|_| Failure::new(503, "The remote state operation failed."))?;
        let Some((family, subject, absolute, idle, replaced)) = current else {
            return Err(Failure::new(401, "Sign in again."));
        };
        if replaced != 0 {
            revoke_family(&transaction, &family)
                .map_err(|_| Failure::new(503, "The remote state operation failed."))?;
            transaction
                .commit()
                .map_err(|_| Failure::new(503, "The remote state operation failed."))?;
            return Err(Failure::new(401, "Sign in again."));
        }
        if absolute <= now || idle <= now {
            transaction
                .execute(
                    "DELETE FROM refresh_tokens WHERE token_hash = ?",
                    [digest(presented)],
                )
                .map_err(|_| Failure::new(503, "The remote state operation failed."))?;
            transaction
                .commit()
                .map_err(|_| Failure::new(503, "The remote state operation failed."))?;
            return Err(Failure::new(401, "Sign in again."));
        }
        let access_token = token();
        let refresh_token = token();
        let identity = Identity {
            subject: subject.clone(),
            label: subject.clone(),
            method: "sso",
            expires_at: now + ACCESS_TTL_MS,
        };
        let access_hash = digest(&access_token);
        transaction
            .execute(
                "UPDATE refresh_tokens SET replaced = 1 WHERE token_hash = ?",
                [digest(presented)],
            )
            .map_err(|_| Failure::new(503, "The remote state operation failed."))?;
        transaction
            .execute(
                "INSERT INTO sessions VALUES (?, ?, ?, ?)",
                params![access_hash, subject, subject, identity.expires_at],
            )
            .map_err(|_| Failure::new(503, "The remote state operation failed."))?;
        transaction
            .execute(
                "INSERT INTO session_family VALUES (?, ?)",
                params![access_hash, family],
            )
            .map_err(|_| Failure::new(503, "The remote state operation failed."))?;
        transaction
            .execute(
                "INSERT INTO refresh_tokens VALUES (?, ?, ?, ?, ?, 0)",
                params![
                    digest(&refresh_token),
                    family,
                    subject,
                    absolute,
                    (now + REFRESH_IDLE_MS).min(absolute)
                ],
            )
            .map_err(|_| Failure::new(503, "The remote state operation failed."))?;
        transaction
            .commit()
            .map_err(|_| Failure::new(503, "The remote state operation failed."))?;
        Ok(Issued {
            access_token,
            refresh_token,
            identity,
        })
    }

    pub fn identity(&self, token: &str, now: i64) -> Result<Option<Identity>> {
        self.db(|connection| {
            let transaction = connection.transaction()?;
            purge(&transaction, now)?;
            let identity = transaction.query_row("SELECT subject, label, expires_at FROM sessions WHERE token_hash = ? AND expires_at > ?",
                params![digest(token), now], |row| Ok(Identity { subject: row.get(0)?, label: row.get(1)?, method: "sso", expires_at: row.get(2)? })).optional()?;
            transaction.commit()?;
            Ok(identity)
        })
    }

    pub fn models(&self, token: &str) -> Result<Vec<String>> {
        let hash = digest(token);
        let catalog: Option<String> = self.db(|connection| {
            connection
                .query_row(
                    "SELECT models FROM family_catalogs WHERE family = (SELECT family FROM session_family WHERE token_hash = ?)",
                    [&hash],
                    |row| row.get(0),
                )
                .optional()
                .and_then(|found| match found {
                    Some(models) => Ok(Some(models)),
                    None => connection
                        .query_row(
                            "SELECT models FROM catalogs WHERE token_hash = ?",
                            [&hash],
                            |row| row.get(0),
                        )
                        .optional(),
                })
        })?;
        catalog
            .map(|value| {
                serde_json::from_str(&value)
                    .map_err(|_| Failure::new(503, "The saved model catalog is unreadable."))
            })
            .unwrap_or(Ok(Vec::new()))
    }

    pub fn logout(&self, token: &str) -> Result<()> {
        let hash = digest(token);
        self.db(|connection| {
            let transaction = connection.transaction()?;
            let family: Option<String> = transaction
                .query_row(
                    "SELECT family FROM session_family WHERE token_hash = ?",
                    [&hash],
                    |row| row.get(0),
                )
                .optional()?;
            if let Some(family) = family {
                revoke_family(&transaction, &family)?;
            } else {
                transaction.execute("DELETE FROM sessions WHERE token_hash = ?", [&hash])?;
                transaction.execute("DELETE FROM catalogs WHERE token_hash = ?", [&hash])?;
            }
            transaction.commit()
        })
    }

    pub fn enroll(
        &self,
        subject: &str,
        name: &str,
        platform: Platform,
        now: i64,
    ) -> Result<(Device, String)> {
        let device = Device {
            id: Uuid::new_v4(),
            subject: subject.into(),
            name: name.into(),
            last_seen: now,
            platform,
        };
        let token = token();
        self.db(|connection| {
            connection.execute(
                "INSERT INTO devices (id, subject, name, token_hash, last_seen, platform) VALUES (?, ?, ?, ?, ?, ?)",
                params![device.id.to_string(), subject, name, digest(&token), now, platform.label()],
            )
        })?;
        Ok((device, token))
    }

    pub fn devices(&self, subject: &str) -> Result<Vec<Device>> {
        self.db(|connection| {
            let mut statement = connection.prepare(
                "SELECT id, subject, name, last_seen, platform FROM devices WHERE subject = ? ORDER BY name",
            )?;
            let rows = statement.query_map([subject], device_row)?;
            rows.collect()
        })
    }

    pub fn authenticate_device(&self, token: &str) -> Result<Option<Device>> {
        self.db(|connection| {
            connection
                .query_row(
                    "SELECT id, subject, name, last_seen, platform FROM devices WHERE token_hash = ?",
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

    pub fn exchange_device(&self, device_token: &str, now: i64) -> Result<(String, i64)> {
        let Some(device) = self.authenticate_device(device_token)? else {
            return Err(Failure::new(401, "Device authentication failed."));
        };
        let access = token();
        let expires = now + ACCESS_TTL_MS;
        self.db(|connection| {
            let transaction = connection.transaction()?;
            purge(&transaction, now)?;
            transaction.execute(
                "INSERT INTO device_access VALUES (?, ?, ?)",
                params![digest(&access), device.id.to_string(), expires],
            )?;
            transaction.commit()
        })?;
        Ok((access, expires))
    }

    pub fn authenticate_access(&self, token: &str, now: i64) -> Result<Option<(Device, i64)>> {
        self.db(|connection| {
            let transaction = connection.transaction()?;
            purge(&transaction, now)?;
            let device = transaction
                .query_row(
                    "SELECT devices.id, devices.subject, devices.name, devices.last_seen, devices.platform, device_access.expires_at FROM device_access JOIN devices ON devices.id = device_access.device_id WHERE device_access.token_hash = ? AND device_access.expires_at > ?",
                    params![digest(token), now],
                    |row| Ok((device_row(row)?, row.get(5)?)),
                )
                .optional()?;
            transaction.commit()?;
            Ok(device)
        })
    }

    pub fn revoke(&self, id: Uuid, subject: &str) -> Result<()> {
        self.db(|connection| {
            let transaction = connection.transaction()?;
            transaction.execute(
                "DELETE FROM device_access WHERE device_id = ?",
                [id.to_string()],
            )?;
            transaction.execute(
                "DELETE FROM devices WHERE id = ? AND subject = ?",
                params![id.to_string(), subject],
            )?;
            transaction.commit()
        })
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

    pub fn cancel_pending(&self, subject: &str, request: Uuid) -> Result<()> {
        self.db(|connection| {
            connection.execute(
                "DELETE FROM requests WHERE subject = ? AND request_id = ? AND response IS NULL",
                params![subject, request.to_string()],
            )
        })
        .map(|_| ())
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

fn purge(connection: &rusqlite::Connection, now: i64) -> rusqlite::Result<()> {
    connection.execute("DELETE FROM sessions WHERE expires_at <= ?", [now])?;
    connection.execute(
        "DELETE FROM catalogs WHERE token_hash NOT IN (SELECT token_hash FROM sessions)",
        [],
    )?;
    connection.execute("DELETE FROM device_access WHERE expires_at <= ?", [now])?;
    connection.execute(
        "DELETE FROM refresh_tokens WHERE absolute_expires_at <= ? OR (replaced = 0 AND idle_expires_at <= ?)",
        [now, now],
    )?;
    connection.execute(
        "DELETE FROM session_family WHERE token_hash NOT IN (SELECT token_hash FROM sessions)",
        [],
    )?;
    connection.execute(
        "DELETE FROM family_catalogs WHERE family NOT IN (SELECT family FROM refresh_tokens UNION SELECT family FROM session_family)",
        [],
    )?;
    Ok(())
}

fn revoke_family(connection: &rusqlite::Connection, family: &str) -> rusqlite::Result<()> {
    connection.execute(
        "DELETE FROM sessions WHERE token_hash IN (SELECT token_hash FROM session_family WHERE family = ?)",
        [family],
    )?;
    connection.execute("DELETE FROM session_family WHERE family = ?", [family])?;
    connection.execute("DELETE FROM refresh_tokens WHERE family = ?", [family])?;
    connection.execute("DELETE FROM family_catalogs WHERE family = ?", [family])?;
    Ok(())
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
        platform: Platform::from(row.get::<_, String>(4)?.as_str()),
    })
}
