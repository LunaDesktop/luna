//! Warstwa logind — lista sesji i przełączanie użytkowników.
//!
//! UI jest frontendem loginda: czytamy `Manager.ListSessions` i właściwości sesji,
//! a przełączanie/blokadę wykonujemy przez `ActivateSession`/`Session.Lock`.

use std::collections::HashMap;

use zbus::blocking::Connection;
use zbus::zvariant::{OwnedObjectPath, OwnedValue};

const LOGIND_DEST: &str = "org.freedesktop.login1";
const LOGIND_PATH: &str = "/org/freedesktop/login1";
const MANAGER_IFACE: &str = "org.freedesktop.login1.Manager";
const SESSION_IFACE: &str = "org.freedesktop.login1.Session";
const PROPS: &str = "org.freedesktop.DBus.Properties";

/// Sesja logind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Session {
    pub id: String,
    pub user: String,
    pub uid: u32,
    pub seat: String,
    /// `x11`/`wayland`/`tty` (logind `Session.Type`).
    pub kind: String,
    /// logind `Session.Active`.
    pub active: bool,
    /// Bieżąca sesja (porównanie z `XDG_SESSION_ID`).
    pub current: bool,
}

/// Migawka sesji logind.
#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    pub available: bool,
    pub sessions: Vec<Session>,
    pub error: Option<String>,
}

/// Surowy wiersz z `ListSessions` (ze ścieżką obiektu).
struct RawSession {
    id: String,
    uid: u32,
    user: String,
    seat: String,
    path: String,
}

fn system() -> Result<Connection, String> {
    Connection::system().map_err(|err| format!("D-Bus (system): {err}"))
}

fn all_props(conn: &Connection, path: &str, iface: &str) -> HashMap<String, OwnedValue> {
    conn.call_method(Some(LOGIND_DEST), path, Some(PROPS), "GetAll", &iface)
        .ok()
        .and_then(|reply| {
            reply
                .body()
                .deserialize::<HashMap<String, OwnedValue>>()
                .ok()
        })
        .unwrap_or_default()
}

fn as_string(props: &HashMap<String, OwnedValue>, key: &str) -> Option<String> {
    props
        .get(key)
        .and_then(|v| String::try_from(v.clone()).ok())
}

fn as_bool(props: &HashMap<String, OwnedValue>, key: &str) -> Option<bool> {
    props.get(key).and_then(|v| bool::try_from(v.clone()).ok())
}

/// Ścieżka obiektu sesji logind.
fn session_path(id: &str) -> String {
    format!("{LOGIND_PATH}/session/{id}")
}

/// `Manager.ListSessions` → wiersze; odpornie na brak ścieżki w strukturze.
fn list_sessions(conn: &Connection) -> Result<Vec<RawSession>, String> {
    let reply = conn
        .call_method(
            Some(LOGIND_DEST),
            LOGIND_PATH,
            Some(MANAGER_IFACE),
            "ListSessions",
            &(),
        )
        .map_err(|err| format!("logind: {err}"))?;
    // logind zwraca `a(susso)`: (id, uid, user, seat, ścieżka obiektu).
    if let Ok(rows) = reply
        .body()
        .deserialize::<Vec<(String, u32, String, String, OwnedObjectPath)>>()
    {
        return Ok(rows
            .into_iter()
            .map(|(id, uid, user, seat, path)| RawSession {
                path: path.to_string(),
                id,
                uid,
                user,
                seat,
            })
            .collect());
    }
    // Wariant bez ścieżki (starsze/odmienne implementacje): `a(suss)`.
    let rows = reply
        .body()
        .deserialize::<Vec<(String, u32, String, String)>>()
        .unwrap_or_default();
    Ok(rows
        .into_iter()
        .map(|(id, uid, user, seat)| RawSession {
            path: session_path(&id),
            id,
            uid,
            user,
            seat,
        })
        .collect())
}

