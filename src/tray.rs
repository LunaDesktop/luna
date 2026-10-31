//! System tray (StatusNotifierItem) dla panelu Luny.
//!
//! Powłoka jest jednocześnie **watcherem** (`org.kde.StatusNotifierWatcher`)
//! i **hostem** tray’a. Aplikacje rejestrują swoje ikony przez D-Bus, a my
//! odczytujemy ich właściwości (`IconName`/`IconPixmap`, `Title`, `Status`, `Menu`)
//! i pokazujemy je na pasku. Odświeżanie jest odporne na aplikacje, które nie
//! wysyłają sygnałów `NewIcon`/`NewTitle` — co sekundę robimy tanie `GetAll`.
//!
//! Ikony nazwane rysuje `libluna_widgets::paint_named_icon` (zestaw ikon
//! użytkownika), a `IconPixmap` konwertujemy z ARGB32 (network order) do
//! premultiplied RGBA8, czyli formatu sceny.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use libluna_platform::WakeHandle;
use libluna_render::ImageData;
use zbus::blocking::Connection;
use zbus::message::Header;
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};

/// Ikona pozycji tray’a.
#[derive(Clone, Debug, PartialEq)]
pub enum TrayIcon {
    /// Nazwa ikony freedesktop (np. `telegram`, `nm-signal-100`).
    Named(String),
    /// Gotowy obraz z `IconPixmap`.
    Pixmap(Arc<ImageData>),
    /// Brak ikony — panel rysuje symbol zastępczy.
    None,
}

/// Status pozycji zgodnie z SNI.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrayStatus {
    Passive,
    Active,
    NeedsAttention,
}

/// Pozycja system traya.
#[derive(Clone, Debug, PartialEq)]
pub struct TrayItem {
    /// Nazwa usługi D-Bus (np. `:1.42` albo `org.kde.kmix`).
    pub service: String,
    /// Ścieżka obiektu SNI (zwykle `/StatusNotifierItem`).
    pub path: String,
    /// `Id` z SNI (stabilny identyfikator aplikacji).
    pub id: String,
    /// Etykieta do menu overflow.
    pub title: String,
    pub status: TrayStatus,
    pub icon: TrayIcon,
    /// Ścieżka `com.canonical.dbusmenu` (jeśli aplikacja ją podała).
    pub menu: Option<String>,
}

impl TrayItem {
    /// Etykieta pozycji: tytuł, a gdy go brak — `Id`.
    pub fn label(&self) -> &str {
        if self.title.trim().is_empty() {
            &self.id
        } else {
            &self.title
        }
    }
}

/// Spłaszczony wiersz menu DBusMenu (menu kontekstowe ikony tray’a).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrayMenuRow {
    /// `id` pozycji DBusMenu (do `Event`).
    pub id: i32,
    pub label: String,
    pub enabled: bool,
    pub separator: bool,
    /// Poziom zagnieżdżenia (podmenu są wcięte).
    pub depth: usize,
    /// Pozycja ma dzieci (rysujemy strzałkę).
    pub submenu: bool,
}

type Items = Arc<Mutex<Vec<TrayItem>>>;
type Registrations = Arc<Mutex<Vec<Registration>>>;

#[derive(Clone, Debug, PartialEq, Eq)]
struct Registration {
    service: String,
    path: String,
}

/// Uchwyt do usługi tray’a (watcher + współdzielona lista pozycji).
pub struct TrayBus {
    items: Items,
    conn: Connection,
}

