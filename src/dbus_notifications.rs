//! Usługa powiadomień D‑Bus (`org.freedesktop.Notifications`).
//!
//! Powłoka rejestruje się na sesyjnej magistrali jako **daemon powiadomień**, więc
//! aplikacje (np. `notify-send`, klienci bibliotek) trafiają wprost do toastów Luny.
//! Serwer obsługuje magistralę we własnym wątku (zbus, API blokujące), a UI opróżnia
//! kolejkę w [`NotificationsBus::drain`] i przerysowuje okno po wybudzeniu
//! ([`WakeHandle`]) — powiadomienie pojawia się więc także, gdy powłoka śpi.
//!
//! Obsługiwane są `CloseNotification` (UI dostaje ID do zamknięcia) oraz `replaces_id`
//! (aktualizacja istniejącego powiadomienia bez tworzenia nowego). Capabilities:
//! `actions`, `body`, `persistence` (timeout 0 = wpis trwały).
//!
//! Gdy nazwę `org.freedesktop.Notifications` trzyma już inny daemon (np. w sesji
//! Xfce obok Luny), [`NotificationsBus::spawn`] zwraca `None` i nic nie przejmujemy.

use std::collections::HashSet;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use libluna_platform::WakeHandle;
use zbus::zvariant::OwnedValue;

/// Żądanie powiadomienia odebrane z magistrali.
#[derive(Debug, Clone, Default)]
pub struct Incoming {
    /// Identyfikator nadany przez nas (do `NotificationClosed`/`ActionInvoked`).
    pub id: u32,
    /// Nazwa aplikacji wysyłającej.
    pub app_name: String,
    /// Podsumowanie (tytuł toasta).
    pub summary: String,
    /// Treść.
    pub body: String,
    /// Czas życia w ms: `-1` = domyślny serwera, `0` = trwałe, `> 0` = jawny.
    pub expire_ms: i32,
    /// Pary `(klucz, etykieta)` akcji do `ActionInvoked`.
    pub actions: Vec<(String, String)>,
    /// `hint urgency = 2` (critical) — takich powiadomień nie wygaszamy automatycznie.
    pub critical: bool,
    /// Ikona aplikacji (`app_icon` albo hint `image-path`).
    pub icon: Option<String>,
    /// Hint `desktop-entry` — plik `.desktop` nadawcy (np. `eu.luna.Store.desktop`).
    /// Powłoka używa go po kliknięciu toasta: aktywuje istniejące okno aplikacji
    /// albo uruchamia ją, jeśli nie działa.
    pub desktop_entry: Option<String>,
}

impl Incoming {
    /// Czas życia w sekundach (dla [`crate::notifications::Notification`]).
    ///
    /// `urgency = critical` jest trwałe; `0` to jawne „bez wygasania”
    /// (`persistence`), a `-1` to domyślne 5 s.
    pub fn timeout_secs(&self) -> f64 {
        if self.critical {
            return 0.0;
        }
        match self.expire_ms {
            0 => 0.0,
            ms if ms > 0 => ms as f64 / 1000.0,
            _ => 5.0,
        }
    }
}

/// Kolejka odebranych powiadomień (współdzielona z wątkiem magistrali).
type Queue = Arc<Mutex<Vec<Incoming>>>;
/// Żądania `CloseNotification` z magistrali (ID do zamknięcia).
type CloseQueue = Arc<Mutex<Vec<u32>>>;
/// ID powiadomień, które są jeszcze „żywe” (do `replaces_id`).
type LiveIds = Arc<Mutex<HashSet<u32>>>;

/// Co serwer potrafi (patrz `GetCapabilities`).
///
/// `persistence` = powiadomienia z `expire_timeout = 0` nie znikają same;
/// `body-markup` pozwala klientom (np. edytorom) używać prostego znacznika HTML —
/// treść i tak sprowadzamy do czystego tekstu, żeby nasz renderer jej nie zgubił.
const CAPABILITIES: [&str; 6] = [
    "actions",
    "body",
    "body-markup",
    "icon-static",
    "persistence",
    "sound",
];