/// Migawka sesji logind.
pub fn snapshot() -> Snapshot {
    let mut snap = Snapshot::default();
    let conn = match system() {
        Ok(conn) => conn,
        Err(err) => {
            snap.error = Some(err);
            return snap;
        }
    };
    let rows = match list_sessions(&conn) {
        Ok(rows) => rows,
        Err(err) => {
            snap.error = Some(err);
            return snap;
        }
    };
    snap.available = true;
    let current_id = std::env::var("XDG_SESSION_ID")
        .ok()
        .filter(|s| !s.is_empty());
    let mut sessions = Vec::with_capacity(rows.len());
    for RawSession {
        id,
        uid,
        user,
        seat,
        path,
    } in rows
    {
        let props = all_props(&conn, &path, SESSION_IFACE);
        let kind = as_string(&props, "Type").unwrap_or_else(|| "tty".to_owned());
        let active = as_bool(&props, "Active").unwrap_or(false);
        let seat = if seat.is_empty() {
            as_string(&props, "Seat").unwrap_or_default()
        } else {
            seat
        };
        let current = current_id.as_deref() == Some(id.as_str());
        sessions.push(Session {
            id,
            user,
            uid,
            seat,
            kind,
            active,
            current,
        });
    }
    sort_sessions(&mut sessions);
    snap.sessions = sessions;
    snap
}

/// Bieżąca sesja najpierw, potem aktywne, dalej wg uid/siedzenia/id.
fn sort_sessions(sessions: &mut [Session]) {
    sessions.sort_by(|a, b| {
        b.current
            .cmp(&a.current)
            .then_with(|| b.active.cmp(&a.active))
            .then_with(|| a.uid.cmp(&b.uid))
            .then_with(|| a.seat.cmp(&b.seat))
            .then_with(|| a.id.cmp(&b.id))
    });
}

fn validate_session_id(id: &str) -> Result<(), String> {
    if id.is_empty() {
        return Err("Pusty identyfikator sesji".to_owned());
    }
    if id.starts_with('-') || id.chars().any(char::is_whitespace) {
        return Err("Nieprawidłowy identyfikator sesji".to_owned());
    }
    Ok(())
}

fn find_session_path(conn: &Connection, id: &str) -> Option<String> {
    list_sessions(conn)
        .ok()?
        .into_iter()
        .find(|session| session.id == id)
        .map(|session| session.path)
}

/// Przełącza na sesję (`Manager.ActivateSession`; wymaga uprawnień logind).
pub fn switch_to(id: &str) -> Result<(), String> {
    validate_session_id(id)?;
    let conn = system()?;
    conn.call_method(
        Some(LOGIND_DEST),
        LOGIND_PATH,
        Some(MANAGER_IFACE),
        "ActivateSession",
        &id,
    )
    .map(|_| ())
    .map_err(|err| format!("logind: {err}"))
}

/// Blokuje sesję (`Session.Lock`).
pub fn lock_session(id: &str) -> Result<(), String> {
    validate_session_id(id)?;
    let conn = system()?;
    let path = find_session_path(&conn, id).unwrap_or_else(|| session_path(id));
    conn.call_method(
        Some(LOGIND_DEST),
        path.as_str(),
        Some(SESSION_IFACE),
        "Lock",
        &(),
    )
    .map(|_| ())
    .map_err(|err| format!("logind: {err}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(id: &str, uid: u32, seat: &str, active: bool, current: bool) -> Session {
        Session {
            id: id.to_owned(),
            user: format!("user{uid}"),
            uid,
            seat: seat.to_owned(),
            kind: "wayland".to_owned(),
            active,
            current,
        }
    }

    #[test]
    fn sorts_current_then_active_then_uid() {
        let mut sessions = vec![
            session("c2", 1001, "seat0", false, false),
            session("c1", 1000, "seat0", true, false),
            session("c3", 1000, "seat0", true, true),
        ];
        sort_sessions(&mut sessions);
        let ids: Vec<_> = sessions.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, ["c3", "c1", "c2"]);
    }

    #[test]
    fn builds_logind_session_path() {
        assert_eq!(session_path("c3"), "/org/freedesktop/login1/session/c3");
    }
}