impl TrayBus {
    /// Rejestruje Lunę jako watcher i host SNI.
    ///
    /// `None`, gdy brak sesyjnej magistrali albo nazwę
    /// `org.kde.StatusNotifierWatcher` trzyma już inny panel (np. w sesji Xfce).
    pub fn spawn(waker: WakeHandle) -> Option<Self> {
        let registrations: Registrations = Arc::new(Mutex::new(Vec::new()));
        let items: Items = Arc::new(Mutex::new(Vec::new()));
        let watcher = Watcher {
            registrations: Arc::clone(&registrations),
        };
        let conn = zbus::blocking::connection::Builder::session()
            .ok()?
            .name("org.kde.StatusNotifierWatcher")
            .ok()?
            .serve_at("/StatusNotifierWatcher", watcher)
            .ok()?
            .build()
            .ok()?;

        let poll_conn = conn.clone();
        let poll_items = Arc::clone(&items);
        thread::Builder::new()
            .name("luna-tray".to_owned())
            .spawn(move || poll_loop(poll_conn, registrations, poll_items, waker))
            .ok()?;

        Some(Self { items, conn })
    }

    /// Migawka pozycji do narysowania (kolejność = kolejność rejestracji).
    ///
    /// `Passive` jest zgodnie z SNI stanem „nie pokazuj” — takie pozycje
    /// zostają w wykazie, ale nie trafiają na pasek.
    pub fn items(&self) -> Vec<TrayItem> {
        self.items
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .filter(|i| i.status != TrayStatus::Passive)
            .cloned()
            .collect()
    }

    /// Lewy klik: aplikacja otwiera swoje główne okno/menu.
    pub fn activate(&self, item: &TrayItem, x: i32, y: i32) {
        call_xy(&self.conn, item, "Activate", x, y);
    }

    /// Środkowy klik / akcja dodatkowa.
    pub fn secondary_activate(&self, item: &TrayItem, x: i32, y: i32) {
        call_xy(&self.conn, item, "SecondaryActivate", x, y);
    }

    /// Prawy klik. Część aplikacji implementuje `ContextMenu` samodzielnie;
    /// dla pozostałych menu jest dostępne przez `Menu` (DBusMenu) w przyszłości.
    pub fn context_menu(&self, item: &TrayItem, x: i32, y: i32) {
        call_xy(&self.conn, item, "ContextMenu", x, y);
    }

    /// Kółko myszy nad ikoną (SNI `Scroll`).
    pub fn scroll(&self, item: &TrayItem, delta: i32, horizontal: bool) {
        let orientation = if horizontal { "horizontal" } else { "vertical" };
        let _ = self.conn.call_method(
            Some(item.service.as_str()),
            item.path.as_str(),
            Some("org.kde.StatusNotifierItem"),
            "Scroll",
            &(delta, orientation),
        );
    }

    /// Pobiera menu DBusMenu przypięte do pozycji.
    ///
    /// `AboutToShow` jest wołane przed `GetLayout`, bo część aplikacji buduje
    /// zawartość dopiero na to żądanie. Głębokość 4 wystarcza dla typowych menu
    /// tray’a; podmenu są spłaszczone i wcięte.
    pub fn menu(&self, item: &TrayItem) -> Option<Vec<TrayMenuRow>> {
        let path = item.menu.as_deref()?;
        if path == "/" || path.is_empty() {
            return None;
        }
        let _ = self.conn.call_method(
            Some(item.service.as_str()),
            path,
            Some("com.canonical.dbusmenu"),
            "AboutToShow",
            &(0i32,),
        );
        let reply = self
            .conn
            .call_method(
                Some(item.service.as_str()),
                path,
                Some("com.canonical.dbusmenu"),
                "GetLayout",
                &(0i32, 4i32, Vec::<String>::new()),
            )
            .ok()?;
        let (_, root): (u32, MenuLayout) = reply.body().deserialize().ok()?;
        let mut rows = Vec::new();
        flatten_root(&root, &mut rows);
        (!rows.is_empty()).then_some(rows)
    }

    /// Wysyła `clicked` dla pozycji menu.
    pub fn menu_event(&self, item: &TrayItem, id: i32) {
        let Some(path) = item.menu.as_deref() else {
            return;
        };
        let _ = self.conn.call_method(
            Some(item.service.as_str()),
            path,
            Some("com.canonical.dbusmenu"),
            "Event",
            &(id, "clicked", Value::I32(0), 0u32),
        );
    }
}