/// Wartość tekstowa hinta (np. `image-path`, `desktop-entry`).
fn hint_string(hints: &std::collections::HashMap<String, OwnedValue>, key: &str) -> Option<String> {
    hints
        .get(key)
        .and_then(|value| String::try_from(value.clone()).ok())
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

/// Sprowadza treść z `body-markup` do czystego tekstu.
///
/// Usuwamy wyłącznie wyglądające na HTML znaczniki (nazwa zaczyna się od litery),
/// więc zwykły tekst w rodzaju `x < y` zostaje bez zmian. Encje zamieniamy na znaki.
fn plain_text(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch != '<' {
            out.push(ch);
            continue;
        }
        let mut probe = String::new();
        let mut closed = false;
        while let Some(&next) = chars.peek() {
            chars.next();
            if next == '>' {
                closed = true;
                break;
            }
            probe.push(next);
            if probe.len() > 256 {
                break;
            }
        }
        let body = probe.trim_start_matches('/').trim();
        let name = body.split_whitespace().next().unwrap_or("");
        // Tylko znaczniki znane z powiadomień (nie dowolny `<T>` z kodu/tytułu).
        const TAGS: [&str; 14] = [
            "b", "i", "u", "a", "img", "span", "br", "tt", "big", "small", "font", "sub", "sup",
            "em",
        ];
        let lower = name.to_ascii_lowercase();
        let looks_like_tag = closed
            && probe.chars().next().is_some_and(|c| !c.is_whitespace())
            && TAGS.contains(&lower.as_str());
        if looks_like_tag {
            continue;
        }
        out.push('<');
        out.push_str(&probe);
        if closed {
            out.push('>');
        }
    }
    out.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
}

/// Przydziela ID: `replaces_id` tylko wtedy, gdy wskazuje żywe powiadomienie.
fn allocate_id(live: &mut HashSet<u32>, next: &AtomicU32, replaces_id: u32) -> u32 {
    if replaces_id != 0 && live.contains(&replaces_id) {
        return replaces_id;
    }
    loop {
        let id = next.fetch_add(1, Ordering::Relaxed);
        if id != 0 && !live.contains(&id) {
            live.insert(id);
            return id;
        }
    }
}

/// Interfejs `org.freedesktop.Notifications` (serwer).
struct Server {
    queue: Queue,
    closes: CloseQueue,
    live: LiveIds,
    next_id: Arc<AtomicU32>,
    waker: WakeHandle,
}

#[zbus::interface(name = "org.freedesktop.Notifications")]
impl Server {
    fn get_capabilities(&self) -> Vec<String> {
        CAPABILITIES.iter().map(|c| (*c).to_owned()).collect()
    }

    #[allow(clippy::too_many_arguments)]
    fn notify(
        &self,
        app_name: String,
        replaces_id: u32,
        app_icon: String,
        summary: String,
        body: String,
        actions: Vec<String>,
        hints: std::collections::HashMap<String, OwnedValue>,
        expire_ms: i32,
    ) -> u32 {
        let acts: Vec<(String, String)> = actions
            .chunks_exact(2)
            .map(|c| (c[0].clone(), c[1].clone()))
            .collect();
        // `urgency`: 0 = low, 1 = normal, 2 = critical.
        let critical = hints.get("urgency").and_then(|v| u8::try_from(v).ok()) == Some(2);
        let icon = (!app_icon.trim().is_empty())
            .then(|| app_icon.trim().to_owned())
            .or_else(|| hint_string(&hints, "image-path"));
        let desktop_entry = hint_string(&hints, "desktop-entry");
        let id = {
            let mut queue = self.queue.lock().unwrap_or_else(|e| e.into_inner());
            let mut live = self.live.lock().unwrap_or_else(|e| e.into_inner());
            let id = allocate_id(&mut live, &self.next_id, replaces_id);
            queue.push(Incoming {
                id,
                app_name,
                summary: plain_text(&summary),
                body: plain_text(&body),
                expire_ms,
                actions: acts,
                critical,
                icon,
                desktop_entry,
            });
            id
        };
        // Obudź pętlę powłoki, żeby nowy toast został narysowany.
        self.waker.wake();
        id
    }

    /// `CloseNotification`: zamyka powiadomienie po stronie UI (powód 3).
    fn close_notification(&self, id: u32) {
        // Usuwamy z „żywych” od razu, żeby `replaces_id` nie wskazał
        // powiadomienia, które zaraz zniknie.
        let was_live = self
            .live
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&id);
        if !was_live {
            return;
        }
        self.closes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(id);
        self.waker.wake();
    }

    fn get_server_information(&self) -> (String, String, String, String) {
        (
            "Luna".to_owned(),
            "libLuna".to_owned(),
            env!("CARGO_PKG_VERSION").to_owned(),
            "1.2".to_owned(),
        )
    }
}

/// Stan nazwy `org.freedesktop.Notifications` na magistrali sesyjnej.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NameState {
    /// Nikt nie obsługuje nazwy — możemy ją zająć.
    Free,
    /// Nazwę trzyma stary/nieaktualny `luna-shell` — można ją przejąć.
    Luna,
    /// Nazwę trzyma inny daemon (dunst, mako, GNOME…) — nie odbieramy mu jej.
    Other,
}

