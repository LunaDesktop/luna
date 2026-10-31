//! Odtwarzacz multimedialny w Quick Panelu — odczyt i sterowanie przez MPRIS
//! (`org.mpris.MediaPlayer2.Player`).
//!
//! Odpytywanie odbywa się w wątku tła (D-Bus potrafi blokować na starcie sesji),
//! a powłoka czyta tylko ostatnią migawkę. Polecenia (play/pauza, następny,
//! poprzedni) idą osobnym wątkiem, żeby klatka UI nigdy nie czekała na magistralę.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use zbus::zvariant::OwnedValue;

/// Bieżący utwór i stan odtwarzacza.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaInfo {
    /// Nazwa usługi D-Bus odtwarzacza (np. `org.mpris.MediaPlayer2.spotify`).
    pub player: String,
    pub title: String,
    pub artist: String,
    pub playing: bool,
}

/// Wątek śledzący MPRIS i ostatnia migawka.
pub struct Media {
    info: Arc<Mutex<Option<MediaInfo>>>,
}

impl Media {
    /// Startuje odpytywanie co ~1,5 s (pierwsze od razu w tle).
    pub fn spawn() -> Self {
        let info: Arc<Mutex<Option<MediaInfo>>> = Arc::new(Mutex::new(None));
        let writer = Arc::clone(&info);
        std::thread::spawn(move || {
            let Ok(conn) = zbus::blocking::Connection::session() else {
                return;
            };
            loop {
                let fresh = query(&conn);
                if let Ok(mut slot) = writer.lock() {
                    *slot = fresh;
                }
                std::thread::sleep(Duration::from_millis(1500));
            }
        });
        Self { info }
    }

    /// Ostatnia znana migawka (kopia).
    pub fn current(&self) -> Option<MediaInfo> {
        self.info.lock().ok().and_then(|g| g.clone())
    }
}

/// Wysyła polecenie do odtwarzacza z migawki (w tle).
pub fn send(player: &str, method: &'static str) {
    let player = player.to_owned();
    std::thread::spawn(move || {
        let Ok(conn) = zbus::blocking::Connection::session() else {
            return;
        };
        let _ = conn.call_method(
            Some(player.as_str()),
            "/org/mpris/MediaPlayer2",
            Some("org.mpris.MediaPlayer2.Player"),
            method,
            &(),
        );
    });
}

fn query(conn: &zbus::blocking::Connection) -> Option<MediaInfo> {
    let reply = conn
        .call_method(
            Some("org.freedesktop.DBus"),
            "/org/freedesktop/DBus",
            Some("org.freedesktop.DBus"),
            "ListNames",
            &(),
        )
        .ok()?;
    let names: Vec<String> = reply.body().deserialize().ok()?;

    let mut fallback: Option<MediaInfo> = None;
    for name in names
        .iter()
        .filter(|n| n.starts_with("org.mpris.MediaPlayer2."))
    {
        let Some(info) = player_info(conn, name) else {
            continue;
        };
        if info.playing {
            return Some(info);
        }
        fallback.get_or_insert(info);
    }
    fallback
}

fn player_info(conn: &zbus::blocking::Connection, name: &str) -> Option<MediaInfo> {
    let reply = conn
        .call_method(
            Some(name),
            "/org/mpris/MediaPlayer2",
            Some("org.freedesktop.DBus.Properties"),
            "GetAll",
            &("org.mpris.MediaPlayer2.Player",),
        )
        .ok()?;
    let props: HashMap<String, OwnedValue> = reply.body().deserialize().ok()?;

    let status = props
        .get("PlaybackStatus")
        .and_then(|v| <&str>::try_from(v).ok())
        .unwrap_or("Stopped");
    if status == "Stopped" {
        return None;
    }

    let metadata = props
        .get("Metadata")
        .and_then(|v| v.try_clone().ok())
        .and_then(|v| HashMap::<String, OwnedValue>::try_from(v).ok())
        .unwrap_or_default();

    let title = metadata
        .get("xesam:title")
        .and_then(|v| <&str>::try_from(v).ok())
        .unwrap_or("")
        .trim()
        .to_owned();
    if title.is_empty() {
        return None;
    }
    let artist = metadata
        .get("xesam:artist")
        .and_then(|v| {
            Vec::<String>::try_from(v.try_clone().ok()?)
                .ok()
                .map(|a| a.join(", "))
                .or_else(|| <&str>::try_from(v).ok().map(str::to_owned))
        })
        .unwrap_or_default();

    Some(MediaInfo {
        player: name.to_owned(),
        title,
        artist,
        playing: status == "Playing",
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn media_info_is_comparable() {
        let a = MediaInfo {
            player: "org.mpris.MediaPlayer2.x".to_owned(),
            title: "T".to_owned(),
            artist: "A".to_owned(),
            playing: true,
        };
        assert_eq!(a.clone(), a);
    }
}