type MenuLayout = (i32, HashMap<String, OwnedValue>, Vec<OwnedValue>);

fn menu_str(props: &HashMap<String, OwnedValue>, key: &str) -> Option<String> {
    props
        .get(key)
        .and_then(|v| <&str>::try_from(v).ok())
        .map(str::to_owned)
}

fn menu_bool(props: &HashMap<String, OwnedValue>, key: &str) -> Option<bool> {
    props.get(key).and_then(|v| bool::try_from(v).ok())
}

/// Węzeł 0 DBusMenu jest kontenerem — jego dzieci to właściwe pozycje menu.
fn flatten_root(root: &MenuLayout, out: &mut Vec<TrayMenuRow>) {
    for child in &root.2 {
        let Ok(child) = child.try_clone() else {
            continue;
        };
        let Ok(layout) = MenuLayout::try_from(child) else {
            continue;
        };
        flatten_menu(&layout, 0, out);
    }
}

fn flatten_menu(node: &MenuLayout, depth: usize, out: &mut Vec<TrayMenuRow>) {
    let (id, props, children) = node;
    if !menu_bool(props, "visible").unwrap_or(true) {
        return;
    }
    let separator = menu_str(props, "type").as_deref() == Some("separator");
    let label = menu_str(props, "label").unwrap_or_default();
    let enabled = menu_bool(props, "enabled").unwrap_or(true);
    let submenu =
        menu_str(props, "children-display").as_deref() == Some("submenu") || !children.is_empty();
    if separator {
        out.push(TrayMenuRow {
            id: *id,
            label: String::new(),
            enabled: false,
            separator: true,
            depth,
            submenu: false,
        });
    } else if !label.is_empty() {
        out.push(TrayMenuRow {
            id: *id,
            label,
            enabled,
            separator: false,
            depth,
            submenu,
        });
    }
    for child in children {
        let Ok(child) = child.try_clone() else {
            continue;
        };
        let Ok(layout) = MenuLayout::try_from(child) else {
            continue;
        };
        flatten_menu(&layout, depth + 1, out);
    }
}

fn call_xy(conn: &Connection, item: &TrayItem, method: &str, x: i32, y: i32) {
    let _ = conn.call_method(
        Some(item.service.as_str()),
        item.path.as_str(),
        Some("org.kde.StatusNotifierItem"),
        method,
        &(x, y),
    );
}

/// Serwer `org.kde.StatusNotifierWatcher`.
struct Watcher {
    registrations: Registrations,
}

#[zbus::interface(name = "org.kde.StatusNotifierWatcher")]
impl Watcher {
    /// Aplikacja rejestruje swoją ikonę. Argument może być nazwą usługi albo
    /// ścieżką obiektu (wtedy usługą jest nadawca komunikatu).
    fn register_status_notifier_item(&self, service: &str, #[zbus(header)] header: Header<'_>) {
        let sender = header.sender().map(ToString::to_string);
        let (service, path) = if service.starts_with('/') {
            let Some(sender) = sender else { return };
            (sender, service.to_owned())
        } else if service.is_empty() {
            let Some(sender) = sender else { return };
            (sender, "/StatusNotifierItem".to_owned())
        } else {
            (service.to_owned(), "/StatusNotifierItem".to_owned())
        };
        if service.is_empty() {
            return;
        }
        let mut regs = self.registrations.lock().unwrap_or_else(|e| e.into_inner());
        let reg = Registration { service, path };
        if !regs.contains(&reg) {
            regs.push(reg);
        }
    }

    /// Hosty (panele) zgłaszają się tutaj. Luna jest hostem, więc to tylko
    /// potwierdzenie dla aplikacji, które sprawdzają protokół.
    fn register_status_notifier_host(&self, _service: &str) {}

    #[zbus(property)]
    fn registered_status_notifier_items(&self) -> Vec<String> {
        self.registrations
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .map(|r| r.service.clone())
            .collect()
    }