/// Sprawdza właściciela nazwy powiadomień (bez blokowania na dłużej niż zwykłe
/// wywołanie D-Bus).
fn notification_name_state() -> NameState {
    let Ok(connection) = zbus::blocking::Connection::session() else {
        return NameState::Free;
    };
    let has_owner: bool = connection
        .call_method(
            Some("org.freedesktop.DBus"),
            "/org/freedesktop/DBus",
            Some("org.freedesktop.DBus"),
            "NameHasOwner",
            &"org.freedesktop.Notifications",
        )
        .ok()
        .and_then(|reply| reply.body().deserialize().ok())
        .unwrap_or(false);
    if !has_owner {
        return NameState::Free;
    }
    let Ok(reply) = connection.call_method(
        Some("org.freedesktop.Notifications"),
        "/org/freedesktop/Notifications",
        Some("org.freedesktop.Notifications"),
        "GetServerInformation",
        &(),
    ) else {
        return NameState::Other;
    };
    let Ok((name, vendor, _, _)) = reply
        .body()
        .deserialize::<(String, String, String, String)>()
    else {
        return NameState::Other;
    };
    if name == "Luna" || vendor == "libLuna" {
        NameState::Luna
    } else {
        NameState::Other
    }
}

/// Loguje pierwszą przyczynę braku rejestracji (retry nie może zasypać logu).
fn warn_once(message: &str) {
    static WARNED: std::sync::Once = std::sync::Once::new();
    WARNED.call_once(|| eprintln!("luna-shell: powiadomienia: {message}"));
}

/// Uchwyt do usługi powiadomień (utrzymuje połączenie i kolejkę).
pub struct NotificationsBus {
    queue: Queue,
    closes: CloseQueue,
    live: LiveIds,
    _conn: zbus::blocking::Connection,
}

impl NotificationsBus {
    /// Rejestruje powłokę jako daemon powiadomień na sesyjnej magistrali.
    ///
    /// `None`, gdy brak magistrali albo nazwę `org.freedesktop.Notifications` trzyma
    /// już **inny** daemon. Jeśli nazwę trzyma stary `luna-shell`, przejmujemy ją
    /// (`ReplaceExisting`), żeby po restarcie powłoki powiadomienia znów działały.
    /// `waker` budzi pętlę UI po każdym odebranym powiadomieniu.
    pub fn spawn(waker: WakeHandle) -> Option<Self> {
        let queue: Queue = Arc::new(Mutex::new(Vec::new()));
        let closes: CloseQueue = Arc::new(Mutex::new(Vec::new()));
        let live: LiveIds = Arc::new(Mutex::new(HashSet::new()));
        let next_id = Arc::new(AtomicU32::new(1));

        let state = notification_name_state();
        if state == NameState::Other {
            warn_once("nazwę org.freedesktop.Notifications trzyma inny daemon — nie przejmuję jej");
            return None;
        }
        let replace = state == NameState::Luna;

        let build = || -> Result<zbus::blocking::Connection, String> {
            let server = Server {
                queue: Arc::clone(&queue),
                closes: Arc::clone(&closes),
                live: Arc::clone(&live),
                next_id: Arc::clone(&next_id),
                waker: waker.clone(),
            };
            zbus::blocking::connection::Builder::session()
                .map_err(|err| format!("sesja D-Bus: {err}"))?
                .name("org.freedesktop.Notifications")
                .map_err(|err| format!("nazwa D-Bus: {err}"))?
                .serve_at("/org/freedesktop/Notifications", server)
                .map_err(|err| format!("interfejs D-Bus: {err}"))?
                // Inne instancje Luny mogą nas później zastąpić (stary proces).
                .allow_name_replacements(true)
                .replace_existing_names(replace)
                .build()
                .map_err(|err| format!("rejestracja D-Bus: {err}"))
        };
        let conn = match build() {
            Ok(conn) => conn,
            Err(err) => {
                warn_once(&err);
                return None;
            }
        };
        if replace {
            warn_once("przejęto nazwę powiadomień po starym luna-shell");
        }
        Some(Self {
            queue,
            closes,
            live,
            _conn: conn,
        })
    }

    /// Czy połączenie z magistralą nadal żyje (bus mógł zostać zrestartowany).
    pub fn is_connected(&self) -> bool {
        !self._conn.is_closed()
    }

    /// Opróżnia kolejkę (UI woła raz na klatkę).
    pub fn drain(&self) -> Vec<Incoming> {
        let mut q = self.queue.lock().unwrap_or_else(|e| e.into_inner());
        std::mem::take(&mut *q)
    }

    /// Żądania `CloseNotification` od aplikacji (ID do usunięcia z UI).
    pub fn drain_closed(&self) -> Vec<u32> {
        let mut q = self.closes.lock().unwrap_or_else(|e| e.into_inner());
        std::mem::take(&mut *q)
    }