    #[zbus(property)]
    fn is_status_notifier_host_registered(&self) -> bool {
        true
    }

    #[zbus(property)]
    fn protocol_version(&self) -> i32 {
        0
    }
}

/// Pętla odpytywania pozycji. Działa poza wątkiem UI; budzi powłokę tylko przy
/// realnej zmianie listy.
fn poll_loop(conn: Connection, registrations: Registrations, items: Items, waker: WakeHandle) {
    loop {
        let regs = registrations
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let previous = items.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let mut next = Vec::with_capacity(regs.len());
        for reg in regs {
            match read_item(&conn, &reg) {
                Some(item) => next.push(item),
                None => {
                    // Chwilowy błąd odczytu nie może „mrugać” ikoną: zostawiamy
                    // poprzednią pozycję, dopóki usługa istnieje.
                    if name_has_owner(&conn, &reg.service) {
                        if let Some(old) = previous
                            .iter()
                            .find(|i| i.service == reg.service && i.path == reg.path)
                        {
                            next.push(old.clone());
                        }
                    }
                }
            }
        }
        let changed = next != previous;
        if changed {
            *items.lock().unwrap_or_else(|e| e.into_inner()) = next;
            waker.wake();
        }
        thread::sleep(Duration::from_secs(1));
    }
}

fn name_has_owner(conn: &Connection, service: &str) -> bool {
    conn.call_method(
        Some("org.freedesktop.DBus"),
        "/org/freedesktop/DBus",
        Some("org.freedesktop.DBus"),
        "NameHasOwner",
        &(service,),
    )
    .ok()
    .and_then(|reply| reply.body().deserialize::<bool>().ok())
    .unwrap_or(false)
}

fn read_item(conn: &Connection, reg: &Registration) -> Option<TrayItem> {
    let reply = conn
        .call_method(
            Some(reg.service.as_str()),
            reg.path.as_str(),
            Some("org.freedesktop.DBus.Properties"),
            "GetAll",
            &("org.kde.StatusNotifierItem",),
        )
        .ok()?;
    let props: HashMap<String, OwnedValue> = reply.body().deserialize().ok()?;

    let text = |key: &str| {
        props
            .get(key)
            .and_then(|v| <&str>::try_from(v).ok())
            .map(str::to_owned)
            .filter(|s| !s.trim().is_empty())
    };

    let id = text("Id").unwrap_or_else(|| reg.service.clone());
    let title = text("Title").unwrap_or_default();
    let status = match text("Status").as_deref() {
        Some("NeedsAttention") => TrayStatus::NeedsAttention,
        Some("Passive") => TrayStatus::Passive,
        _ => TrayStatus::Active,
    };

    let icon = if let Some(name) = text("IconName") {
        TrayIcon::Named(name)
    } else if let Some(pixmap) = props
        .get("IconPixmap")
        .and_then(|v| v.try_clone().ok())
        .and_then(|v| Vec::<(i32, i32, Vec<u8>)>::try_from(v).ok())
        .and_then(|list| largest_pixmap(&list))
    {
        TrayIcon::Pixmap(Arc::new(pixmap))
    } else {
        TrayIcon::None
    };

    let menu = props
        .get("Menu")
        .and_then(|v| OwnedObjectPath::try_from(v.try_clone().ok()?).ok())
        .map(|p| p.to_string())
        .filter(|p| p != "/" && !p.is_empty());

    Some(TrayItem {
        service: reg.service.clone(),
        path: reg.path.clone(),
        id,
        title,
        status,
        icon,
        menu,
    })
}

/// Wybiera największy obraz z listy `(szerokość, wysokość, ARGB32)`.
fn largest_pixmap(list: &[(i32, i32, Vec<u8>)]) -> Option<ImageData> {
    list.iter()
        .filter(|(w, h, data)| *w > 0 && *h > 0 && data.len() >= (*w as usize) * (*h as usize) * 4)
        .max_by_key(|(w, h, _)| w.saturating_mul(*h))
        .and_then(|(w, h, data)| argb_to_image(*w as u32, *h as u32, data))
}

/// SNI podaje `IconPixmap` jako ARGB32 w kolejności sieciowej (A, R, G, B).
/// Scena oczekuje premultiplied RGBA8.
fn argb_to_image(width: u32, height: u32, data: &[u8]) -> Option<ImageData> {
    let pixels = (width as usize).checked_mul(height as usize)?;
    if pixels == 0 || data.len() < pixels * 4 {
        return None;
    }
    let mut rgba = Vec::with_capacity(pixels * 4);
    for px in data.chunks_exact(4).take(pixels) {
        let a = px[0];
        let r = premultiply(px[1], a);
        let g = premultiply(px[2], a);
        let b = premultiply(px[3], a);
        rgba.extend_from_slice(&[r, g, b, a]);
    }
    Some(ImageData::new(width, height, rgba))
}

fn premultiply(channel: u8, alpha: u8) -> u8 {
    ((u16::from(channel) * u16::from(alpha) + 127) / 255) as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn item_label_falls_back_to_id() {
        let mut item = TrayItem {
            service: ":1.2".into(),
            path: "/StatusNotifierItem".into(),
            id: "telegram".into(),
            title: String::new(),
            status: TrayStatus::Active,
            icon: TrayIcon::None,
            menu: None,
        };
        assert_eq!(item.label(), "telegram");
        item.title = "Telegram".into();
        assert_eq!(item.label(), "Telegram");
    }

    #[test]
    fn argb_pixmap_is_converted_and_premultiplied() {
        // Jeden piksel: A=128, R=255, G=0, B=64.
        let image = argb_to_image(1, 1, &[128, 255, 0, 64]).expect("obraz");
        assert_eq!(image.width, 1);
        assert_eq!(image.height, 1);
        assert_eq!(&image.rgba[..], &[128, 0, 32, 128]);
    }

    #[test]
    fn largest_pixmap_wins() {
        let small = (1, 1, vec![255, 0, 0, 0]);
        let big = (2, 2, vec![255, 255, 255, 255].repeat(4));
        let list = vec![small, big];
        let image = largest_pixmap(&list).expect("obraz");
        assert_eq!((image.width, image.height), (2, 2));
    }

    #[test]
    fn broken_pixmap_is_rejected() {
        assert!(argb_to_image(2, 2, &[0, 0, 0, 0]).is_none());
    }

    fn props(pairs: &[(&str, &str)]) -> HashMap<String, OwnedValue> {
        pairs
            .iter()
            .map(|(k, v)| {
                (
                    (*k).to_owned(),
                    OwnedValue::try_from(Value::from(*v)).expect("OwnedValue"),
                )
            })
            .collect()
    }

    #[test]
    fn dbusmenu_flattens_labels_depths_and_separators() {
        let child_props = props(&[("label", "Child")]);
        let child = (2i32, child_props, Vec::<OwnedValue>::new());
        let parent_props = props(&[("label", "Parent"), ("children-display", "submenu")]);
        let parent = (
            1i32,
            parent_props,
            vec![OwnedValue::try_from(Value::from(child)).expect("child")],
        );
        let sep_props = props(&[("type", "separator")]);
        let sep = (3i32, sep_props, Vec::<OwnedValue>::new());
        let root = (
            0i32,
            props(&[]),
            vec![
                OwnedValue::try_from(Value::from(parent)).expect("parent"),
                OwnedValue::try_from(Value::from(sep)).expect("sep"),
            ],
        );
        let mut rows = Vec::new();
        flatten_root(&root, &mut rows);
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].label, "Parent");
        assert!(rows[0].submenu);
        assert_eq!(rows[1].label, "Child");
        assert_eq!(rows[1].depth, 1);
        assert!(rows[2].separator);
    }
}