    /// Sygnalizuje zamknięcie powiadomienia (`NotificationClosed`).
    ///
    /// `reason`: 1 = wygasło, 2 = zamknięte przez użytkownika, 3 = `CloseNotification`.
    pub fn signal_closed(&self, id: u32, reason: u32) {
        if id == 0 {
            return;
        }
        // ID nie jest już żywe — kolejne `replaces_id` dostanie nowy numer.
        self.live
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&id);
        let _ = self._conn.emit_signal(
            None::<&str>,
            "/org/freedesktop/Notifications",
            "org.freedesktop.Notifications",
            "NotificationClosed",
            &(id, reason),
        );
    }

    /// Sygnalizuje wywołanie akcji (`ActionInvoked`).
    pub fn signal_action(&self, id: u32, key: &str) {
        if id == 0 {
            return;
        }
        let _ = self._conn.emit_signal(
            None::<&str>,
            "/org/freedesktop/Notifications",
            "org.freedesktop.Notifications",
            "ActionInvoked",
            &(id, key),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timeout_maps_special_values() {
        let with = |ms| {
            Incoming {
                expire_ms: ms,
                ..Default::default()
            }
            .timeout_secs()
        };
        assert_eq!(with(-1), 5.0);
        assert_eq!(with(0), 0.0);
        assert_eq!(with(2500), 2.5);
        // Critical nie wygasa automatycznie, nawet z jawnym timeoutem.
        let critical = Incoming {
            expire_ms: 1000,
            critical: true,
            ..Default::default()
        };
        assert_eq!(critical.timeout_secs(), 0.0);
    }

    #[test]
    fn ids_are_monotonic_and_replaces_only_reuses_live_ids() {
        let next = AtomicU32::new(1);
        let mut live = HashSet::new();

        let a = allocate_id(&mut live, &next, 0);
        let b = allocate_id(&mut live, &next, 0);
        assert!(b > a, "nowe ID muszą rosnąć ({a} → {b})");

        // `replaces_id` żywego powiadomienia jest respektowane.
        assert_eq!(allocate_id(&mut live, &next, a), a);
        // Nieistniejące `replaces_id` dostaje nowy numer (nie koliduje).
        let c = allocate_id(&mut live, &next, 999);
        assert_ne!(c, 999);
        assert_ne!(c, a);
        assert_ne!(c, b);
    }

    #[test]
    fn capabilities_match_what_ui_supports() {
        assert!(CAPABILITIES.contains(&"actions"), "przyciski akcji");
        assert!(CAPABILITIES.contains(&"body"), "treść powiadomienia");
        assert!(
            CAPABILITIES.contains(&"body-markup"),
            "prosty znacznik HTML"
        );
        assert!(CAPABILITIES.contains(&"persistence"), "timeout = 0");
    }

    #[test]
    fn markup_is_reduced_to_plain_text() {
        assert_eq!(
            plain_text("<b>Gotowe</b> — <i>3</i> błędy"),
            "Gotowe — 3 błędy"
        );
        assert_eq!(plain_text("x < y oraz a &amp; b"), "x < y oraz a & b");
        assert_eq!(plain_text("x < y > z"), "x < y > z");
        assert_eq!(
            plain_text("Błąd w Vec<String> — <b>sprawdź</b>"),
            "Błąd w Vec<String> — sprawdź"
        );
        assert_eq!(plain_text("<a href=\"https://x\">link</a>"), "link");
    }

    /// Test integracyjny: rejestruje usługę i wysyła `Notify` przez magistralę.
    /// Wymaga sesyjnej magistrali D-Bus, więc jest domyślnie pomijany.
    #[test]
    #[ignore = "wymaga sesyjnej magistrali D-Bus"]
    fn integration_notify_reaches_queue() {
        use std::collections::HashMap;

        use libluna_platform::WakeHandle;
        use zbus::zvariant::Value;

        let bus = NotificationsBus::spawn(WakeHandle::detached()).expect("usługa powiadomień");
        assert!(bus.is_connected());
        let connection = zbus::blocking::Connection::session().expect("sesja D-Bus");
        let actions: Vec<&str> = Vec::new();
        let hints: HashMap<&str, Value<'_>> = HashMap::new();
        let id: u32 = connection
            .call_method(
                Some("org.freedesktop.Notifications"),
                "/org/freedesktop/Notifications",
                Some("org.freedesktop.Notifications"),
                "Notify",
                &("Test", 0u32, "", "Tytuł", "Treść", actions, hints, 5000i32),
            )
            .expect("Notify")
            .body()
            .deserialize()
            .expect("id");
        let incoming = bus.drain();
        assert_eq!(incoming.len(), 1);
        assert_eq!(incoming[0].summary, "Tytuł");
        assert_eq!(incoming[0].body, "Treść");
        assert_eq!(incoming[0].id, id);
        bus.signal_closed(id, 2);
    }
}
