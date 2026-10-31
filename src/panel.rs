//! Panel górny i launcher środowiska Luna.
//!
//! Warstwa rysowania jest oddzielona od źródła danych ([`PanelState`], [`App`]), więc
//! panel można renderować headless (podglądy) oraz „na żywo” w sesji Selene.
//!
//! Panel ma po lewej **sam przycisk burger** (bez etykiety) — kliknięcie otwiera launcher.
//! Launcher ma trzy widoki: **lista**, **siatka** i **karuzela**.

use libluna_core::holidays::{self, Holiday};
use libluna_core::style::weight;
use libluna_core::{Color, IconTheme, Point, Rect, Size, Theme, ThemePreference};
use libluna_i18n::{tr, tr_args};
use libluna_render::TextEngine;
use libluna_widgets::{paint_icon, paint_icon_themed, paint_named_icon, Hover, IconName, PaintCtx};

use crate::calendar::CalendarEvent;
use crate::network::WifiNetwork;
use crate::notifications::Notification;
use crate::power::PowerAction;
use crate::system::BatteryStatus;
use crate::toplevels::Task;
use crate::tray::{TrayIcon, TrayItem, TrayMenuRow, TrayStatus};

/// Wysokość panelu (piksele logiczne).
pub const THICKNESS: f64 = 40.0;
/// Domyślna szerokość karty launchera.
pub const LAUNCHER_W: f64 = 720.0;
/// Domyślna wysokość karty launchera.
pub const LAUNCHER_H: f64 = 600.0;
/// Bok przycisku zasilania w pasku.
pub const POWER_BTN: f64 = 24.0;
/// Szerokość rozwijanego menu zasilania.
pub const POWER_MENU_W: f64 = 176.0;
/// Wysokość pozycji menu zasilania.
pub const POWER_ITEM_H: f64 = 34.0;

const GRID_COLS: usize = 4;
const GRID_CELL_H: f64 = 76.0;
const LIST_ROW_H: f64 = 52.0;
const CARD_W: f64 = 168.0;
const CARD_H: f64 = 200.0;

/// Akcja przełącznika w pasku (kliknięcie).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ToggleAction {
    #[default]
    None,
    Wifi,
    Volume,
    /// Jasność ekranu (klik cyklicznie zmienia poziom).
    Brightness,
}

/// Przełącznik szybkiego dostępu (Wi-Fi, dźwięk, bateria…).
#[derive(Debug, Clone)]
pub struct Toggle {
    /// Pełna nazwa przełącznika (tytuł w popupie, np. SSID albo „Dźwięk”).
    /// **Nie** jest rysowana na pasku — pasek pokazuje same ikony.
    pub label: String,
    /// Krótka wartość obok ikony na pasku (np. „82%”); `None` = sama ikona.
    pub value: Option<String>,
    /// Ikona z pakietu Luny.
    pub icon: IconName,
    pub active: bool,
    /// Co robi klik (jeśli cokolwiek).
    pub action: ToggleAction,
}

/// Stan panelu — czyste dane, bez logiki okna (łatwe do testów i podglądów).
#[derive(Debug, Clone)]
pub struct PanelState {
    /// Tytuł aktywnego okna lub nazwa przestrzeni.
    pub title: String,
    /// Godzina, np. „14:32”.
    pub clock: String,
    /// Data, np. „pon, 10 paź”.
    pub date: String,
    /// Liczba przestrzeni roboczych.
    pub workspaces: usize,
    /// Indeks bieżącej przestrzeni roboczej.
    pub current_ws: usize,
    /// Przełączniki szybkiego dostępu.
    pub toggles: Vec<Toggle>,
    /// Nazwa zalogowanego użytkownika.
    pub user: String,
    /// Okna z kompozytora (`zwlr_foreign_toplevel`) — taskbar na pasku.
    pub tasks: Vec<Task>,
    /// Liczba powiadomień w historii (wysokość centrum powiadomień).
    pub notifications: usize,
    /// Liczba powiadomień, których użytkownik jeszcze nie widział w centrum
    /// (kropka przy zegarze znika po otwarciu centrum).
    pub unseen: usize,
    /// Pozycje system traya (StatusNotifierItem).
    pub tray: Vec<TrayItem>,
    /// Wskaźniki bezpieczeństwa (mikrofon/kamera/sieć).
    pub security: SecurityIndicators,
}

/// Aktywne wskaźniki prywatności rysowane na pasku.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SecurityIndicators {
    pub microphone: bool,
    pub camera: bool,
    /// Sieć pokazujemy tylko gdy jest alert (prośba/odmowa), nie przy każdym dozwolonym połączeniu.
    pub network: bool,
    pub alert: bool,
}

impl SecurityIndicators {
    pub fn any(self) -> bool {
        self.microphone || self.camera || self.network
    }
}

/// Akcja w modalu zgody.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecurityChoice {
    Allow,
    Deny,
}

/// Rozmieszczenie modala „Luna Security”.
#[derive(Debug, Clone, Copy)]
pub struct SecurityDialogLayout {
    pub card: Rect,
    pub allow: Rect,
    pub deny: Rect,
}

/// Modal jest wyśrodkowany pod paskiem; przy wąskim ekranie kurczy się do marginesów.
pub fn security_dialog_layout(size: Size, theme: &Theme) -> SecurityDialogLayout {
    let w = 560.0_f64.min((size.width - 64.0).max(360.0));
    let h = 222.0;
    let card = Rect::from_xywh(((size.width - w) / 2.0).max(16.0), THICKNESS + 72.0, w, h);
    let bw = 148.0;
    let bh = 36.0;
    let pad = theme.spacing.lg;
    let allow = Rect::from_xywh(card.right() - pad - bw, card.bottom() - pad - bh, bw, bh);
    let deny = Rect::from_xywh(allow.x() - 8.0 - bw, allow.y(), bw, bh);
    SecurityDialogLayout { card, allow, deny }
}

/// Trafienie w przycisk modala.
pub fn security_dialog_hit(layout: SecurityDialogLayout, p: Point) -> Option<SecurityChoice> {
    if layout.allow.contains(p) {
        Some(SecurityChoice::Allow)
    } else if layout.deny.contains(p) {
        Some(SecurityChoice::Deny)
    } else {
        None
    }
}

impl PanelState {
    /// Przykładowy stan (podglądy, testy).
    pub fn sample() -> Self {
        Self {
            title: format!("{} — {}", tr("Files"), tr("Documents")),
            clock: "14:32".to_owned(),
            date: "pon, 10 paź".to_owned(),
            workspaces: 4,
            current_ws: 1,
            toggles: vec![
                Toggle {
                    label: "Wi-Fi".to_owned(),
                    value: None,
                    icon: IconName::Wifi,
                    active: true,
                    action: ToggleAction::Wifi,
                },
                Toggle {
                    label: tr("Sound"),
                    value: None,
                    icon: IconName::Volume,
                    active: true,
                    action: ToggleAction::Volume,
                },
                Toggle {
                    label: tr("Battery"),
                    value: Some("82%".to_owned()),
                    icon: IconName::Battery,
                    active: false,
                    action: ToggleAction::None,
                },
            ],
            user: "patryks".to_owned(),
            tasks: vec![
                Task {
                    title: format!("{} — {}", tr("Files"), tr("Documents")),
                    app_id: "files".to_owned(),
                    active: true,
                    minimized: false,
                },
                Task {
                    title: tr("Terminal"),
                    app_id: "terminal".to_owned(),
                    active: false,
                    minimized: false,
                },
            ],
            notifications: 0,
            unseen: 0,
            tray: Vec::new(),
            security: SecurityIndicators::default(),
        }
    }
}

/// Widok launchera.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LauncherView {
    /// Pionowa lista wierszy.
    List,
    /// Siatka kafelków.
    #[default]
    Grid,
    /// Pozioma karuzela kart.
    Carousel,
}

impl LauncherView {
    /// Wszystkie widoki w kolejności przełącznika.
    pub const ALL: [LauncherView; 3] = [
        LauncherView::List,
        LauncherView::Grid,
        LauncherView::Carousel,
    ];

    /// Etykieta w przełączniku.
    pub fn label(self) -> String {
        match self {
            LauncherView::List => tr("List"),
            LauncherView::Grid => tr("Grid"),
            LauncherView::Carousel => tr("Carousel"),
        }
    }

    /// Następny widok (cyklicznie).
    pub fn next(self) -> Self {
        match self {
            LauncherView::List => LauncherView::Grid,
            LauncherView::Grid => LauncherView::Carousel,
            LauncherView::Carousel => LauncherView::List,
        }
    }
}

/// Stan launchera (widok + bieżąca karta karuzeli + przewinięcie + wyszukiwanie).
#[derive(Debug, Clone, Default)]
pub struct Launcher {
    pub view: LauncherView,
    pub carousel: usize,
    pub selected: Option<usize>,
    pub scroll: f64,
    /// Fraza wyszukiwania aplikacji.
    pub search: String,
    /// Pozycja pod kursorem — podświetlenie niezależne od zaznaczenia klawiatury.
    pub hover: Option<usize>,
    /// Klucze aplikacji przypiętych do góry listy (w kolejności przypinania).
    pub pinned: Vec<String>,
    /// Klucze ostatnio uruchomionych aplikacji (najnowsza pierwsza).
    pub recent: Vec<String>,
}

impl Launcher {
    pub fn new() -> Self {
        Self::default()
    }

    /// Ile ostatnio używanych aplikacji pamiętamy.
    pub const RECENT_LIMIT: usize = 8;

    /// Filtruje i porządkuje aplikacje: przypięte, potem ostatnio używane,
    /// a na końcu reszta w kolejności alfabetycznej z `apps`.
    ///
    /// Kolejność jest częścią kontraktu — indeksy z [`launcher_app_at`],
    /// nawigacji klawiatury i rysowania muszą się zgadzać.
    pub fn filter_apps<'a>(&self, apps: &'a [App]) -> Vec<&'a App> {
        let q = self.search.trim().to_lowercase();
        let hit = |a: &App| {
            q.is_empty()
                || a.name.to_lowercase().contains(&q)
                || a.category.to_lowercase().contains(&q)
                || a.exec.as_deref().unwrap_or("").to_lowercase().contains(&q)
        };
        // Klucze liczymy raz na aplikację — launcher woła tę funkcję co klatkę.
        let keys: Vec<String> = apps.iter().map(App::key).collect();
        let mut used = vec![false; apps.len()];
        let mut out: Vec<&App> = Vec::with_capacity(apps.len());

        let mut take = |key: &str, out: &mut Vec<&'a App>| {
            if let Some(i) = keys.iter().position(|k| k == key) {
                if !used[i] && hit(&apps[i]) {
                    used[i] = true;
                    out.push(&apps[i]);
                }
            }
        };
        for key in &self.pinned {
            take(key, &mut out);
        }
        for key in &self.recent {
            take(key, &mut out);
        }
        for (i, app) in apps.iter().enumerate() {
            if !used[i] && hit(app) {
                out.push(app);
            }
        }
        out
    }

    /// Czy aplikacja jest przypięta do góry listy.
    pub fn is_pinned(&self, app: &App) -> bool {
        self.pinned.contains(&app.key())
    }

    /// Przypina lub odpina aplikację. Zwraca `true`, gdy po zmianie jest przypięta.
    pub fn toggle_pin(&mut self, app: &App) -> bool {
        let key = app.key();
        if let Some(i) = self.pinned.iter().position(|k| k == &key) {
            self.pinned.remove(i);
            false
        } else {
            self.pinned.push(key);
            true
        }
    }

    /// Zapisuje uruchomienie aplikacji na początku listy „ostatnio używane”.
    pub fn remember_used(&mut self, app: &App) {
        let key = app.key();
        self.recent.retain(|k| k != &key);
        self.recent.insert(0, key);
        self.recent.truncate(Self::RECENT_LIMIT);
    }

    /// Przełącza na kolejny widok.
    pub fn next_view(&mut self) {
        self.view = self.view.next();
    }

    /// Bieżący indeks zaznaczenia (domyślnie 0, o ile są pozycje).
    pub fn selected_index(&self, count: usize) -> Option<usize> {
        if count == 0 {
            None
        } else {
            Some(self.selected.unwrap_or(0).min(count - 1))
        }
    }

    /// Przesuwa zaznaczenie o krok (`dx`, `dy`) zależnie od widoku.
    ///
    /// W siatce `dx`/`dy` to ruch odpowiednio w kolumnie/wierszu; w liście
    /// i karuzeli używany jest tylko jeden z nich (`dy`/`dx`).
    pub fn navigate(&mut self, count: usize, dx: i64, dy: i64) {
        if count == 0 {
            return;
        }
        let n = count as i64;
        let cur = self.selected.unwrap_or(0).min(count - 1) as i64;
        let next = match self.view {
            LauncherView::Grid => {
                let cols = GRID_COLS as i64;
                let rows = (n + cols - 1) / cols;
                let col = (cur % cols + dx).clamp(0, cols - 1);
                let row = (cur / cols + dy).clamp(0, rows - 1);
                (row * cols + col).min(n - 1)
            }
            _ => (cur + dx + dy).rem_euclid(n),
        };
        self.selected = Some(next as usize);
        if self.view == LauncherView::Carousel {
            self.carousel = next as usize;
        }
    }

    /// Przewija zawartość listy/siatki o `notches` ząbków kółka myszy.
    ///
    /// `notches` jest w „ząbkach” — **dodatnie = w dół** (dalej w treści / w stronę
    /// końca listy), ujemne = w górę. `px_per_notch` mówi, ile pikseli przypada na
    /// jeden ząbek. Wynik jest ograniczony do `[0, max]`.
    pub fn scroll_by_notches(&mut self, notches: f64, px_per_notch: f64, max: f64) {
        if !notches.is_finite() || notches == 0.0 {
            return;
        }
        self.scroll = (self.scroll + notches * px_per_notch).clamp(0.0, max.max(0.0));
    }

    /// Czyści frazę wyszukiwania i wraca do początku listy.
    ///
    /// Używane przez przycisk „×” w polu wyszukiwania oraz przy zamykaniu
    /// launchera — nowe otwarcie ma zawsze pokazywać pełną listę.
    pub fn clear_search(&mut self) {
        self.search.clear();
        self.scroll = 0.0;
        self.selected = Some(0);
        self.hover = None;
    }
}

/// Pozycja w launcherze.
#[derive(Debug, Clone)]
pub struct App {
    /// Ikona z pakietu Luny.
    pub icon: IconName,
    /// Nazwa ikony z pliku `.desktop` (np. `firefox`) — używana przez zestaw zewnętrzny.
    pub theme_icon: Option<String>,
    pub name: String,
    /// Krótki opis/kategoria (widok listy).
    pub category: String,
    /// Polecenie uruchamiające (dla prawdziwych aplikacji z plików `.desktop`).
    pub exec: Option<String>,
    /// Czy aplikacja wymaga uruchomienia w terminalu.
    pub terminal: bool,
    /// Plik `.desktop` — uruchamiamy przez `gio launch`, gdy jest dostępny.
    pub desktop_path: Option<std::path::PathBuf>,
}

impl App {
    /// Stabilny klucz aplikacji do zapisu w konfiguracji launchera.
    ///
    /// Preferujemy nazwę pliku `.desktop` (nie zależy od języka ani od nazwy
    /// wyświetlanej); dla wpisów przykładowych zostaje nazwa z prefiksem.
    pub fn key(&self) -> String {
        if let Some(path) = &self.desktop_path {
            if let Some(stem) = path.file_stem() {
                let stem = stem.to_string_lossy();
                if !stem.is_empty() {
                    return stem.into_owned();
                }
            }
        }
        format!("name:{}", self.name)
    }

    /// Przykładowy zestaw ikon (podglądy, gdy brak `.desktop`).
    pub fn sample() -> Vec<App> {
        const APPS: [(IconName, &str, &str); 12] = [
            (IconName::Files, "Files", "System"),
            (IconName::Settings, "Settings", "System"),
            (IconName::Edit, "Editor", "Accessories"),
            (IconName::Music, "Player", "Multimedia"),
            (IconName::Star, "Favorites", "System"),
            (IconName::Clock, "Clock", "Accessories"),
            (IconName::Terminal, "Terminal", "System"),
            (IconName::Store, "Store", "System"),
            (IconName::Wifi, "Network", "System"),
            (IconName::Home, "Home", "System"),
            (IconName::User, "Accounts", "System"),
            (IconName::Info, "Help", "System"),
        ];
        APPS.iter()
            .map(|(i, n, c)| App {
                icon: *i,
                theme_icon: None,
                name: tr(n),
                category: tr(c),
                exec: None,
                terminal: *n == "Terminal",
                desktop_path: None,
            })
            .collect()
    }

    /// Buduje pozycje launchera z wykrytych aplikacji (plików `.desktop`).
    pub fn from_desktop(list: &[crate::apps::DesktopApp]) -> Vec<App> {
        list.iter()
            .map(|d| App {
                icon: crate::apps::icon_for(d),
                theme_icon: d.icon.clone(),
                name: d.name.clone(),
                category: d.categories.first().cloned().unwrap_or_default(),
                exec: Some(d.exec.clone()),
                terminal: d.terminal,
                desktop_path: d.desktop_path.clone(),
            })
            .collect()
    }
}

/// Prostokąt przycisku burgera na panelu.
pub fn burger_rect(panel: Rect, theme: &Theme) -> Rect {
    let cy = panel.center().y;
    Rect::from_xywh(panel.x() + theme.spacing.md, cy - 14.0, 28.0, 28.0)
}

/// Prostokąt przycisku zasilania (skrajna prawa strona paska).
pub fn power_rect(bar: Rect, theme: &Theme) -> Rect {
    let cy = bar.center().y;
    let pad = theme.spacing.md;
    Rect::from_xywh(
        bar.right() - pad - POWER_BTN,
        cy - POWER_BTN / 2.0,
        POWER_BTN,
        POWER_BTN,
    )
}

/// Prostokąty pigułek przełączników (spójne z `paint_panel`), w kolejności `state.toggles`.
pub fn toggle_rects(bar: Rect, theme: &Theme, text: &TextEngine, state: &PanelState) -> Vec<Rect> {
    let t = theme;
    let cy = bar.center().y;
    let cap = t.typography.caption.size;
    let pad = t.spacing.md;
    // Cały klaster statusu kończy się przy prawej krawędzi paska.
    let mut right = bar.right() - pad;
    let mut rects = vec![Rect::ZERO; state.toggles.len()];
    for (i, toggle) in state.toggles.iter().enumerate().rev() {
        // Kompaktowy klaster: sama ikona (bez pigułki) i ciasne odstępy.
        let icon = 15.0;
        let vw = toggle
            .value
            .as_deref()
            .map(|v| t.spacing.xs + text.measure(v, cap, weight::MEDIUM).width)
            .unwrap_or(0.0);
        let w = 2.0 * t.spacing.xs + icon + vw;
        let pill = Rect::from_xywh(right - w, cy - 11.0, w, 22.0);
        rects[i] = pill;
        right = pill.x() - t.spacing.xs;
    }
    rects
}

/// Indeks przełącznika pod punktem `p`.
pub fn toggle_at(
    bar: Rect,
    theme: &Theme,
    text: &TextEngine,
    state: &PanelState,
    p: Point,
) -> Option<usize> {
    if p.y < bar.y() || p.y > bar.bottom() {
        return None;
    }
    toggle_rects(bar, theme, text, state)
        .iter()
        .position(|r| p.x >= r.x() - 4.0 && p.x <= r.right() + 4.0)
}

/// Prostokąty kropek przestrzeni roboczych (spójne z `paint_panel`), w kolejności `state.workspaces`.
pub fn workspace_rects(
    bar: Rect,
    theme: &Theme,
    text: &TextEngine,
    state: &PanelState,
) -> Vec<Rect> {
    let t = theme;
    let cy = bar.center().y;
    let d = 7.0;
    let toggles = toggle_rects(bar, t, text, state);
    // Kropki leżą **na lewo** od całej grupy przełączników — przełączniki są układane
    // od prawej, więc pierwszy z nich (`first()`) jest skrajnie lewy.
    let mut right = toggles
        .first()
        .map(|r| r.x() - t.spacing.xs)
        .unwrap_or(bar.right() - t.spacing.md);
    let mut rects = vec![Rect::ZERO; state.workspaces];
    for i in (0..state.workspaces).rev() {
        let dot = Rect::from_xywh(right - d, cy - d / 2.0, d, d);
        rects[i] = dot;
        right = dot.x() - t.spacing.xs;
    }
    rects
}

/// Indeks kropki przestrzeni roboczej pod punktem `p`.
pub fn workspace_at(
    bar: Rect,
    theme: &Theme,
    text: &TextEngine,
    state: &PanelState,
    p: Point,
) -> Option<usize> {
    if p.y < bar.y() || p.y > bar.bottom() {
        return None;
    }
    workspace_rects(bar, theme, text, state)
        .iter()
        .position(|r| p.x >= r.x() - 4.0 && p.x <= r.right() + 4.0)
}

/// Aktywny popup lub menu na panelu.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ActivePopup {
    #[default]
    None,
    Launcher,
    /// Lista sieci Wi-Fi — jedyne miejsce wyboru sieci (otwierane z Quick Panela).
    Wifi,
    Calendar,
    /// Menu kontekstowe przycisku okna w taskbarze (PPM).
    TaskMenu,
    /// Wspólny panel sterowania: przełączniki, dźwięk, jasność, media i sesja.
    Quick,
    /// Lista ukrytych ikon system traya (overflow).
    Tray,
    /// Menu kontekstowe pozycji tray’a (DBusMenu).
    TrayContext,
}

/// Prostokąt sekcji zegara na środku paska.
pub fn clock_rect(bar: Rect, theme: &Theme, text: &TextEngine, state: &PanelState) -> Rect {
    let body = theme.typography.body.size;
    let clock = format!("{}   ·   {}", state.clock, state.date);
    let cw = text.measure(&clock, body, weight::MEDIUM).width;
    let pad = theme.spacing.md;
    Rect::from_xywh(
        bar.center().x - cw / 2.0 - pad,
        bar.center().y - 14.0,
        cw + 2.0 * pad,
        28.0,
    )
}

/// Prostokąt etykiety użytkownika na pasku.
pub fn user_rect(bar: Rect, theme: &Theme, text: &TextEngine, state: &PanelState) -> Rect {
    let power = power_rect(bar, theme);
    let uw = text
        .measure(&state.user, theme.typography.body.size, weight::NORMAL)
        .width;
    let right = power.x() - theme.spacing.sm;
    Rect::from_xywh(
        right - uw - theme.spacing.md,
        bar.center().y - 14.0,
        uw + theme.spacing.md,
        28.0,
    )
}

/// Prostokąt rozwijanego menu zasilania (pod przyciskiem, wyrównany do prawej).
pub fn power_menu_rect(bar: Rect, theme: &Theme, items: usize) -> Rect {
    let p = power_rect(bar, theme);
    let pad = 6.0;
    let h = items as f64 * POWER_ITEM_H + 2.0 * pad;
    Rect::from_xywh(
        p.right() - POWER_MENU_W,
        bar.bottom() + pad,
        POWER_MENU_W,
        h,
    )
}

/// Indeks pozycji menu zasilania pod punktem `p`.
pub fn power_item_at(bar: Rect, theme: &Theme, items: usize, p: Point) -> Option<usize> {
    let menu = power_menu_rect(bar, theme, items);
    let inner = Rect::from_xywh(
        menu.x() + 6.0,
        menu.y() + 6.0,
        menu.width() - 12.0,
        items as f64 * POWER_ITEM_H,
    );
    if !inner.contains(p) {
        return None;
    }
    let i = ((p.y - inner.y()) / POWER_ITEM_H).floor() as usize;
    (i < items).then_some(i)
}

/// Rysuje menu zasilania z podanymi etykietami (spójne z [`power_item_at`]).
pub fn paint_power_menu(ctx: &mut PaintCtx, bar: Rect, labels: &[&str]) {
    let t = ctx.theme;
    let menu = power_menu_rect(bar, t, labels.len());
    ctx.scene
        .fill_rounded_rect(menu, t.radius.medium, t.colors.card_bg);
    ctx.scene
        .stroke_rounded_rect(menu, t.radius.medium, 1.0, t.colors.border);
    let x = menu.x() + 6.0;
    let y = menu.y() + 6.0;
    let w = menu.width() - 12.0;
    for (i, label) in labels.iter().enumerate() {
        let row = Rect::from_xywh(x, y + i as f64 * POWER_ITEM_H, w, POWER_ITEM_H);
        ctx.label_left(
            row.x() + 10.0,
            row.center().y,
            label,
            t.typography.body.size,
            weight::NORMAL,
            t.colors.text,
        );
    }
}

/// Prostokąt rozwijanego menu sesji (pod przyciskiem zasilania i użytkownikiem).
pub fn session_menu_rect(bar: Rect, theme: &Theme) -> Rect {
    let p = power_rect(bar, theme);
    let pad = 6.0;
    let w = 210.0;
    let h = crate::power::ITEM_COUNT as f64 * POWER_ITEM_H + 2.0 * pad;
    Rect::from_xywh(p.right() - w, bar.bottom() + pad, w, h)
}

/// Wykrywa kliknięcie w pozycję menu sesji.
pub fn session_item_at(bar: Rect, theme: &Theme, p: Point) -> Option<PowerAction> {
    let items = crate::power::items();
    let menu = session_menu_rect(bar, theme);
    let inner = Rect::from_xywh(
        menu.x() + 6.0,
        menu.y() + 6.0,
        menu.width() - 12.0,
        items.len() as f64 * POWER_ITEM_H,
    );
    if !inner.contains(p) {
        return None;
    }
    let i = ((p.y - inner.y()) / POWER_ITEM_H).floor() as usize;
    items.get(i).map(|item| item.action)
}

/// Rysuje menu sesji (zablokuj, wyloguj, restart, wyłącz).
pub fn paint_session_menu(
    ctx: &mut PaintCtx,
    bar: Rect,
    items: &[crate::power::PowerItem],
    hover: &Hover,
) {
    let t = ctx.theme;
    let menu = session_menu_rect(bar, t);
    ctx.scene
        .fill_rounded_rect(menu, t.radius.medium, t.colors.card_bg);
    ctx.scene
        .stroke_rounded_rect(menu, t.radius.medium, 1.0, t.colors.border);
    let x = menu.x() + 6.0;
    let y = menu.y() + 6.0;
    let w = menu.width() - 12.0;
    for (i, item) in items.iter().enumerate() {
        let row = Rect::from_xywh(x, y + i as f64 * POWER_ITEM_H, w, POWER_ITEM_H);
        if hover.is_hovered(row) {
            ctx.scene
                .fill_rounded_rect(row, t.radius.small, libluna_widgets::hover_fill(t));
        }
        let icon_name = match item.action {
            PowerAction::Settings => IconName::Settings,
            PowerAction::Lock => IconName::Star,
            PowerAction::Logout => IconName::User,
            PowerAction::Reboot => IconName::Clock,
            PowerAction::PowerOff => IconName::Power,
        };
        let isz = 18.0;
        let icon_rect = Rect::from_xywh(row.x() + 8.0, row.center().y - isz / 2.0, isz, isz);
        panel_icon(ctx, icon_name, icon_rect, t.colors.text);

        ctx.label_left(
            icon_rect.right() + 10.0,
            row.center().y,
            &item.label,
            t.typography.body.size,
            weight::NORMAL,
            t.colors.text,
        );
    }
}

/// ── Quick Panel (motyw + Wi-Fi + dźwięk + jasność) ──

/// Bok ikony statusu/paska (spójny dla burgera, Quick Panelu i przełączników).
pub const PANEL_ICON: f64 = 16.0;

/// Ikony paska są **monochromatyczne** (kolor z motywu).
///
/// Pastelowe ilustracje i zewnętrzne zestawy PNG ignorują kolor, więc w jasnym
/// motywie giną na jasnym tle — a status ma być czytelny w obu trybach. Ikony
/// aplikacji (taskbar, launcher) zostają kolorowe, bo identyfikują program.
fn panel_icon(ctx: &mut PaintCtx, name: IconName, rect: Rect, color: libluna_core::Color) {
    paint_icon_themed(ctx, name, rect, color, libluna_core::IconTheme::Monochrome);
}

/// Prostokąt przycisku otwierającego Quick Panel (na lewo od kropek i przełączników).
pub fn quick_button_rect(bar: Rect, theme: &Theme, text: &TextEngine, state: &PanelState) -> Rect {
    let left = workspace_rects(bar, theme, text, state)
        .first()
        .map(|r| r.x())
        .or_else(|| toggle_rects(bar, theme, text, state).first().map(|r| r.x()))
        .unwrap_or(bar.center().x);
    Rect::from_xywh(
        left - theme.spacing.sm - 24.0,
        bar.center().y - 12.0,
        24.0,
        24.0,
    )
}

// ── System tray (StatusNotifierItem) ──

/// Bok ikony tray’a.
pub const TRAY_ICON: f64 = 22.0;
/// Odstęp między ikonami tray’a.
const TRAY_GAP: f64 = 4.0;
/// Ile ikon mieści się przed przyciskiem Quick, zanim pojawi się overflow.
pub const TRAY_VISIBLE_MAX: usize = 4;
/// Wysokość wiersza w menu overflow.
const TRAY_ROW_H: f64 = 34.0;

/// Rozmieszczenie widocznych ikon tray’a i przycisku overflow.
pub struct TrayLayout {
    /// `(prostokąt, indeks w `PanelState::tray`)`, od lewej do prawej.
    pub icons: Vec<(Rect, usize)>,
    /// Przycisk „więcej” — tylko gdy pozycji jest więcej niż [`TRAY_VISIBLE_MAX`].
    pub overflow: Option<Rect>,
    /// Indeks pierwszej pozycji ukrytej w overflow.
    pub hidden_from: usize,
}

fn tray_hidden_from(total: usize) -> usize {
    if total > TRAY_VISIBLE_MAX {
        TRAY_VISIBLE_MAX - 1
    } else {
        total
    }
}

/// Ikony tray’a leżą bezpośrednio na lewo od przycisku Quick.
pub fn tray_layout(bar: Rect, theme: &Theme, text: &TextEngine, state: &PanelState) -> TrayLayout {
    let right = quick_button_rect(bar, theme, text, state).x() - theme.spacing.md;
    let cy = bar.center().y;
    let total = state.tray.len();
    let overflow = total > TRAY_VISIBLE_MAX;
    let visible = tray_hidden_from(total);
    let mut icons = Vec::with_capacity(visible);
    let mut x = right;
    let overflow_rect = if overflow {
        let r = Rect::from_xywh(
            right - TRAY_ICON,
            cy - TRAY_ICON / 2.0,
            TRAY_ICON,
            TRAY_ICON,
        );
        x = r.x() - TRAY_GAP;
        Some(r)
    } else {
        None
    };
    for i in (0..visible).rev() {
        let r = Rect::from_xywh(x - TRAY_ICON, cy - TRAY_ICON / 2.0, TRAY_ICON, TRAY_ICON);
        icons.push((r, i));
        x = r.x() - TRAY_GAP;
    }
    icons.sort_by(|a, b| a.0.x().total_cmp(&b.0.x()));
    TrayLayout {
        icons,
        overflow: overflow_rect,
        hidden_from: visible,
    }
}

/// Trafienie w tray.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayHit {
    /// Widoczna ikona o indeksie w `PanelState::tray`.
    Icon(usize),
    /// Przycisk rozwinięcia ukrytych ikon.
    Overflow,
}

pub fn tray_at(
    bar: Rect,
    theme: &Theme,
    text: &TextEngine,
    state: &PanelState,
    p: Point,
) -> Option<TrayHit> {
    if p.y < bar.y() || p.y > bar.bottom() {
        return None;
    }
    let l = tray_layout(bar, theme, text, state);
    if let Some(r) = l.overflow {
        if r.contains(p) {
            return Some(TrayHit::Overflow);
        }
    }
    l.icons
        .iter()
        .find(|(r, _)| r.contains(p))
        .map(|(_, i)| TrayHit::Icon(*i))
}

/// Prostokąt menu overflow (lista ukrytych ikon).
pub fn tray_menu_rect(bar: Rect, theme: &Theme, text: &TextEngine, state: &PanelState) -> Rect {
    let l = tray_layout(bar, theme, text, state);
    let Some(anchor) = l.overflow.or_else(|| l.icons.last().map(|(r, _)| *r)) else {
        return Rect::ZERO;
    };
    let hidden = state.tray.len().saturating_sub(l.hidden_from);
    let rows = hidden.min(8) as f64;
    let w = 260.0;
    let h = 12.0 + rows * TRAY_ROW_H;
    Rect::from_xywh(
        (anchor.right() - w).max(bar.x() + 8.0),
        bar.bottom() + 6.0,
        w,
        h,
    )
}

/// Indeks pozycji tray’a pod punktem w menu overflow.
pub fn tray_menu_hit(
    bar: Rect,
    theme: &Theme,
    text: &TextEngine,
    state: &PanelState,
    p: Point,
) -> Option<usize> {
    let rect = tray_menu_rect(bar, theme, text, state);
    if !rect.contains(p) {
        return None;
    }
    let hidden_from = tray_hidden_from(state.tray.len());
    let hidden = state.tray.len().saturating_sub(hidden_from).min(8);
    let inner = Rect::from_xywh(
        rect.x() + 6.0,
        rect.y() + 6.0,
        rect.width() - 12.0,
        hidden as f64 * TRAY_ROW_H,
    );
    if !inner.contains(p) {
        return None;
    }
    let row = ((p.y - inner.y()) / TRAY_ROW_H).floor() as usize;
    (row < hidden).then_some(hidden_from + row)
}

/// Rysuje ikonę tray’a (nazwana z motywu, pikselowa albo zapasowa).
fn paint_tray_icon(ctx: &mut PaintCtx, icon: &TrayIcon, rect: Rect) {
    match icon {
        TrayIcon::Named(name) => {
            if !paint_named_icon(ctx, Some(name), rect) {
                panel_icon(ctx, IconName::Info, rect, ctx.theme.colors.text);
            }
        }
        TrayIcon::Pixmap(image) => {
            ctx.scene.image(rect, image.clone());
        }
        TrayIcon::None => panel_icon(ctx, IconName::Info, rect, ctx.theme.colors.text),
    }
}

fn paint_tray_item(ctx: &mut PaintCtx, item: &TrayItem, rect: Rect, hovered: bool) {
    if hovered {
        ctx.scene.fill_rounded_rect(
            rect,
            ctx.theme.radius.small,
            libluna_widgets::hover_fill(ctx.theme),
        );
    }
    let inner = Rect::from_xywh(
        rect.x() + 2.0,
        rect.y() + 2.0,
        rect.width() - 4.0,
        rect.height() - 4.0,
    );
    paint_tray_icon(ctx, &item.icon, inner);
    if item.status == TrayStatus::NeedsAttention {
        let dot = Rect::from_xywh(rect.right() - 6.0, rect.bottom() - 6.0, 5.0, 5.0);
        ctx.scene
            .fill_rounded_rect(dot, 2.5, ctx.theme.colors.accent);
    }
}

/// Rysuje menu overflow z ukrytymi ikonami.
pub fn paint_tray_menu(ctx: &mut PaintCtx, rect: Rect, state: &PanelState, hover: Option<usize>) {
    let t = ctx.theme;
    ctx.scene
        .fill_rounded_rect(rect, t.radius.medium, t.colors.card_bg);
    ctx.scene
        .stroke_rounded_rect(rect, t.radius.medium, 1.0, t.colors.border);
    let hidden_from = tray_hidden_from(state.tray.len());
    let x = rect.x() + 6.0;
    let y = rect.y() + 6.0;
    let w = rect.width() - 12.0;
    for (row, item) in state.tray.iter().skip(hidden_from).take(8).enumerate() {
        let r = Rect::from_xywh(x, y + row as f64 * TRAY_ROW_H, w, TRAY_ROW_H);
        if hover == Some(hidden_from + row) {
            ctx.scene
                .fill_rounded_rect(r, t.radius.small, libluna_widgets::hover_fill(t));
        }
        let icon = Rect::from_xywh(r.x() + 4.0, r.center().y - 9.0, 18.0, 18.0);
        paint_tray_icon(ctx, &item.icon, icon);
        let label = ellipsize(
            item.label(),
            r.width() - 44.0,
            t.typography.body.size,
            weight::NORMAL,
            ctx.text,
        );
        ctx.label_left(
            icon.right() + 10.0,
            r.center().y,
            &label,
            t.typography.body.size,
            weight::NORMAL,
            t.colors.text,
        );
        if item.status == TrayStatus::NeedsAttention {
            let dot = Rect::from_xywh(r.right() - 10.0, r.center().y - 2.5, 5.0, 5.0);
            ctx.scene.fill_rounded_rect(dot, 2.5, t.colors.accent);
        }
    }
}

// ── Menu kontekstowe tray’a (DBusMenu) ──

const TRAY_MENU_ROW_H: f64 = 30.0;
const TRAY_MENU_SEPARATOR_H: f64 = 9.0;
const TRAY_MENU_W: f64 = 250.0;

fn tray_menu_row_h(row: &TrayMenuRow) -> f64 {
    if row.separator {
        TRAY_MENU_SEPARATOR_H
    } else {
        TRAY_MENU_ROW_H
    }
}

/// Prostokąt menu kontekstowego pozycji tray’a.
pub fn tray_context_rect(anchor: Rect, rows: &[TrayMenuRow], theme: &Theme) -> Rect {
    let pad = theme.spacing.sm;
    let h = 2.0 * pad + rows.iter().map(tray_menu_row_h).sum::<f64>();
    Rect::from_xywh(
        (anchor.right() - TRAY_MENU_W).max(8.0),
        anchor.bottom() + 6.0,
        TRAY_MENU_W,
        h,
    )
}

/// Indeks wiersza menu kontekstowego pod punktem `p`.
pub fn tray_context_hit(rect: Rect, rows: &[TrayMenuRow], p: Point) -> Option<usize> {
    if !rect.contains(p) {
        return None;
    }
    let mut y = rect.y() + 6.0;
    for (i, row) in rows.iter().enumerate() {
        let h = tray_menu_row_h(row);
        let r = Rect::from_xywh(rect.x() + 6.0, y, rect.width() - 12.0, h);
        if r.contains(p) {
            return (row.enabled && !row.separator).then_some(i);
        }
        y += h;
    }
    None
}

/// Rysuje menu kontekstowe tray’a.
pub fn paint_tray_context_menu(
    ctx: &mut PaintCtx,
    rect: Rect,
    rows: &[TrayMenuRow],
    hover: Option<usize>,
) {
    let t = ctx.theme;
    ctx.scene
        .fill_rounded_rect(rect, t.radius.medium, t.colors.card_bg);
    ctx.scene
        .stroke_rounded_rect(rect, t.radius.medium, 1.0, t.colors.border);
    let mut y = rect.y() + 6.0;
    for (i, row) in rows.iter().enumerate() {
        let h = tray_menu_row_h(row);
        let r = Rect::from_xywh(rect.x() + 6.0, y, rect.width() - 12.0, h);
        if row.separator {
            ctx.scene.fill_rect(
                Rect::from_xywh(r.x() + 8.0, r.center().y, r.width() - 16.0, 1.0),
                t.colors.border,
            );
            y += h;
            continue;
        }
        if hover == Some(i) {
            ctx.scene
                .fill_rounded_rect(r, t.radius.small, libluna_widgets::hover_fill(t));
        }
        let indent = 10.0 + row.depth as f64 * 14.0;
        let color = if row.enabled {
            t.colors.text
        } else {
            t.colors.dim_text
        };
        let label_w = r.width() - indent - 26.0;
        let label = ellipsize(
            &row.label,
            label_w,
            t.typography.body.size,
            weight::NORMAL,
            ctx.text,
        );
        ctx.label_left(
            r.x() + indent,
            r.center().y,
            &label,
            t.typography.body.size,
            weight::NORMAL,
            color,
        );
        if row.submenu {
            panel_icon(
                ctx,
                IconName::ChevronRight,
                Rect::from_xywh(r.right() - 20.0, r.center().y - 7.0, 14.0, 14.0),
                t.colors.dim_text,
            );
        }
        y += h;
    }
}

/// Szerokość panelu szybkich ustawień.
pub const QUICK_W: f64 = 400.0;
/// Wysokość panelu szybkich ustawień.
pub const QUICK_H: f64 = 492.0;

/// Prostokąt panelu szybkich ustawień.
pub fn quick_panel_rect(bar: Rect, theme: &Theme) -> Rect {
    let right = bar.right() - theme.spacing.md;
    Rect::from_xywh(
        (right - QUICK_W).max(bar.x() + 8.0),
        bar.bottom() + 6.0,
        QUICK_W,
        QUICK_H,
    )
}

/// Rozmieszczenie elementów Quick Panelu (spójne dla rysowania i trafień).
pub struct QuickLayout {
    pub header: Rect,
    pub avatar: Rect,
    /// `[0]` = przełącznik jasny/ciemny, `[1]` = Ustawienia systemu.
    pub header_buttons: [Rect; 2],
    /// Wi-Fi, Bluetooth, Samolot, Nie przeszkadzać, Motyw, Zrzut ekranu.
    pub tiles: [Rect; 6],
    pub settings: Rect,
    pub mute: Rect,
    pub volume: Rect,
    pub brightness: Rect,
    pub media: Rect,
    pub media_art: Rect,
    /// Poprzedni, play/pauza, następny.
    pub media_buttons: [Rect; 3],
    /// Zablokuj ekran, wyloguj, uruchom ponownie, wyłącz komputer.
    pub session: [Rect; 4],
}

pub fn quick_layout(rect: Rect, theme: &Theme) -> QuickLayout {
    let pad = theme.spacing.lg;
    let x = rect.x() + pad;
    let w = rect.width() - 2.0 * pad;
    let mut y = rect.y() + pad;

    let header = Rect::from_xywh(x, y, w, 44.0);
    let avatar = Rect::from_xywh(x, y, 44.0, 44.0);
    let settings_btn = Rect::from_xywh(x + w - 36.0, y + 4.0, 36.0, 36.0);
    let theme_btn = Rect::from_xywh(settings_btn.x() - 8.0 - 36.0, y + 4.0, 36.0, 36.0);
    y = header.bottom() + 14.0;

    let gap = 8.0;
    let tw = (w - 2.0 * gap) / 3.0;
    let th = 66.0;
    let mut tiles = [Rect::ZERO; 6];
    for row in 0..2 {
        for col in 0..3 {
            tiles[row * 3 + col] = Rect::from_xywh(
                x + col as f64 * (tw + gap),
                y + row as f64 * (th + gap),
                tw,
                th,
            );
        }
    }
    y += 2.0 * th + gap + 14.0;

    let settings = Rect::from_xywh(x, y, w, 108.0);
    let inset = 12.0;
    let track = Rect::from_xywh(
        settings.x() + inset,
        settings.y() + 34.0,
        settings.width() - 2.0 * inset,
        6.0,
    );
    let mute = Rect::from_xywh(
        settings.right() - inset - 18.0,
        settings.y() + 9.0,
        18.0,
        18.0,
    );
    let brightness = Rect::from_xywh(
        settings.x() + inset,
        settings.y() + 90.0,
        settings.width() - 2.0 * inset,
        6.0,
    );
    y = settings.bottom() + 12.0;

    let media = Rect::from_xywh(x, y, w, 72.0);
    let art = Rect::from_xywh(media.x() + 12.0, media.center().y - 24.0, 48.0, 48.0);
    let next = Rect::from_xywh(
        media.right() - 12.0 - 22.0,
        media.center().y - 11.0,
        22.0,
        22.0,
    );
    let play = Rect::from_xywh(next.x() - 8.0 - 32.0, media.center().y - 16.0, 32.0, 32.0);
    let prev = Rect::from_xywh(play.x() - 8.0 - 22.0, media.center().y - 11.0, 22.0, 22.0);
    y = media.bottom() + 12.0;

    let sw = (w - 3.0 * gap) / 4.0;
    let session = [
        Rect::from_xywh(x, y, sw, 40.0),
        Rect::from_xywh(x + sw + gap, y, sw, 40.0),
        Rect::from_xywh(x + 2.0 * (sw + gap), y, sw, 40.0),
        Rect::from_xywh(x + 3.0 * (sw + gap), y, sw, 40.0),
    ];

    QuickLayout {
        header,
        avatar,
        header_buttons: [theme_btn, settings_btn],
        tiles,
        settings,
        mute,
        volume: track,
        brightness,
        media,
        media_art: art,
        media_buttons: [prev, play, next],
        session,
    }
}

/// Trafienie w Quick Panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuickHit {
    /// Przełącznik motywu w nagłówku (jasny → ciemny → auto).
    Theme,
    /// Skrót do Ustawień systemu w nagłówku.
    Settings,
    Wifi,
    Bluetooth,
    Airplane,
    Dnd,
    /// Ciepła korekta gamma (night light).
    NightLight,
    /// Oszczędzanie energii.
    PowerSaver,
    Mute,
    Volume(u8),
    Brightness(u8),
    MediaPrevious,
    MediaPlayPause,
    MediaNext,
    Lock,
    Logout,
    Reboot,
    PowerOff,
}

fn slider_value(track: Rect, p: Point) -> Option<u8> {
    let r = Rect::from_xywh(
        track.x() - 6.0,
        track.y() - 9.0,
        track.width() + 12.0,
        track.height() + 18.0,
    );
    if !r.contains(p) {
        return None;
    }
    let f = ((p.x - track.x()) / track.width().max(1.0)).clamp(0.0, 1.0);
    Some((f * 100.0).round() as u8)
}

pub fn quick_hit(rect: Rect, theme: &Theme, p: Point) -> Option<QuickHit> {
    let l = quick_layout(rect, theme);
    if l.header_buttons[0].contains(p) {
        return Some(QuickHit::Theme);
    }
    if l.header_buttons[1].contains(p) {
        return Some(QuickHit::Settings);
    }
    let tile_hits = [
        QuickHit::Wifi,
        QuickHit::Bluetooth,
        QuickHit::Airplane,
        QuickHit::Dnd,
        QuickHit::NightLight,
        QuickHit::PowerSaver,
    ];
    for (r, hit) in l.tiles.iter().zip(tile_hits) {
        if r.contains(p) {
            return Some(hit);
        }
    }
    if l.mute.contains(p) {
        return Some(QuickHit::Mute);
    }
    if let Some(v) = slider_value(l.volume, p) {
        return Some(QuickHit::Volume(v));
    }
    if let Some(v) = slider_value(l.brightness, p) {
        return Some(QuickHit::Brightness(v));
    }
    if l.media_buttons[0].contains(p) {
        return Some(QuickHit::MediaPrevious);
    }
    if l.media_buttons[1].contains(p) {
        return Some(QuickHit::MediaPlayPause);
    }
    if l.media_buttons[2].contains(p) {
        return Some(QuickHit::MediaNext);
    }
    if l.session[0].contains(p) {
        return Some(QuickHit::Lock);
    }
    if l.session[1].contains(p) {
        return Some(QuickHit::Logout);
    }
    if l.session[2].contains(p) {
        return Some(QuickHit::Reboot);
    }
    if l.session[3].contains(p) {
        return Some(QuickHit::PowerOff);
    }
    None
}

fn paint_thin_slider(ctx: &mut PaintCtx, track: Rect, pct: u8, enabled: bool, hovered: bool) {
    let t = ctx.theme;
    let radius = track.height() / 2.0;
    ctx.scene.fill_rounded_rect(
        track,
        radius,
        if hovered {
            libluna_widgets::hover_fill(t)
        } else {
            t.colors.view_bg
        },
    );
    let fill_w = (track.width() * (pct as f64 * 0.01)).clamp(0.0, track.width());
    if fill_w > 0.0 {
        let fill = Rect::from_xywh(track.x(), track.y(), fill_w, track.height());
        let col = if enabled {
            t.colors.accent
        } else {
            t.colors.dim_text
        };
        ctx.scene.fill_rounded_rect(fill, radius, col);
    }
    if !enabled {
        return;
    }
    let r = 7.0;
    let thumb_x = (track.x() + fill_w).clamp(track.x() + r, track.right() - r);
    let thumb = Rect::from_xywh(thumb_x - r, track.center().y - r, 2.0 * r, 2.0 * r);
    ctx.scene.fill_rounded_rect(thumb, r, Color::WHITE);
    ctx.scene
        .stroke_rounded_rect(thumb, r, 1.0, t.colors.border);
}

/// Największy rozmiar czcionki (od `start` w dół), przy którym tekst mieści się w `max_w`.
fn fitted_size(
    text: &str,
    max_w: f64,
    start: f64,
    min: f64,
    weight: u16,
    engine: &TextEngine,
) -> f64 {
    let mut size = start;
    while size > min && engine.measure(text, size, weight).width > max_w {
        size -= 0.5;
    }
    size
}

/// Jeden kafelek Quick Panelu (tytuł, opcjonalna druga linia, podtytuł).
fn paint_quick_tile(
    ctx: &mut PaintCtx,
    rect: Rect,
    title: &str,
    second: Option<&str>,
    subtitle: &str,
    active: bool,
    hovered: bool,
) {
    let t = ctx.theme;
    let (bg, fg, sub_fg) = if active {
        (
            t.colors.accent,
            t.colors.accent_fg,
            t.colors.accent_fg.scale_alpha(0.82),
        )
    } else {
        (
            if hovered {
                libluna_widgets::hover_fill(t)
            } else {
                t.colors.shade
            },
            t.colors.text,
            t.colors.dim_text,
        )
    };
    ctx.scene.fill_rounded_rect(rect, t.radius.medium, bg);
    if !active {
        ctx.scene.stroke_rounded_rect(
            rect,
            t.radius.medium,
            if hovered { 1.5 } else { 1.0 },
            if hovered {
                t.colors.accent.scale_alpha(0.55)
            } else {
                t.colors.border
            },
        );
    } else if hovered {
        // Aktywny kafelek ma już tło akcentu — hover zaznaczamy jaśniejszą obwódką.
        ctx.scene.stroke_rounded_rect(
            rect,
            t.radius.medium,
            2.0,
            t.colors.accent_fg.scale_alpha(0.65),
        );
    }

    let inner = Rect::from_xywh(
        rect.x() + 10.0,
        rect.y(),
        (rect.width() - 20.0).max(0.0),
        rect.height(),
    );
    ctx.scene.push_clip(inner);
    let title_sz = t.typography.body.size - 1.0;
    let sub_sz = t.typography.caption.size - 1.0;
    if let Some(second) = second {
        let size = fitted_size(
            title,
            inner.width(),
            title_sz - 1.0,
            10.0,
            weight::BOLD,
            ctx.text,
        );
        let size = fitted_size(second, inner.width(), size, 10.0, weight::BOLD, ctx.text);
        let first = ellipsize(title, inner.width(), size, weight::BOLD, ctx.text);
        let second = ellipsize(second, inner.width(), size, weight::BOLD, ctx.text);
        ctx.label_left(inner.x(), rect.y() + 16.0, &first, size, weight::BOLD, fg);
        ctx.label_left(inner.x(), rect.y() + 32.0, &second, size, weight::BOLD, fg);
        let sub_size = fitted_size(
            subtitle,
            inner.width(),
            sub_sz,
            9.5,
            weight::NORMAL,
            ctx.text,
        );
        let subtitle = ellipsize(subtitle, inner.width(), sub_size, weight::NORMAL, ctx.text);
        ctx.label_left(
            inner.x(),
            rect.y() + 52.0,
            &subtitle,
            sub_size,
            weight::NORMAL,
            sub_fg,
        );
    } else {
        let size = fitted_size(title, inner.width(), title_sz, 10.0, weight::BOLD, ctx.text);
        let sub_size = fitted_size(
            subtitle,
            inner.width(),
            sub_sz,
            9.5,
            weight::NORMAL,
            ctx.text,
        );
        let title = ellipsize(title, inner.width(), size, weight::BOLD, ctx.text);
        let subtitle = ellipsize(subtitle, inner.width(), sub_size, weight::NORMAL, ctx.text);
        ctx.label_left(inner.x(), rect.y() + 25.0, &title, size, weight::BOLD, fg);
        ctx.label_left(
            inner.x(),
            rect.y() + 46.0,
            &subtitle,
            sub_size,
            weight::NORMAL,
            sub_fg,
        );
    }
    ctx.scene.pop_clip();
}

/// Odtwarzacz pokazywany w Quick Panelu.
#[derive(Debug, Clone, Copy)]
pub struct QuickMedia<'a> {
    pub title: &'a str,
    pub artist: &'a str,
    pub playing: bool,
}

/// Dane do narysowania Quick Panelu.
pub struct QuickData<'a> {
    pub user: &'a str,
    pub preference: ThemePreference,
    /// Czy rozwiązany motyw jest ciemny (dla wyróżnienia kafelka „Motyw”).
    pub dark: bool,
    pub wifi_on: bool,
    pub ssid: Option<&'a str>,
    /// `None` = brak adaptera Bluetooth.
    pub bluetooth: Option<(bool, usize)>,
    pub airplane: bool,
    pub dnd: bool,
    /// Siła night lightu (0 = wyłączony).
    pub night_light: u8,
    /// Czy kompozytor ma sprzętową tablicę gamma.
    pub gamma_available: bool,
    pub power_saver: bool,
    /// Czy oszczędzanie działa przez limit kompozytora (brak `power-profiles-daemon`).
    pub power_fallback: bool,
    pub volume: u8,
    pub muted: bool,
    /// `None` = brak podświetlenia (np. komputer stacjonarny).
    pub brightness: Option<u8>,
    pub sink: Option<&'a str>,
    pub battery: Option<BatteryStatus>,
    pub media: Option<QuickMedia<'a>>,
}

/// Rysuje Quick Panel: profil, kafelki szybkich akcji, dźwięk/jasność, media i sesję.
pub fn paint_quick_panel(ctx: &mut PaintCtx, rect: Rect, data: &QuickData, hover: &Hover) {
    let t = ctx.theme;
    let l = quick_layout(rect, t);
    let hit = hover.position().and_then(|p| quick_hit(rect, t, p));
    ctx.scene
        .fill_rounded_rect(rect, t.radius.large, t.colors.card_bg);
    ctx.scene
        .stroke_rounded_rect(rect, t.radius.large, 1.0, t.colors.border);

    // ── Nagłówek: awatar, użytkownik, bateria, przyciski ──
    ctx.scene
        .fill_rounded_rect(l.avatar, l.avatar.width() / 2.0, t.colors.accent);
    let initial = data
        .user
        .chars()
        .next()
        .map(|c| c.to_uppercase().to_string())
        .unwrap_or_else(|| "U".to_owned());
    ctx.label_center(
        l.avatar.center().x,
        l.avatar.center().y,
        &initial,
        t.typography.heading.size,
        weight::BOLD,
        t.colors.accent_fg,
    );

    let text_x = l.avatar.right() + 12.0;
    let text_w = (l.header_buttons[0].x() - 8.0 - text_x).max(0.0);
    let name = ellipsize(
        data.user,
        text_w,
        t.typography.heading.size,
        weight::BOLD,
        ctx.text,
    );
    ctx.label_left(
        text_x,
        l.header.y() + 14.0,
        &name,
        t.typography.heading.size,
        weight::BOLD,
        t.colors.text,
    );

    let subtitle = match data.battery {
        None => tr("AC power"),
        Some(b) if b.charging => {
            tr_args("Battery {n}% · charging", &[("n", &b.percent.to_string())])
        }
        Some(b) => match b.minutes {
            Some(m) if m > 0 => tr_args(
                "Battery {n}% · {h} h {min} min",
                &[
                    ("n", &b.percent.to_string()),
                    ("h", &(m / 60).to_string()),
                    ("min", &(m % 60).to_string()),
                ],
            ),
            _ => tr_args("Battery {n}%", &[("n", &b.percent.to_string())]),
        },
    };
    let subtitle = ellipsize(
        &subtitle,
        text_w,
        t.typography.caption.size,
        weight::NORMAL,
        ctx.text,
    );
    ctx.label_left(
        text_x,
        l.header.y() + 32.0,
        &subtitle,
        t.typography.caption.size,
        weight::NORMAL,
        t.colors.dim_text,
    );

    {
        let header_hover = |i: usize, current: Option<QuickHit>| {
            let target = if i == 0 {
                QuickHit::Theme
            } else {
                QuickHit::Settings
            };
            if current == Some(target) {
                libluna_widgets::hover_fill(t)
            } else {
                t.colors.shade
            }
        };
        for (i, (r, icon)) in l
            .header_buttons
            .iter()
            .zip([
                match data.preference {
                    ThemePreference::Light => IconName::Brightness,
                    ThemePreference::Dark => IconName::Moon,
                    ThemePreference::System => IconName::Auto,
                },
                IconName::Settings,
            ])
            .enumerate()
        {
            ctx.scene
                .fill_rounded_rect(*r, t.radius.medium, header_hover(i, hit));
            let sz = 20.0;
            // Ciemny motyw zaznaczamy akcentem — od razu widać stan przełącznika.
            let fg = if i == 0 && data.dark {
                t.colors.accent
            } else {
                t.colors.text
            };
            panel_icon(
                ctx,
                icon,
                Rect::from_xywh(r.center().x - sz / 2.0, r.center().y - sz / 2.0, sz, sz),
                fg,
            );
        }
    }

    // ── Kafelki ──
    let bt_subtitle = match data.bluetooth {
        None => tr("No adapter"),
        Some((false, _)) => tr("Off"),
        Some((true, 0)) => tr("On"),
        Some((true, 1)) => tr_args("{n} device", &[("n", "1")]),
        Some((true, n @ 2..=4)) => tr_args("{n} devices", &[("n", &n.to_string())]),
        Some((true, n)) => tr_args("{n} devices", &[("n", &n.to_string())]),
    };
    let bt_active = matches!(data.bluetooth, Some((true, _)));
    let night_on = data.gamma_available && data.night_light > 0;
    let night_subtitle = if !data.gamma_available {
        tr("Unavailable")
    } else if night_on {
        tr_args("On · {n}%", &[("n", &data.night_light.to_string())])
    } else {
        tr("Off")
    };
    let power_subtitle = if data.power_saver {
        if data.power_fallback {
            tr("On · 30 FPS")
        } else {
            tr("On")
        }
    } else {
        tr("Off")
    };
    let on = tr("On");
    let off = tr("Off");
    let tiles: [(String, Option<String>, String, bool); 6] = [
        (
            "Wi-Fi".to_owned(),
            None,
            if data.wifi_on {
                data.ssid.unwrap_or(&on).to_owned()
            } else {
                off.clone()
            },
            data.wifi_on,
        ),
        ("Bluetooth".to_owned(), None, bt_subtitle, bt_active),
        (
            tr("Airplane"),
            None,
            if data.airplane {
                on.clone()
            } else {
                off.clone()
            },
            data.airplane,
        ),
        (
            tr("Do not"),
            Some(tr("disturb")),
            if data.dnd { on.clone() } else { off.clone() },
            data.dnd,
        ),
        (tr("Night light"), None, night_subtitle, night_on),
        (tr("Power saver"), None, power_subtitle, data.power_saver),
    ];
    let tile_hits = [
        QuickHit::Wifi,
        QuickHit::Bluetooth,
        QuickHit::Airplane,
        QuickHit::Dnd,
        QuickHit::NightLight,
        QuickHit::PowerSaver,
    ];
    for ((r, (title, second, subtitle, active)), target) in
        l.tiles.iter().zip(tiles.iter()).zip(tile_hits)
    {
        paint_quick_tile(
            ctx,
            *r,
            title,
            second.as_deref(),
            subtitle,
            *active,
            hit == Some(target),
        );
    }

    // ── Dźwięk i jasność ──
    ctx.scene
        .fill_rounded_rect(l.settings, t.radius.medium, t.colors.shade);
    ctx.label_left(
        l.settings.x() + 12.0,
        l.settings.y() + 18.0,
        &tr("Sound and volume"),
        t.typography.body.size,
        weight::BOLD,
        t.colors.text,
    );
    if hit == Some(QuickHit::Mute) {
        ctx.scene.fill_rounded_rect(
            Rect::from_xywh(l.mute.x() - 4.0, l.mute.y() - 4.0, 26.0, 26.0),
            6.0,
            libluna_widgets::hover_fill(t),
        );
    }
    panel_icon(
        ctx,
        IconName::Volume,
        Rect::from_xywh(l.mute.x() + 1.0, l.mute.y() + 1.0, 18.0, 18.0),
        if data.muted {
            t.colors.dim_text
        } else {
            t.colors.text
        },
    );
    ctx.label_right(
        l.mute.x() - 8.0,
        l.mute.center().y,
        &format!("{}%", data.volume),
        t.typography.caption.size,
        weight::MEDIUM,
        if data.muted {
            t.colors.dim_text
        } else {
            t.colors.text
        },
    );
    paint_thin_slider(
        ctx,
        l.volume,
        data.volume,
        !data.muted,
        matches!(hit, Some(QuickHit::Volume(_))),
    );

    let sink = match data.sink {
        Some(s) => tr_args("Output: {name}", &[("name", s)]),
        None => tr("Output: default"),
    };
    let sink = ellipsize(
        &sink,
        l.settings.width() - 24.0,
        t.typography.caption.size,
        weight::NORMAL,
        ctx.text,
    );
    ctx.label_left(
        l.settings.x() + 12.0,
        l.settings.y() + 58.0,
        &sink,
        t.typography.caption.size,
        weight::NORMAL,
        t.colors.dim_text,
    );

    let brightness = data.brightness;
    let brightness_fg = if brightness.is_some() {
        t.colors.text
    } else {
        t.colors.dim_text
    };
    ctx.label_left(
        l.settings.x() + 12.0,
        l.settings.y() + 78.0,
        &tr("Brightness"),
        t.typography.body.size,
        weight::BOLD,
        brightness_fg,
    );
    ctx.label_right(
        l.settings.right() - 12.0,
        l.settings.y() + 78.0,
        &brightness
            .map(|v| format!("{v}%"))
            .unwrap_or_else(|| "—".to_owned()),
        t.typography.caption.size,
        weight::MEDIUM,
        brightness_fg,
    );
    paint_thin_slider(
        ctx,
        l.brightness,
        brightness.unwrap_or(0),
        brightness.is_some(),
        matches!(hit, Some(QuickHit::Brightness(_))),
    );

    // ── Odtwarzacz ──
    ctx.scene
        .fill_rounded_rect(l.media, t.radius.medium, t.colors.shade);
    ctx.scene.fill_rounded_rect(
        l.media_art,
        t.radius.small,
        t.colors.accent.scale_alpha(0.18),
    );
    panel_icon(
        ctx,
        IconName::Music,
        Rect::from_xywh(
            l.media_art.center().x - 12.0,
            l.media_art.center().y - 12.0,
            24.0,
            24.0,
        ),
        t.colors.accent,
    );
    let has_media = data.media.is_some();
    let (title, artist) = match data.media {
        Some(m) => (m.title, m.artist),
        None => ("Nothing is playing", "Start a player"),
    };
    let title = if has_media {
        title.to_owned()
    } else {
        tr(title)
    };
    let artist = if has_media {
        artist.to_owned()
    } else {
        tr(artist)
    };
    let media_text_x = l.media_art.right() + 12.0;
    let media_text_w = (l.media_buttons[0].x() - 8.0 - media_text_x).max(0.0);
    let title = ellipsize(
        &title,
        media_text_w,
        t.typography.body.size,
        weight::MEDIUM,
        ctx.text,
    );
    let artist = ellipsize(
        &artist,
        media_text_w,
        t.typography.caption.size,
        weight::NORMAL,
        ctx.text,
    );
    ctx.label_left(
        media_text_x,
        l.media.y() + 28.0,
        &title,
        t.typography.body.size,
        weight::MEDIUM,
        if has_media {
            t.colors.text
        } else {
            t.colors.dim_text
        },
    );
    ctx.label_left(
        media_text_x,
        l.media.y() + 47.0,
        &artist,
        t.typography.caption.size,
        weight::NORMAL,
        t.colors.dim_text,
    );

    let icon_fg = if has_media {
        t.colors.text
    } else {
        t.colors.dim_text
    };
    for (i, (r, icon)) in [
        (l.media_buttons[0], IconName::ChevronLeft),
        (l.media_buttons[2], IconName::ChevronRight),
    ]
    .into_iter()
    .enumerate()
    {
        let hovered = hit
            == Some(if i == 0 {
                QuickHit::MediaPrevious
            } else {
                QuickHit::MediaNext
            });
        if hovered {
            ctx.scene
                .fill_rounded_rect(r, r.height() / 2.0, libluna_widgets::hover_fill(t));
        }
        panel_icon(
            ctx,
            icon,
            Rect::from_xywh(r.center().x - 10.0, r.center().y - 10.0, 20.0, 20.0),
            icon_fg,
        );
    }
    let play = l.media_buttons[1];
    ctx.scene.fill_rounded_rect(
        play,
        play.width() / 2.0,
        if has_media {
            t.colors.accent
        } else {
            t.colors.shade
        },
    );
    if hit == Some(QuickHit::MediaPlayPause) {
        ctx.scene
            .fill_rounded_rect(play, play.width() / 2.0, libluna_widgets::hover_fill(t));
    }
    let playing = data.media.map(|m| m.playing).unwrap_or(false);
    panel_icon(
        ctx,
        if playing {
            IconName::Pause
        } else {
            IconName::Play
        },
        Rect::from_xywh(play.center().x - 9.0, play.center().y - 9.0, 18.0, 18.0),
        if has_media {
            t.colors.accent_fg
        } else {
            t.colors.dim_text
        },
    );

    // ── Sesja ──
    for (i, (r, label)) in l
        .session
        .iter()
        .zip([tr("Lock"), tr("Log out"), tr("Reboot"), tr("Power off")])
        .enumerate()
    {
        let target = match i {
            0 => QuickHit::Lock,
            1 => QuickHit::Logout,
            2 => QuickHit::Reboot,
            _ => QuickHit::PowerOff,
        };
        ctx.scene.fill_rounded_rect(
            *r,
            t.radius.medium,
            if hit == Some(target) {
                libluna_widgets::hover_fill(t)
            } else {
                t.colors.shade
            },
        );
        let size = t.typography.caption.size - 1.0;
        let label = ellipsize(&label, r.width() - 8.0, size, weight::MEDIUM, ctx.text);
        ctx.label_center(
            r.center().x,
            r.center().y,
            &label,
            size,
            weight::MEDIUM,
            t.colors.text,
        );
    }
}

// ── Popup Dźwięk / Głośność ──

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VolumeAction {
    SetLevel(u8),
    StepDown,
    StepUp,
    ToggleMute,
}

pub fn volume_popup_rect(bar: Rect, theme: &Theme, text: &TextEngine, state: &PanelState) -> Rect {
    let w = 280.0;
    let h = 160.0;
    let pad = 6.0;
    let toggles = toggle_rects(bar, theme, text, state);
    let vol_idx = state
        .toggles
        .iter()
        .position(|t| t.action == ToggleAction::Volume);
    let right = match vol_idx.and_then(|i| toggles.get(i)) {
        Some(r) => r.right(),
        None => bar.right() - 80.0,
    };
    let x = (right - w).clamp(10.0, (bar.right() - w - 10.0).max(10.0));
    Rect::from_xywh(x, bar.bottom() + pad, w, h)
}

pub fn paint_volume_popup(ctx: &mut PaintCtx, rect: Rect, pct: u8, muted: bool, hover: &Hover) {
    let t = ctx.theme;
    let hit = hover.position().and_then(|p| volume_popup_hit(rect, t, p));
    ctx.scene
        .fill_rounded_rect(rect, t.radius.large, t.colors.card_bg);
    ctx.scene
        .stroke_rounded_rect(rect, t.radius.large, 1.0, t.colors.border);

    let pad = t.spacing.lg;
    let icon_sz = 18.0;
    let icon_rect = Rect::from_xywh(rect.x() + pad, rect.y() + pad, icon_sz, icon_sz);
    panel_icon(
        ctx,
        IconName::Volume,
        icon_rect,
        if muted {
            t.colors.dim_text
        } else {
            t.colors.accent
        },
    );

    ctx.label_left(
        icon_rect.right() + t.spacing.sm,
        icon_rect.center().y,
        &tr("Sound and volume"),
        t.typography.body.size,
        weight::BOLD,
        t.colors.text,
    );

    let status_str = if muted {
        tr_args("{n}% (muted)", &[("n", &pct.to_string())])
    } else {
        format!("{pct}%")
    };
    ctx.label_right(
        rect.right() - pad,
        icon_rect.center().y,
        &status_str,
        t.typography.caption.size,
        weight::MEDIUM,
        if muted {
            t.colors.dim_text
        } else {
            t.colors.text
        },
    );

    let slider_y = icon_rect.bottom() + t.spacing.lg;
    let slider_track = Rect::from_xywh(rect.x() + pad, slider_y, rect.width() - 2.0 * pad, 12.0);
    ctx.scene
        .fill_rounded_rect(slider_track, 6.0, t.colors.view_bg);
    ctx.scene
        .stroke_rounded_rect(slider_track, 6.0, 1.0, t.colors.border);
    if matches!(hit, Some(VolumeAction::SetLevel(_))) {
        ctx.scene
            .stroke_rounded_rect(slider_track, 6.0, 1.5, t.colors.accent);
    }

    let fill_w = (slider_track.width() * (pct as f64 / 100.0)).clamp(0.0, slider_track.width());
    if fill_w > 0.0 {
        let fill_rect = Rect::from_xywh(
            slider_track.x(),
            slider_track.y(),
            fill_w,
            slider_track.height(),
        );
        let fill_col = if muted {
            t.colors.dim_text
        } else {
            t.colors.accent
        };
        ctx.scene.fill_rounded_rect(fill_rect, 6.0, fill_col);
    }

    // Suwak na pasku głośności (thumb)
    let thumb_x =
        (slider_track.x() + fill_w).clamp(slider_track.x() + 8.0, slider_track.right() - 8.0);
    let thumb_rect = Rect::from_xywh(thumb_x - 8.0, slider_track.center().y - 8.0, 16.0, 16.0);
    ctx.scene.fill_rounded_rect(thumb_rect, 8.0, Color::WHITE);
    ctx.scene
        .stroke_rounded_rect(thumb_rect, 8.0, 1.5, t.colors.border);

    let btn_y = slider_track.bottom() + t.spacing.lg;
    let btn_h = 32.0;

    let minus_rect = Rect::from_xywh(rect.x() + pad, btn_y, 44.0, btn_h);
    ctx.scene.fill_rounded_rect(
        minus_rect,
        t.radius.small,
        if hit == Some(VolumeAction::StepDown) {
            libluna_widgets::hover_fill(t)
        } else {
            t.colors.shade
        },
    );
    ctx.scene
        .stroke_rounded_rect(minus_rect, t.radius.small, 1.0, t.colors.border);
    ctx.label_center(
        minus_rect.center().x,
        minus_rect.center().y,
        "−",
        t.typography.body.size,
        weight::BOLD,
        t.colors.text,
    );

    let plus_rect = Rect::from_xywh(minus_rect.right() + t.spacing.sm, btn_y, 44.0, btn_h);
    ctx.scene.fill_rounded_rect(
        plus_rect,
        t.radius.small,
        if hit == Some(VolumeAction::StepUp) {
            libluna_widgets::hover_fill(t)
        } else {
            t.colors.shade
        },
    );
    ctx.scene
        .stroke_rounded_rect(plus_rect, t.radius.small, 1.0, t.colors.border);
    ctx.label_center(
        plus_rect.center().x,
        plus_rect.center().y,
        "+",
        t.typography.body.size,
        weight::BOLD,
        t.colors.text,
    );

    let mute_w = rect.right() - pad - (plus_rect.right() + t.spacing.md);
    let mute_rect = Rect::from_xywh(plus_rect.right() + t.spacing.md, btn_y, mute_w, btn_h);
    let (mbg, mfg, mlabel) = if muted {
        (t.colors.accent, t.colors.accent_fg, tr("Unmute"))
    } else if hit == Some(VolumeAction::ToggleMute) {
        (libluna_widgets::hover_fill(t), t.colors.text, tr("Mute"))
    } else {
        (t.colors.shade, t.colors.text, tr("Mute"))
    };
    ctx.scene.fill_rounded_rect(mute_rect, t.radius.small, mbg);
    if !muted {
        ctx.scene
            .stroke_rounded_rect(mute_rect, t.radius.small, 1.0, t.colors.border);
    }
    ctx.label_center(
        mute_rect.center().x,
        mute_rect.center().y,
        &mlabel,
        t.typography.caption.size,
        weight::MEDIUM,
        mfg,
    );
}

pub fn volume_popup_hit(rect: Rect, theme: &Theme, p: Point) -> Option<VolumeAction> {
    if !rect.contains(p) {
        return None;
    }
    let pad = theme.spacing.lg;
    let icon_sz = 18.0;
    let slider_y = rect.y() + pad + icon_sz + theme.spacing.lg;
    let slider_hit = Rect::from_xywh(
        rect.x() + pad,
        slider_y - 6.0,
        rect.width() - 2.0 * pad,
        24.0,
    );
    if slider_hit.contains(p) {
        let frac = ((p.x - slider_hit.x()) / slider_hit.width()).clamp(0.0, 1.0);
        return Some(VolumeAction::SetLevel((frac * 100.0).round() as u8));
    }

    let btn_y = slider_y + 12.0 + theme.spacing.lg;
    let btn_h = 32.0;
    let minus_rect = Rect::from_xywh(rect.x() + pad, btn_y, 44.0, btn_h);
    if minus_rect.contains(p) {
        return Some(VolumeAction::StepDown);
    }

    let plus_rect = Rect::from_xywh(minus_rect.right() + theme.spacing.sm, btn_y, 44.0, btn_h);
    if plus_rect.contains(p) {
        return Some(VolumeAction::StepUp);
    }

    let mute_w = rect.right() - pad - (plus_rect.right() + theme.spacing.md);
    let mute_rect = Rect::from_xywh(plus_rect.right() + theme.spacing.md, btn_y, mute_w, btn_h);
    if mute_rect.contains(p) {
        return Some(VolumeAction::ToggleMute);
    }

    None
}

// ── Popup Sieć / Wi-Fi ──

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WifiAction {
    TogglePower,
    SelectNetwork(String),
}

pub fn wifi_popup_rect(bar: Rect, theme: &Theme, text: &TextEngine, state: &PanelState) -> Rect {
    let w = 290.0;
    let h = 270.0;
    let pad = 6.0;
    let toggles = toggle_rects(bar, theme, text, state);
    let wifi_idx = state
        .toggles
        .iter()
        .position(|t| t.action == ToggleAction::Wifi);
    let right = match wifi_idx.and_then(|i| toggles.get(i)) {
        Some(r) => r.right(),
        None => bar.right() - 140.0,
    };
    let x = (right - w).clamp(10.0, (bar.right() - w - 10.0).max(10.0));
    Rect::from_xywh(x, bar.bottom() + pad, w, h)
}

pub fn paint_wifi_popup(
    ctx: &mut PaintCtx,
    rect: Rect,
    wifi_enabled: bool,
    connected_ssid: Option<&str>,
    networks: &[WifiNetwork],
    hover: &Hover,
) {
    let t = ctx.theme;
    let hit = hover
        .position()
        .and_then(|p| wifi_popup_hit(rect, t, p, connected_ssid, networks));
    ctx.scene
        .fill_rounded_rect(rect, t.radius.large, t.colors.card_bg);
    ctx.scene
        .stroke_rounded_rect(rect, t.radius.large, 1.0, t.colors.border);

    let pad = t.spacing.lg;
    let icon_sz = 18.0;
    let icon_rect = Rect::from_xywh(rect.x() + pad, rect.y() + pad, icon_sz, icon_sz);
    panel_icon(
        ctx,
        IconName::Wifi,
        icon_rect,
        if wifi_enabled {
            t.colors.accent
        } else {
            t.colors.dim_text
        },
    );

    ctx.label_left(
        icon_rect.right() + t.spacing.sm,
        icon_rect.center().y,
        &tr("Wi-Fi network"),
        t.typography.body.size,
        weight::BOLD,
        t.colors.text,
    );

    let sw_w = 78.0;
    let sw_h = 24.0;
    let sw_rect = Rect::from_xywh(
        rect.right() - pad - sw_w,
        icon_rect.center().y - sw_h / 2.0,
        sw_w,
        sw_h,
    );
    let (sw_bg, sw_fg, sw_lbl) = if wifi_enabled {
        (t.colors.accent, t.colors.accent_fg, tr("On"))
    } else if hit == Some(WifiAction::TogglePower) {
        (libluna_widgets::hover_fill(t), t.colors.text, tr("Off"))
    } else {
        (t.colors.shade, t.colors.dim_text, tr("Off"))
    };
    ctx.scene.fill_rounded_rect(sw_rect, sw_h / 2.0, sw_bg);
    if !wifi_enabled {
        ctx.scene
            .stroke_rounded_rect(sw_rect, sw_h / 2.0, 1.0, t.colors.border);
    }
    ctx.label_center(
        sw_rect.center().x,
        sw_rect.center().y,
        &sw_lbl,
        t.typography.caption.size,
        weight::MEDIUM,
        sw_fg,
    );

    let sep_y = icon_rect.bottom() + t.spacing.md;
    ctx.scene.fill_rect(
        Rect::from_xywh(rect.x() + pad, sep_y, rect.width() - 2.0 * pad, 1.0),
        t.colors.border,
    );

    let list_y = sep_y + t.spacing.md;
    if !wifi_enabled {
        ctx.label_center(
            rect.center().x,
            list_y + 40.0,
            &tr("Wi-Fi is disabled"),
            t.typography.body.size,
            weight::NORMAL,
            t.colors.dim_text,
        );
        return;
    }

    if let Some(ssid) = connected_ssid {
        let conn_rect = Rect::from_xywh(rect.x() + pad, list_y, rect.width() - 2.0 * pad, 36.0);
        ctx.scene
            .fill_rounded_rect(conn_rect, t.radius.small, t.colors.accent.scale_alpha(0.12));
        ctx.scene
            .stroke_rounded_rect(conn_rect, t.radius.small, 1.0, t.colors.accent);
        ctx.label_left(
            conn_rect.x() + 10.0,
            conn_rect.center().y,
            &format!("✓ {ssid}"),
            t.typography.body.size,
            weight::MEDIUM,
            t.colors.accent,
        );
        ctx.label_right(
            conn_rect.right() - 10.0,
            conn_rect.center().y,
            &tr("Connected"),
            t.typography.caption.size,
            weight::NORMAL,
            t.colors.accent,
        );
    }

    let items_start_y = if connected_ssid.is_some() {
        list_y + 44.0
    } else {
        list_y
    };

    if networks.is_empty() && connected_ssid.is_none() {
        ctx.label_center(
            rect.center().x,
            items_start_y + 30.0,
            &tr("No Wi-Fi networks found"),
            t.typography.body.size,
            weight::NORMAL,
            t.colors.dim_text,
        );
        return;
    }

    let item_h = 32.0;
    let mut rendered = 0;
    for net in networks.iter() {
        if connected_ssid == Some(net.ssid.as_str()) {
            continue;
        }
        if rendered >= 4 {
            break;
        }
        let row = Rect::from_xywh(
            rect.x() + pad,
            items_start_y + rendered as f64 * item_h,
            rect.width() - 2.0 * pad,
            item_h,
        );
        let hovered = matches!(&hit, Some(WifiAction::SelectNetwork(ssid)) if ssid == &net.ssid);
        ctx.scene.fill_rounded_rect(
            row,
            t.radius.small,
            if hovered {
                libluna_widgets::hover_fill(t)
            } else {
                t.colors.shade
            },
        );
        ctx.label_left(
            row.x() + 10.0,
            row.center().y,
            &net.ssid,
            t.typography.body.size,
            weight::NORMAL,
            t.colors.text,
        );
        let sig_str = format!("{}%", net.signal);
        ctx.label_right(
            row.right() - 10.0,
            row.center().y,
            &sig_str,
            t.typography.caption.size,
            weight::NORMAL,
            t.colors.dim_text,
        );
        rendered += 1;
    }
}

pub fn wifi_popup_hit(
    rect: Rect,
    theme: &Theme,
    p: Point,
    connected_ssid: Option<&str>,
    networks: &[WifiNetwork],
) -> Option<WifiAction> {
    if !rect.contains(p) {
        return None;
    }
    let pad = theme.spacing.lg;
    let icon_sz = 18.0;
    let sw_w = 78.0;
    let sw_h = 24.0;
    let sw_rect = Rect::from_xywh(
        rect.right() - pad - sw_w,
        rect.y() + pad + icon_sz / 2.0 - sw_h / 2.0,
        sw_w,
        sw_h,
    );
    if sw_rect.contains(p) {
        return Some(WifiAction::TogglePower);
    }

    let sep_y = rect.y() + pad + icon_sz + theme.spacing.md;
    let list_y = sep_y + theme.spacing.md;
    let items_start_y = if connected_ssid.is_some() {
        list_y + 44.0
    } else {
        list_y
    };
    let item_h = 32.0;
    let mut rendered = 0;
    for net in networks.iter() {
        if connected_ssid == Some(net.ssid.as_str()) {
            continue;
        }
        if rendered >= 4 {
            break;
        }
        let row = Rect::from_xywh(
            rect.x() + pad,
            items_start_y + rendered as f64 * item_h,
            rect.width() - 2.0 * pad,
            item_h,
        );
        if row.contains(p) {
            return Some(WifiAction::SelectNetwork(net.ssid.clone()));
        }
        rendered += 1;
    }

    None
}

// ── Popup Kalendarz i Zegar ──

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CalendarAction {
    PrevMonth,
    NextMonth,
    SelectDay(u32),
    /// Otwórz pełną aplikację `luna-clock` na wybranym dniu.
    OpenApp,
}

pub fn calendar_popup_rect(bar: Rect, _theme: &Theme) -> Rect {
    let w = 320.0;
    // Więcej miejsca na plan dnia i „Otwórz kalendarz”.
    let h = 424.0;
    let pad = 6.0;
    let x = (bar.center().x - w / 2.0).clamp(10.0, (bar.right() - w - 10.0).max(10.0));
    Rect::from_xywh(x, bar.bottom() + pad, w, h)
}

/// Nazwa miesiąca w języku interfejsu (1 = styczeń).
pub fn month_name(month: u32) -> String {
    match month {
        1 => tr("January"),
        2 => tr("February"),
        3 => tr("March"),
        4 => tr("April"),
        5 => tr("May"),
        6 => tr("June"),
        7 => tr("July"),
        8 => tr("August"),
        9 => tr("September"),
        10 => tr("October"),
        11 => tr("November"),
        _ => tr("December"),
    }
}

pub fn days_in_month(year: i64, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            if (year % 4 == 0 && year % 100 != 0) || (year % 400 == 0) {
                29
            } else {
                28
            }
        }
        _ => 30,
    }
}

pub fn first_weekday_of_month(year: i64, month: u32) -> usize {
    let t = [0, 3, 2, 5, 0, 3, 5, 1, 4, 6, 2, 4];
    let y = if month < 3 { year - 1 } else { year };
    let m = month as usize;
    let d = 1;
    let w = (y + y / 4 - y / 100 + y / 400 + t[m - 1] + d) % 7;
    if w == 0 {
        6
    } else {
        (w - 1) as usize
    }
}

/// Prostokąt przycisku „Otwórz kalendarz” (spójny dla rysowania i trafień).
pub fn calendar_open_button_rect(rect: Rect, theme: &Theme) -> Rect {
    let pad = theme.spacing.lg;
    Rect::from_xywh(
        rect.x() + pad,
        rect.bottom() - pad - 30.0,
        (rect.width() - 2.0 * pad).max(0.0),
        30.0,
    )
}

/// Rysuje kalendarz panelu wraz z planem wybranego dnia.
pub fn paint_calendar_popup(
    ctx: &mut PaintCtx,
    rect: Rect,
    clock_str: &str,
    date_str: &str,
    view_year: i64,
    view_month: u32,
    today_year: i64,
    today_month: u32,
    today_day: u32,
    selected_day: Option<u32>,
) {
    paint_calendar_popup_with_events(
        ctx,
        rect,
        clock_str,
        date_str,
        view_year,
        view_month,
        today_year,
        today_month,
        today_day,
        selected_day,
        &[],
        "",
        true,
    );
}

/// Wariant z wydarzeniami i świętami (kropki i plan dnia).
pub fn paint_calendar_popup_with_events(
    ctx: &mut PaintCtx,
    rect: Rect,
    clock_str: &str,
    date_str: &str,
    view_year: i64,
    view_month: u32,
    today_year: i64,
    today_month: u32,
    today_day: u32,
    selected_day: Option<u32>,
    events: &[CalendarEvent],
    holiday_country: &str,
    use_24h: bool,
) {
    let t = ctx.theme;
    ctx.scene
        .fill_rounded_rect(rect, t.radius.large, t.colors.card_bg);
    ctx.scene
        .stroke_rounded_rect(rect, t.radius.large, 1.0, t.colors.border);

    let pad = t.spacing.lg;
    ctx.label_center(
        rect.center().x,
        rect.y() + pad + 14.0,
        clock_str,
        24.0,
        weight::BOLD,
        t.colors.text,
    );
    ctx.label_center(
        rect.center().x,
        rect.y() + pad + 38.0,
        date_str,
        t.typography.caption.size,
        weight::NORMAL,
        t.colors.dim_text,
    );

    let sep_y = rect.y() + pad + 54.0;
    ctx.scene.fill_rect(
        Rect::from_xywh(rect.x() + pad, sep_y, rect.width() - 2.0 * pad, 1.0),
        t.colors.border,
    );

    let nav_y = sep_y + t.spacing.md;
    let arrow_btn_sz = 26.0;

    let prev_btn = Rect::from_xywh(rect.x() + pad, nav_y, arrow_btn_sz, arrow_btn_sz);
    ctx.scene
        .fill_rounded_rect(prev_btn, t.radius.small, t.colors.shade);
    ctx.label_center(
        prev_btn.center().x,
        prev_btn.center().y,
        "‹",
        t.typography.body.size,
        weight::BOLD,
        t.colors.text,
    );

    let next_btn = Rect::from_xywh(
        rect.right() - pad - arrow_btn_sz,
        nav_y,
        arrow_btn_sz,
        arrow_btn_sz,
    );
    ctx.scene
        .fill_rounded_rect(next_btn, t.radius.small, t.colors.shade);
    ctx.label_center(
        next_btn.center().x,
        next_btn.center().y,
        "›",
        t.typography.body.size,
        weight::BOLD,
        t.colors.text,
    );

    let month_idx = view_month.clamp(1, 12);
    let title = format!("{} {}", month_name(month_idx), view_year);
    ctx.label_center(
        rect.center().x,
        nav_y + arrow_btn_sz / 2.0,
        &title,
        t.typography.body.size,
        weight::BOLD,
        t.colors.text,
    );

    let grid_x = rect.x() + pad;
    let grid_w = rect.width() - 2.0 * pad;
    let cell_w = grid_w / 7.0;
    let weekdays = [
        tr("Mon"),
        tr("Tue"),
        tr("Wed"),
        tr("Thu"),
        tr("Fri"),
        tr("Sat"),
        tr("Sun"),
    ];
    let day_names_y = nav_y + arrow_btn_sz + t.spacing.md;

    for (col, wname) in weekdays.iter().enumerate() {
        let cx = grid_x + col as f64 * cell_w + cell_w / 2.0;
        ctx.label_center(
            cx,
            day_names_y + 8.0,
            wname,
            t.typography.caption.size,
            weight::BOLD,
            t.colors.dim_text,
        );
    }

    let grid_start_y = day_names_y + 24.0;
    let cell_h = 28.0;
    let first_weekday = first_weekday_of_month(view_year, view_month);
    let total_days = days_in_month(view_year, view_month);
    let month_holidays: Vec<Holiday> = if holiday_country.is_empty() {
        Vec::new()
    } else {
        holidays::year_holidays(holiday_country, view_year as i32)
            .into_iter()
            .filter(|holiday| holiday.date.month == view_month)
            .collect()
    };

    for day in 1..=total_days {
        let cell_idx = first_weekday + (day - 1) as usize;
        let col = cell_idx % 7;
        let row = cell_idx / 7;
        let cx = grid_x + col as f64 * cell_w + cell_w / 2.0;
        let cy = grid_start_y + row as f64 * cell_h + cell_h / 2.0;
        let cell_rect = Rect::from_xywh(cx - 13.0, cy - 13.0, 26.0, 26.0);

        let is_today = view_year == today_year && view_month == today_month && day == today_day;
        let is_selected = selected_day == Some(day);
        let day_key = format!("{view_year:04}-{view_month:02}-{day:02}");
        let has_event = events.iter().any(|event| event.date == day_key);
        let has_holiday = month_holidays.iter().any(|holiday| holiday.date.day == day);

        if is_today {
            ctx.scene
                .fill_rounded_rect(cell_rect, 13.0, t.colors.accent);
            ctx.label_center(
                cx,
                cy,
                &day.to_string(),
                t.typography.caption.size,
                weight::BOLD,
                t.colors.accent_fg,
            );
        } else if is_selected {
            ctx.scene.fill_rounded_rect(cell_rect, 13.0, t.colors.shade);
            ctx.scene
                .stroke_rounded_rect(cell_rect, 13.0, 1.5, t.colors.accent);
            ctx.label_center(
                cx,
                cy,
                &day.to_string(),
                t.typography.caption.size,
                weight::BOLD,
                t.colors.text,
            );
        } else {
            ctx.label_center(
                cx,
                cy,
                &day.to_string(),
                t.typography.caption.size,
                weight::NORMAL,
                t.colors.text,
            );
        }
        if has_event {
            ctx.scene.fill_circle(
                Point::new(cx - if has_holiday { 3.0 } else { 0.0 }, cy + 9.0),
                2.0,
                if is_today {
                    t.colors.accent_fg
                } else {
                    t.colors.accent
                },
            );
        }
        if has_holiday {
            ctx.scene.fill_circle(
                Point::new(cx + if has_event { 3.0 } else { 0.0 }, cy + 9.0),
                2.0,
                t.colors.warning,
            );
        }
    }

    // Plan wybranego dnia + skrót do pełnego kalendarza.
    let list_day = selected_day
        .or_else(|| (view_year == today_year && view_month == today_month).then_some(today_day));
    let list_top = grid_start_y + 6.0 * cell_h + 4.0;
    let open = calendar_open_button_rect(rect, t);
    let list_bottom = open.y() - 6.0;
    if let Some(day) = list_day {
        let key = format!("{view_year:04}-{view_month:02}-{day:02}");
        let mut day_events: Vec<&CalendarEvent> =
            events.iter().filter(|event| event.date == key).collect();
        day_events.sort_by_key(|event| event.start_minute.unwrap_or(0));
        let day_holidays: Vec<&Holiday> = month_holidays
            .iter()
            .filter(|holiday| holiday.date.day == day)
            .collect();
        let mut ey = list_top + 8.0;
        let mut shown_events = 0usize;

        if let Some(holiday) = day_holidays.first() {
            ctx.scene
                .fill_circle(Point::new(rect.x() + pad + 4.0, ey), 3.0, t.colors.warning);
            ctx.label_left(
                rect.x() + pad + 14.0,
                ey,
                &tr(holiday.name),
                t.typography.caption.size,
                weight::MEDIUM,
                t.colors.warning,
            );
            ey += 17.0;
        }

        if day_events.is_empty() && day_holidays.is_empty() {
            ctx.label_left(
                rect.x() + pad,
                ey,
                &tr("No events"),
                t.typography.caption.size,
                weight::NORMAL,
                t.colors.dim_text,
            );
        } else {
            let max_events = if day_holidays.is_empty() { 2 } else { 1 };
            for event in day_events.iter().take(max_events) {
                if ey + 8.0 > list_bottom {
                    break;
                }
                let time = match event.start_minute {
                    Some(minute) if use_24h => format!("{:02}:{:02}", minute / 60, minute % 60),
                    Some(minute) => {
                        let suffix = if minute / 60 < 12 { "AM" } else { "PM" };
                        let hour = match (minute / 60) % 12 {
                            0 => 12,
                            hour => hour,
                        };
                        format!("{hour}:{:02} {suffix}", minute % 60)
                    }
                    None => tr("All day"),
                };
                let time_width = ctx.measure(&time, t.typography.caption.size, weight::MEDIUM);
                ctx.label_left(
                    rect.x() + pad,
                    ey,
                    &time,
                    t.typography.caption.size,
                    weight::MEDIUM,
                    t.colors.accent,
                );
                ctx.label_left(
                    rect.x() + pad + time_width.max(42.0) + 10.0,
                    ey,
                    &event.title,
                    t.typography.caption.size,
                    weight::NORMAL,
                    t.colors.text,
                );
                ey += 17.0;
                shown_events += 1;
            }
            let extra = day_events.len().saturating_sub(shown_events);
            if extra > 0 {
                ctx.label_left(
                    rect.x() + pad,
                    ey,
                    &format!("+{extra}"),
                    t.typography.caption.size,
                    weight::NORMAL,
                    t.colors.dim_text,
                );
            }
        }
    }

    ctx.scene
        .fill_rounded_rect(open, t.radius.small, t.colors.accent.scale_alpha(0.18));
    ctx.scene
        .stroke_rounded_rect(open, t.radius.small, 1.0, t.colors.accent);
    ctx.label_center(
        open.center().x,
        open.center().y,
        &tr("Open calendar"),
        t.typography.caption.size,
        weight::BOLD,
        t.colors.accent,
    );
}

pub fn calendar_popup_hit(
    rect: Rect,
    theme: &Theme,
    p: Point,
    view_year: i64,
    view_month: u32,
) -> Option<CalendarAction> {
    if !rect.contains(p) {
        return None;
    }
    let pad = theme.spacing.lg;
    let sep_y = rect.y() + pad + 54.0;
    let nav_y = sep_y + theme.spacing.md;
    let arrow_btn_sz = 26.0;

    let prev_btn = Rect::from_xywh(rect.x() + pad, nav_y, arrow_btn_sz, arrow_btn_sz);
    if prev_btn.contains(p) {
        return Some(CalendarAction::PrevMonth);
    }

    let next_btn = Rect::from_xywh(
        rect.right() - pad - arrow_btn_sz,
        nav_y,
        arrow_btn_sz,
        arrow_btn_sz,
    );
    if next_btn.contains(p) {
        return Some(CalendarAction::NextMonth);
    }

    if calendar_open_button_rect(rect, theme).contains(p) {
        return Some(CalendarAction::OpenApp);
    }

    let day_names_y = nav_y + arrow_btn_sz + theme.spacing.md;
    let grid_start_y = day_names_y + 24.0;
    let grid_x = rect.x() + pad;
    let grid_w = rect.width() - 2.0 * pad;
    let cell_w = grid_w / 7.0;
    let cell_h = 28.0;

    let first_weekday = first_weekday_of_month(view_year, view_month);
    let total_days = days_in_month(view_year, view_month);

    for day in 1..=total_days {
        let cell_idx = first_weekday + (day - 1) as usize;
        let col = cell_idx % 7;
        let row = cell_idx / 7;
        let cx = grid_x + col as f64 * cell_w + cell_w / 2.0;
        let cy = grid_start_y + row as f64 * cell_h + cell_h / 2.0;
        let cell_rect = Rect::from_xywh(cx - 13.0, cy - 13.0, 26.0, 26.0);
        if cell_rect.contains(p) {
            return Some(CalendarAction::SelectDay(day));
        }
    }

    None
}

// ── Centrum powiadomień (obok kalendarza, otwierane kliknięciem w zegar) ──

/// Szerokość karty centrum powiadomień.
pub const NOTIFICATIONS_W: f64 = 360.0;
/// Maksymalna wysokość karty centrum powiadomień.
pub const NOTIFICATIONS_H: f64 = 440.0;
/// Minimalna (pusta) wysokość karty — stan „brak powiadomień”.
const NOTIFICATIONS_MIN_H: f64 = 180.0;
/// Wysokość nagłówka (od góry karty do pierwszego wiersza).
const NOTIFY_HEADER_H: f64 = 44.0;
/// Wysokość wiersza powiadomienia.
const NOTIFY_ROW_H: f64 = 56.0;
/// Szerokość przycisku „Wyczyść”.
const NOTIFY_CLEAR_W: f64 = 74.0;

/// Prostokąt centrum powiadomień — obok kalendarza, po stronie z wolnym miejscem.
///
/// Karta rośnie z liczbą powiadomień (do [`NOTIFICATIONS_H`]), więc pusty stan
/// nie zajmuje pół ekranu.
pub fn notification_center_rect(bar: Rect, theme: &Theme, count: usize) -> Rect {
    let cal = calendar_popup_rect(bar, theme);
    let right_x = cal.right() + 8.0;
    let x = if right_x + NOTIFICATIONS_W <= bar.right() - 10.0 {
        right_x
    } else if cal.x() - 8.0 - NOTIFICATIONS_W >= 10.0 {
        cal.x() - 8.0 - NOTIFICATIONS_W
    } else {
        (bar.right() - NOTIFICATIONS_W - 10.0).max(10.0)
    };
    let max_rows = ((NOTIFICATIONS_H - NOTIFY_HEADER_H - theme.spacing.md) / NOTIFY_ROW_H)
        .floor()
        .max(0.0) as usize;
    let visible = count.min(max_rows);
    let h = (NOTIFY_HEADER_H + visible as f64 * NOTIFY_ROW_H + theme.spacing.md)
        .clamp(NOTIFICATIONS_MIN_H, NOTIFICATIONS_H);
    Rect::from_xywh(x, cal.y(), NOTIFICATIONS_W, h)
}

/// Rozmieszczenie elementów centrum powiadomień.
pub struct NotificationLayout {
    pub header: Rect,
    pub clear: Rect,
    /// Widoczne wiersze, od najnowszego.
    pub rows: Vec<Rect>,
}

/// Liczy układ centrum dla `count` powiadomień (spójny dla rysowania i trafień).
pub fn notification_layout(rect: Rect, theme: &Theme, count: usize) -> NotificationLayout {
    let pad = theme.spacing.md;
    let header = Rect::from_xywh(
        rect.x() + pad,
        rect.y() + pad + 2.0,
        (rect.width() - 2.0 * pad - NOTIFY_CLEAR_W - 8.0).max(0.0),
        22.0,
    );
    let clear = Rect::from_xywh(
        rect.right() - pad - NOTIFY_CLEAR_W,
        rect.y() + pad,
        NOTIFY_CLEAR_W,
        26.0,
    );
    let max_rows = ((rect.height() - NOTIFY_HEADER_H - pad) / NOTIFY_ROW_H)
        .floor()
        .max(0.0) as usize;
    let rows = (0..count.min(max_rows))
        .map(|i| {
            Rect::from_xywh(
                rect.x() + pad,
                rect.y() + NOTIFY_HEADER_H + i as f64 * NOTIFY_ROW_H,
                rect.width() - 2.0 * pad,
                NOTIFY_ROW_H - 4.0,
            )
        })
        .collect();
    NotificationLayout {
        header,
        clear,
        rows,
    }
}

/// Trafienie w centrum powiadomień.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotificationHit {
    /// Przycisk „Wyczyść” (czyści całą historię).
    ClearAll,
    /// Wiersz liczony od najnowszego (0 = najnowsze powiadomienie).
    Row(usize),
}

/// Co znajduje się pod punktem `p` w centrum powiadomień.
pub fn notification_center_hit(
    rect: Rect,
    theme: &Theme,
    count: usize,
    p: Point,
) -> Option<NotificationHit> {
    if !rect.contains(p) {
        return None;
    }
    let l = notification_layout(rect, theme, count);
    if count > 0 && l.clear.contains(p) {
        return Some(NotificationHit::ClearAll);
    }
    l.rows
        .iter()
        .position(|r| r.contains(p))
        .map(NotificationHit::Row)
}

/// Rysuje centrum powiadomień z historią (najnowsze na górze).
pub fn paint_notification_center(
    ctx: &mut PaintCtx,
    rect: Rect,
    history: &[Notification],
    dnd: bool,
    hover: Option<usize>,
) {
    let t = ctx.theme;
    let l = notification_layout(rect, t, history.len());
    ctx.scene
        .fill_rounded_rect(rect, t.radius.large, t.colors.card_bg);
    ctx.scene
        .stroke_rounded_rect(rect, t.radius.large, 1.0, t.colors.border);

    ctx.label_left(
        l.header.x(),
        l.header.center().y,
        &tr("Notifications"),
        t.typography.body.size,
        weight::BOLD,
        t.colors.text,
    );

    if !history.is_empty() {
        ctx.scene
            .fill_rounded_rect(l.clear, t.radius.small, t.colors.shade);
        ctx.label_center(
            l.clear.center().x,
            l.clear.center().y,
            &tr("Clear"),
            t.typography.caption.size,
            weight::MEDIUM,
            t.colors.text,
        );
    }

    if history.is_empty() {
        let cy = rect.center().y;
        let icon = Rect::from_xywh(rect.center().x - 16.0, cy - 44.0, 32.0, 32.0);
        panel_icon(ctx, IconName::Info, icon, t.colors.dim_text);
        ctx.label_center(
            rect.center().x,
            cy + 4.0,
            &tr("No notifications"),
            t.typography.body.size,
            weight::NORMAL,
            t.colors.dim_text,
        );
        if dnd {
            ctx.label_center(
                rect.center().x,
                cy + 26.0,
                &tr("Do Not Disturb"),
                t.typography.caption.size,
                weight::NORMAL,
                t.colors.accent,
            );
        }
        return;
    }

    let body = t.typography.body.size;
    let cap = t.typography.caption.size;
    for (row, r) in l.rows.iter().enumerate() {
        let idx = history.len() - 1 - row;
        let n = &history[idx];
        if hover == Some(row) {
            ctx.scene
                .fill_rounded_rect(*r, t.radius.small, libluna_widgets::hover_fill(t));
        }
        let icon = Rect::from_xywh(r.x() + 4.0, r.center().y - 14.0, 28.0, 28.0);
        ctx.scene
            .fill_rounded_rect(icon, t.radius.small, t.colors.accent.scale_alpha(0.18));
        panel_icon(
            ctx,
            n.icon,
            Rect::from_xywh(icon.x() + 6.0, icon.y() + 6.0, 16.0, 16.0),
            t.colors.accent,
        );

        let app_w = ctx.text.measure(&n.app, cap, weight::NORMAL).width;
        let text_x = icon.right() + 10.0;
        let title_w = (r.right() - 4.0 - text_x - app_w - 8.0).max(0.0);
        let title = ellipsize(&n.title, title_w, body, weight::MEDIUM, ctx.text);
        ctx.label_left(
            text_x,
            r.y() + 16.0,
            &title,
            body,
            weight::MEDIUM,
            t.colors.text,
        );
        ctx.label_right(
            r.right() - 4.0,
            r.y() + 16.0,
            &n.app,
            cap,
            weight::NORMAL,
            t.colors.dim_text,
        );

        let body_text = ellipsize(
            &n.body,
            r.right() - 4.0 - text_x,
            cap,
            weight::NORMAL,
            ctx.text,
        );
        ctx.label_left(
            text_x,
            r.y() + 36.0,
            &body_text,
            cap,
            weight::NORMAL,
            t.colors.dim_text,
        );
        ctx.scene.fill_rect(
            Rect::from_xywh(r.x() + 2.0, r.bottom(), r.width() - 4.0, 1.0),
            t.colors.border.scale_alpha(0.5),
        );
    }

    let more = history.len().saturating_sub(l.rows.len());
    if more > 0 {
        ctx.label_center(
            rect.center().x,
            rect.bottom() - 10.0,
            &tr_args("+{n} more", &[("n", &more.to_string())]),
            cap,
            weight::NORMAL,
            t.colors.dim_text,
        );
    }
}

/// Zwraca prostokąt aktywnego popupu (do testowania kliknięć poza nim).
pub fn popup_bounding_rect(
    popup: ActivePopup,
    bar: Rect,
    larea: Rect,
    theme: &Theme,
    text: &TextEngine,
    state: &PanelState,
) -> Option<Rect> {
    match popup {
        ActivePopup::None => None,
        ActivePopup::Launcher => Some(larea),
        ActivePopup::Wifi => Some(wifi_popup_rect(bar, theme, text, state)),
        ActivePopup::Calendar => Some(
            calendar_popup_rect(bar, theme).union(&notification_center_rect(
                bar,
                theme,
                state.notifications,
            )),
        ),
        ActivePopup::TaskMenu => None,
        ActivePopup::Quick => Some(quick_panel_rect(bar, theme)),
        ActivePopup::Tray => Some(tray_menu_rect(bar, theme, text, state)),
        // Menu kontekstowe tray’a ma prostokąt zależny od klikniętej ikony.
        ActivePopup::TrayContext => None,
    }
}

/// Prostokąt przycisku otwierającego dany popup na pasku.
pub fn popup_trigger_rect(
    popup: ActivePopup,
    bar: Rect,
    theme: &Theme,
    text: &TextEngine,
    state: &PanelState,
) -> Option<Rect> {
    match popup {
        ActivePopup::None => None,
        ActivePopup::Launcher => Some(burger_rect(bar, theme)),
        ActivePopup::Wifi => {
            let toggles = toggle_rects(bar, theme, text, state);
            let idx = state
                .toggles
                .iter()
                .position(|t| t.action == ToggleAction::Wifi)?;
            toggles.get(idx).copied()
        }
        ActivePopup::Calendar => Some(clock_rect(bar, theme, text, state)),
        // Menu taskbara ma własny prostokąt (zależny od klikniętego przycisku).
        ActivePopup::TaskMenu => None,
        ActivePopup::Quick => Some(quick_button_rect(bar, theme, text, state)),
        ActivePopup::Tray => tray_layout(bar, theme, text, state).overflow.or_else(|| {
            tray_layout(bar, theme, text, state)
                .icons
                .last()
                .map(|(r, _)| *r)
        }),
        ActivePopup::TrayContext => None,
    }
}

/// Ikona dla okna w taskbarze (heurystyka po `app_id` i tytule).
///
/// Protokół `zwlr_foreign_toplevel` nie przekazuje ikony aplikacji, więc dobieramy
/// ją z naszego zestawu — tak jak launcher dopasowuje wpisy `.desktop`.
pub fn task_icon(task: &Task) -> IconName {
    crate::apps::icon_for_hint(&task.app_id, &task.title, None, &[], "")
}

/// Rozmieszczenie przycisków okien (taskbar) w pasku.
///
/// Najpierw próbujemy wariantu z ikoną i nazwą. Gdy okien jest dużo i nie mieszczą się
/// przed zegarem, przechodzimy na **same ikony** (kwadratowe przyciski) — dzięki temu
/// taskbar nie „przelewa się” poza pasek przy kilkunastu oknach.
pub fn task_layout(
    bar: Rect,
    theme: &Theme,
    text: &TextEngine,
    tasks: &[Task],
) -> (Vec<Rect>, bool) {
    let t = theme;
    let cy = bar.center().y;
    let cap = t.typography.caption.size;
    let icon = 20.0;
    let start = burger_rect(bar, t).right() + t.spacing.md + 1.0 + t.spacing.md;
    let limit = bar.center().x - t.spacing.lg;

    let layout = |icon_only: bool| {
        let mut x = start;
        let mut rects = Vec::new();
        for task in tasks {
            let w = if icon_only {
                2.0 * t.spacing.sm + icon
            } else {
                let label = task_label(task);
                (text.measure(&label, cap, weight::MEDIUM).width
                    + icon
                    + t.spacing.sm
                    + 2.0 * t.spacing.md)
                    .min(200.0)
            };
            if x + w > limit {
                break;
            }
            rects.push(Rect::from_xywh(x, cy - 13.0, w, 26.0));
            x += w + t.spacing.sm;
        }
        rects
    };

    let labeled = layout(false);
    if labeled.len() == tasks.len() {
        (labeled, false)
    } else {
        (layout(true), true)
    }
}

/// Rozmieszczenie przycisków okien (bez informacji o trybie ikon).
pub fn task_rects(bar: Rect, theme: &Theme, text: &TextEngine, tasks: &[Task]) -> Vec<Rect> {
    task_layout(bar, theme, text, tasks).0
}

/// Akcje menu kontekstowego przycisku okna (PPM).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskMenuAction {
    /// Pokaż zminimalizowane okno.
    Show,
    /// Schowaj (zminimalizuj) widoczne okno.
    Minimize,
    /// Zamknij program.
    Close,
    MoveTo(usize),
}

/// Wysokość pozycji menu taskbara.
pub const TASK_MENU_ITEM_H: f64 = 30.0;

/// Prostokąt menu kontekstowego pod przyciskiem okna.
pub fn task_menu_rect(anchor: Rect, bar: Rect, items: usize, _theme: &Theme) -> Rect {
    let pad = 6.0;
    let w = 240.0;
    let h = items as f64 * TASK_MENU_ITEM_H + 2.0 * pad;
    let min_x = bar.x() + 4.0;
    let max_x = (bar.right() - w - 4.0).max(min_x);
    let x = (anchor.center().x - w / 2.0).clamp(min_x, max_x);
    Rect::from_xywh(x, bar.bottom() + 4.0, w, h)
}

/// Indeks pozycji menu pod punktem `p`.
pub fn task_menu_item_at(rect: Rect, items: usize, p: Point) -> Option<usize> {
    let pad = 6.0;
    let inner = Rect::from_xywh(
        rect.x() + pad,
        rect.y() + pad,
        rect.width() - 2.0 * pad,
        items as f64 * TASK_MENU_ITEM_H,
    );
    if !inner.contains(p) {
        return None;
    }
    let i = ((p.y - inner.y()) / TASK_MENU_ITEM_H).floor() as usize;
    (i < items).then_some(i)
}

/// Rysuje menu kontekstowe taskbara.
pub fn paint_task_menu(
    ctx: &mut PaintCtx,
    rect: Rect,
    labels: &[String],
    actions: &[TaskMenuAction],
    hovered: Option<usize>,
) {
    let t = ctx.theme;
    ctx.scene
        .fill_rounded_rect(rect, t.radius.medium, t.colors.card_bg);
    ctx.scene
        .stroke_rounded_rect(rect, t.radius.medium, 1.0, t.colors.border);
    let pad = 6.0;
    let inner = Rect::from_xywh(
        rect.x() + pad,
        rect.y() + pad,
        rect.width() - 2.0 * pad,
        labels.len() as f64 * TASK_MENU_ITEM_H,
    );
    for (i, label) in labels.iter().enumerate() {
        let row = Rect::from_xywh(
            inner.x(),
            inner.y() + i as f64 * TASK_MENU_ITEM_H,
            inner.width(),
            TASK_MENU_ITEM_H,
        );
        if hovered == Some(i) {
            ctx.scene
                .fill_rounded_rect(row, t.radius.small, libluna_widgets::hover_fill(t));
        }
        let icon = match actions.get(i) {
            Some(TaskMenuAction::Show) => IconName::App,
            Some(TaskMenuAction::Minimize) => IconName::Minimize,
            Some(TaskMenuAction::Close) => IconName::Close,
            Some(TaskMenuAction::MoveTo(_)) => IconName::Grid,
            None => IconName::App,
        };
        let isz = 18.0;
        let icon_rect = Rect::from_xywh(row.x() + 8.0, row.center().y - isz / 2.0, isz, isz);
        panel_icon(ctx, icon, icon_rect, t.colors.text);
        let text_x = icon_rect.right() + 10.0;
        let avail = (row.right() - 8.0 - text_x).max(0.0);
        let label = ellipsize(
            label,
            avail,
            t.typography.body.size,
            weight::NORMAL,
            ctx.text,
        );
        ctx.label_left(
            text_x,
            row.center().y,
            &label,
            t.typography.body.size,
            weight::NORMAL,
            t.colors.text,
        );
    }
}

// ── Menu kontekstowe aplikacji w launcherze (PPM) ──

/// Akcje menu kontekstowego aplikacji w launcherze.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppMenuAction {
    /// Uruchom aplikację.
    Launch,
    /// Dodaj do ulubionych / usuń z ulubionych.
    ToggleFavorite,
    /// Uruchom z uprawnieniami administratora (`pkexec` → agent polkit Luny).
    OpenAsAdmin,
    /// Skopiuj polecenie uruchamiające do schowka.
    CopyCommand,
    /// Otwórz Sklep na pakiecie, żeby móc go odinstalować.
    Uninstall,
}

/// Jedna pozycja menu kontekstowego aplikacji.
///
/// `action == None` oznacza separator — nie da się go kliknąć.
#[derive(Debug, Clone)]
pub struct AppMenuRow {
    pub label: String,
    pub action: Option<AppMenuAction>,
    pub enabled: bool,
    /// Pozycja niebezpieczna (np. odinstalowanie) — rysowana kolorem błędu.
    pub danger: bool,
}

impl AppMenuRow {
    /// Pozioma linia rozdzielająca grupy akcji.
    pub fn separator() -> Self {
        Self {
            label: String::new(),
            action: None,
            enabled: false,
            danger: false,
        }
    }
}

/// Wysokość pozycji menu aplikacji.
pub const APP_MENU_ROW_H: f64 = 32.0;
/// Wysokość separatora w menu aplikacji.
pub const APP_MENU_SEPARATOR_H: f64 = 9.0;
/// Szerokość menu aplikacji.
pub const APP_MENU_W: f64 = 244.0;

fn app_menu_row_h(row: &AppMenuRow) -> f64 {
    if row.action.is_none() {
        APP_MENU_SEPARATOR_H
    } else {
        APP_MENU_ROW_H
    }
}

/// Prostokąt menu kontekstowego obok kafla `anchor`.
///
/// Menu otwiera się po prawej stronie kafla; gdy brakuje miejsca — po lewej.
/// Całość jest przycinana do `bounds` (powierzchni powłoki), żeby nie wyszła
/// poza ekran przy kafelkach na dole listy.
pub fn app_menu_rect(anchor: Rect, rows: &[AppMenuRow], bounds: Rect, theme: &Theme) -> Rect {
    let pad = theme.spacing.sm;
    let h = 2.0 * pad + rows.iter().map(app_menu_row_h).sum::<f64>();
    let gap = 8.0;
    let min_x = bounds.x() + gap;
    let max_x = (bounds.right() - APP_MENU_W - gap).max(min_x);
    let mut x = anchor.right() + gap;
    if x > max_x {
        x = anchor.x() - APP_MENU_W - gap;
    }
    let x = x.clamp(min_x, max_x);
    let min_y = bounds.y() + gap;
    let max_y = (bounds.bottom() - h - gap).max(min_y);
    let y = anchor.y().clamp(min_y, max_y);
    Rect::from_xywh(x, y, APP_MENU_W, h)
}

/// Indeks klikniętej pozycji menu aplikacji (pomija separatory i wyłączone wpisy).
pub fn app_menu_item_at(rect: Rect, rows: &[AppMenuRow], p: Point) -> Option<usize> {
    if !rect.contains(p) {
        return None;
    }
    let mut y = rect.y() + 6.0;
    for (i, row) in rows.iter().enumerate() {
        let h = app_menu_row_h(row);
        let r = Rect::from_xywh(rect.x() + 6.0, y, rect.width() - 12.0, h);
        if r.contains(p) {
            return (row.enabled && row.action.is_some()).then_some(i);
        }
        y += h;
    }
    None
}

/// Rysuje menu kontekstowe aplikacji.
pub fn paint_app_menu(ctx: &mut PaintCtx, rect: Rect, rows: &[AppMenuRow], hovered: Option<usize>) {
    let t = ctx.theme;
    ctx.scene
        .fill_rounded_rect(rect, t.radius.medium, t.colors.card_bg);
    ctx.scene
        .stroke_rounded_rect(rect, t.radius.medium, 1.0, t.colors.border);
    let mut y = rect.y() + 6.0;
    for (i, row) in rows.iter().enumerate() {
        let h = app_menu_row_h(row);
        let r = Rect::from_xywh(rect.x() + 6.0, y, rect.width() - 12.0, h);
        let Some(action) = row.action else {
            ctx.scene.fill_rect(
                Rect::from_xywh(r.x() + 8.0, r.center().y, r.width() - 16.0, 1.0),
                t.colors.border,
            );
            y += h;
            continue;
        };
        if hovered == Some(i) && row.enabled {
            ctx.scene
                .fill_rounded_rect(r, t.radius.small, libluna_widgets::hover_fill(t));
        }
        let color = if !row.enabled {
            t.colors.dim_text
        } else if row.danger {
            t.colors.error
        } else {
            t.colors.text
        };
        let icon = match action {
            AppMenuAction::Launch => IconName::Play,
            AppMenuAction::ToggleFavorite => IconName::Star,
            AppMenuAction::OpenAsAdmin => IconName::User,
            AppMenuAction::CopyCommand => IconName::Edit,
            AppMenuAction::Uninstall => IconName::Close,
        };
        let isz = 18.0;
        let icon_rect = Rect::from_xywh(r.x() + 8.0, r.center().y - isz / 2.0, isz, isz);
        panel_icon(ctx, icon, icon_rect, color);
        let text_x = icon_rect.right() + 10.0;
        let avail = (r.right() - 8.0 - text_x).max(0.0);
        let label = ellipsize(
            &row.label,
            avail,
            t.typography.body.size,
            weight::NORMAL,
            ctx.text,
        );
        ctx.label_left(
            text_x,
            r.center().y,
            &label,
            t.typography.body.size,
            weight::NORMAL,
            color,
        );
        y += h;
    }
}

/// Indeks okna (przycisku) pod punktem `p`.
pub fn task_at(
    bar: Rect,
    theme: &Theme,
    text: &TextEngine,
    tasks: &[Task],
    p: Point,
) -> Option<usize> {
    if p.y < bar.y() || p.y > bar.bottom() {
        return None;
    }
    task_rects(bar, theme, text, tasks)
        .iter()
        .position(|r| p.x >= r.x() - 2.0 && p.x <= r.right() + 2.0)
}

#[cfg(test)]
mod taskbar_tests {
    use super::*;

    #[test]
    fn long_task_label_is_ellipsized_to_button_width() {
        let theme = Theme::light();
        let text = TextEngine::system();
        let cap = theme.typography.caption.size;
        let long = "Bardzo długa nazwa okna — dokument bez tytułu 2026";
        let out = ellipsize(long, 90.0, cap, libluna_core::style::weight::MEDIUM, &text);
        assert!(out.ends_with('…'), "brak wielokropka: {out:?}");
        assert!(out.chars().count() < long.chars().count());
        assert!(
            text.measure(&out, cap, libluna_core::style::weight::MEDIUM)
                .width
                <= 90.5,
            "nazwa nadal wystaje: {out:?}"
        );
    }

    #[test]
    fn many_tasks_switch_to_icons() {
        let theme = Theme::light();
        let text = TextEngine::system();
        let bar = Rect::from_xywh(0.0, 0.0, 1280.0, THICKNESS);
        let tasks: Vec<Task> = (0..12)
            .map(|i| Task {
                title: format!("Bardzo długa nazwa okna numer {i}"),
                app_id: "app".into(),
                active: false,
                minimized: false,
            })
            .collect();
        let (rects, icon_only) = task_layout(bar, &theme, &text, &tasks);
        assert!(icon_only, "przy 12 oknach taskbar powinien użyć ikon");
        assert_eq!(rects.len(), tasks.len());
        assert!((rects[0].width() - 32.0).abs() < 0.1);
        assert!(rects.last().unwrap().right() < bar.center().x);
    }

    #[test]
    fn task_menu_hit_test_matches_rows() {
        let theme = Theme::light();
        let bar = Rect::from_xywh(0.0, 0.0, 1280.0, THICKNESS);
        let anchor = Rect::from_xywh(50.0, 8.0, 100.0, 24.0);
        let menu = task_menu_rect(anchor, bar, 6, &theme);
        assert!(menu.x() >= bar.x());
        assert!(menu.right() <= bar.right());
        for i in 0..6 {
            let y = menu.y() + 6.0 + (i as f64 + 0.5) * TASK_MENU_ITEM_H;
            assert_eq!(
                task_menu_item_at(menu, 6, Point::new(menu.center().x, y)),
                Some(i)
            );
        }
        assert_eq!(
            task_menu_item_at(menu, 6, Point::new(menu.center().x, menu.bottom() + 20.0)),
            None
        );
    }

    #[test]
    fn task_icons_follow_app_id() {
        assert_eq!(
            task_icon(&Task {
                title: "patryks — Thunar".into(),
                app_id: "thunar".into(),
                active: false,
                minimized: false,
            }),
            IconName::Files
        );
        assert_eq!(
            task_icon(&Task {
                title: "Terminal".into(),
                app_id: "xfce4-terminal".into(),
                active: false,
                minimized: false,
            }),
            IconName::Terminal
        );
        assert_eq!(
            task_icon(&Task {
                title: "cokolwiek".into(),
                app_id: "nieznana".into(),
                active: false,
                minimized: false,
            }),
            IconName::App
        );
    }

    #[test]
    fn task_buttons_layout_and_hit_test() {
        let theme = Theme::light();
        let text = TextEngine::system();
        let bar = Rect::from_xywh(0.0, 0.0, 1280.0, THICKNESS);
        let tasks = vec![
            Task {
                title: "Pliki".into(),
                app_id: "files".into(),
                active: true,
                minimized: false,
            },
            Task {
                title: "Terminal".into(),
                app_id: "term".into(),
                active: false,
                minimized: false,
            },
        ];
        let rects = task_rects(bar, &theme, &text, &tasks);
        assert_eq!(rects.len(), 2);
        assert!(rects[1].x() > rects[0].right());
        assert_eq!(
            task_at(bar, &theme, &text, &tasks, rects[1].center()),
            Some(1)
        );
        assert_eq!(
            task_at(bar, &theme, &text, &tasks, Point::new(2.0, 2.0)),
            None
        );
    }
}

/// Krótka etykieta okna (tytuł, a gdy pusty — `app_id`), przycięta do ~28 znaków.
fn task_label(task: &Task) -> String {
    let s = if !task.title.is_empty() {
        task.title.clone()
    } else {
        task.app_id.clone()
    };
    let max = 28;
    if s.chars().count() > max {
        let mut out: String = s.chars().take(max - 1).collect();
        out.push('…');
        out
    } else {
        s
    }
}

/// Skraca tekst tak, żeby **w pikselach** zmieścił się w `max_w` (z „…” na końcu).
///
/// Wcześniej nazwa była ucinana po liczbie znaków, ale przy szerokich literach
/// (albo wąskiej pigułce) i tak wystawała poza przycisk taskbara.
fn ellipsize(text: &str, max_w: f64, size: f64, weight: u16, engine: &TextEngine) -> String {
    if max_w <= 0.0 || engine.measure(text, size, weight).width <= max_w {
        return text.to_owned();
    }
    let dots = "…";
    let dots_w = engine.measure(dots, size, weight).width;
    let mut out = String::new();
    for ch in text.chars() {
        let mut probe = out.clone();
        probe.push(ch);
        if engine.measure(&probe, size, weight).width + dots_w > max_w {
            break;
        }
        out = probe;
    }
    out.push('…');
    out
}

/// Skraca tekst od **lewej**, żeby koniec (tam, gdzie jest kursor) był widoczny.
///
/// Używane w polu wyszukiwania: przy długiej frazie widać jej końcówkę z „…”
/// na początku, a nie początek ucięty pod przyciskiem „×”.
fn ellipsize_left(text: &str, max_w: f64, size: f64, weight: u16, engine: &TextEngine) -> String {
    if max_w <= 0.0 || engine.measure(text, size, weight).width <= max_w {
        return text.to_owned();
    }
    let dots = "…";
    let dots_w = engine.measure(dots, size, weight).width;
    let mut tail = String::new();
    for ch in text.chars().rev() {
        let mut probe = ch.to_string();
        probe.push_str(&tail);
        if engine.measure(&probe, size, weight).width + dots_w > max_w {
            break;
        }
        tail = probe;
    }
    format!("{dots}{tail}")
}

/// Ikona okna w taskbarze: najpierw oryginalna ikona aplikacji, potem wektorowa.
///
/// `app_id` z kompozytora nie zawsze jest nazwą ikony, dlatego próbujemy
/// dopasować wpis `.desktop` (`Icon=gparted`, `Icon=net.lutris.Lutris`…).
fn paint_task_icon(ctx: &mut PaintCtx, task: &Task, rect: Rect, color: Color) {
    let theme_icon = crate::apps::icon_name_for_app_id(&task.app_id)
        .or_else(|| (!task.app_id.trim().is_empty()).then(|| task.app_id.clone()));
    if let Some(name) = theme_icon {
        if paint_named_icon(ctx, Some(&name), rect) {
            return;
        }
    }
    paint_icon(ctx, task_icon(task), rect, color);
}

/// Rysuje przycisk okna w taskbarze (aktywny — akcent).
fn paint_task_button(ctx: &mut PaintCtx, rect: Rect, task: &Task, icon_only: bool, hovered: bool) {
    let t = ctx.theme;
    let cap = t.typography.caption.size;
    let (bg, fg) = if task.active {
        (t.colors.accent, t.colors.accent_fg)
    } else {
        (t.colors.card_bg, t.colors.text)
    };
    ctx.scene.fill_rounded_rect(rect, rect.height() / 2.0, bg);
    if hovered {
        ctx.scene
            .fill_rounded_rect(rect, rect.height() / 2.0, libluna_widgets::hover_fill(t));
    }
    if !task.active {
        ctx.scene
            .stroke_rounded_rect(rect, rect.height() / 2.0, 1.0, t.colors.border);
    }
    let isz = 20.0;
    if icon_only {
        let icon_rect = Rect::from_xywh(
            rect.center().x - isz / 2.0,
            rect.center().y - isz / 2.0,
            isz,
            isz,
        );
        paint_task_icon(ctx, task, icon_rect, fg);
        return;
    }
    let icon_rect = Rect::from_xywh(
        rect.x() + t.spacing.sm,
        rect.center().y - isz / 2.0,
        isz,
        isz,
    );
    paint_task_icon(ctx, task, icon_rect, fg);
    let text_x = icon_rect.right() + t.spacing.sm;
    let avail = (rect.right() - t.spacing.sm - text_x).max(0.0);
    let label = ellipsize(&task_label(task), avail, cap, weight::MEDIUM, ctx.text);
    ctx.label_left(text_x, rect.center().y, &label, cap, weight::MEDIUM, fg);
}

/// Rysuje panel górny w zadanym prostokącie.
pub fn paint_panel(panel: &PanelState, hover: &Hover, ctx: &mut PaintCtx, rect: Rect) {
    let t = ctx.theme;
    let cy = rect.center().y;
    let body = t.typography.body.size;

    // Tło + dolna krawędź oddzielająca od pulpitu.
    ctx.scene.fill_rect(rect, t.colors.headerbar_bg);
    ctx.scene.fill_rect(
        Rect::from_xywh(rect.x(), rect.bottom() - 1.0, rect.width(), 1.0),
        t.colors.border,
    );

    // ── Lewa grupa: sam burger (ikona Menu) + separator + tytuł okna ──
    let burger = burger_rect(rect, t);
    ctx.scene.fill_rounded_rect(
        burger,
        t.radius.small,
        if hover.is_hovered(burger) {
            libluna_widgets::hover_fill(t)
        } else {
            t.colors.shade
        },
    );
    let isz = burger.width() * 0.64;
    panel_icon(
        ctx,
        IconName::Menu,
        Rect::from_xywh(
            burger.center().x - isz / 2.0,
            burger.center().y - isz / 2.0,
            isz,
            isz,
        ),
        t.colors.text,
    );
    let mut x = burger.right() + t.spacing.md;
    ctx.scene
        .fill_rect(Rect::from_xywh(x, cy - 8.0, 1.0, 16.0), t.colors.border);
    x += t.spacing.md;
    // Taskbar: przyciski okien; przy braku okien — tytuł (podglądy headless).
    if panel.tasks.is_empty() {
        let max = (clock_rect(rect, t, ctx.text, panel).x() - t.spacing.md - x).max(0.0);
        let title = ellipsize(&panel.title, max, body, weight::NORMAL, ctx.text);
        ctx.label_left(x, cy, &title, body, weight::NORMAL, t.colors.dim_text);
    } else {
        let (rects, icon_only) = task_layout(rect, t, ctx.text, &panel.tasks);
        for (r, task) in rects.iter().zip(panel.tasks.iter()) {
            paint_task_button(ctx, *r, task, icon_only, hover.is_hovered(*r));
        }
    }

    // ── Środek: zegar + data ──
    let clock_r = clock_rect(rect, t, ctx.text, panel);
    if hover.is_hovered(clock_r) {
        ctx.scene
            .fill_rounded_rect(clock_r, t.radius.small, libluna_widgets::hover_fill(t));
    }
    let clock = format!("{}   ·   {}", panel.clock, panel.date);
    ctx.label_center(
        rect.center().x,
        cy,
        &clock,
        body,
        weight::MEDIUM,
        t.colors.text,
    );
    // Kropka „są nowe powiadomienia” — klik w zegar otwiera kalendarz z centrum.
    // Znika po otwarciu centrum (jak w GNOME), a nie dopiero po wyczyszczeniu historii.
    if panel.unseen > 0 {
        let dot = Rect::from_xywh(clock_r.right() - 9.0, cy - 3.0, 6.0, 6.0);
        ctx.scene.fill_rounded_rect(dot, 3.0, t.colors.accent);
    }

    if panel.security.any() {
        paint_security_indicators(ctx, rect, clock_r, panel);
    }

    // ── Prawa grupa (od prawej): przełączniki statusu i przestrzenie ──
    // Użytkownik i akcje sesji są w Quick Panelu — pasek nie powtarza ich etykiet.

    // Przełączniki szybkiego dostępu (pigułki).
    let toggles = toggle_rects(rect, t, ctx.text, panel);
    for (r, toggle) in toggles.iter().zip(panel.toggles.iter()) {
        paint_toggle_pill(ctx, *r, toggle, hover.is_hovered_within(*r, 4.0));
    }

    // Kropki przestrzeni roboczych (spójne z `workspace_rects`).
    for (i, dot) in workspace_rects(rect, t, ctx.text, panel).iter().enumerate() {
        if hover.is_hovered_within(*dot, 4.0) {
            let bubble =
                Rect::from_xywh(dot.x() - 4.0, dot.center().y - 7.0, dot.width() + 8.0, 14.0);
            ctx.scene
                .fill_rounded_rect(bubble, 7.0, libluna_widgets::hover_fill(t));
        }
        let color = if i == panel.current_ws {
            t.colors.accent
        } else {
            t.colors.dim_text
        };
        ctx.scene.fill_rounded_rect(*dot, dot.height() / 2.0, color);
    }

    // Ikony tray’a (SNI) — na lewo od przycisku Quick.
    let tray = tray_layout(rect, t, ctx.text, panel);
    for (r, i) in &tray.icons {
        if let Some(item) = panel.tray.get(*i) {
            paint_tray_item(ctx, item, *r, hover.is_hovered_within(*r, 3.0));
        }
    }
    if let Some(r) = tray.overflow {
        ctx.scene.fill_rounded_rect(
            r,
            t.radius.small,
            if hover.is_hovered_within(r, 3.0) {
                libluna_widgets::hover_fill(t)
            } else {
                t.colors.shade
            },
        );
        panel_icon(ctx, IconName::ChevronRight, r, t.colors.text);
    }

    // Przycisk Quick Panelu (motyw, Wi-Fi, dźwięk, jasność).
    let qb = quick_button_rect(rect, t, ctx.text, panel);
    ctx.scene.fill_rounded_rect(
        qb,
        t.radius.small,
        if hover.is_hovered(qb) {
            libluna_widgets::hover_fill(t)
        } else {
            t.colors.shade
        },
    );
    panel_icon(
        ctx,
        IconName::Sliders,
        Rect::from_xywh(qb.center().x - 7.5, qb.center().y - 7.5, 15.0, 15.0),
        t.colors.text,
    );
}

/// Wskaźniki prywatności na pasku: mikrofon, kamera, alert sieciowy.
///
/// Rysujemy je między zegarem a prawą grupą przełączników. Jeśli miejsca jest
/// za mało (wąski ekran), pomijamy je — ważniejszy jest taskbar i przełączniki.
fn paint_security_indicators(ctx: &mut PaintCtx, bar: Rect, clock_r: Rect, panel: &PanelState) {
    let t = *ctx.theme;
    let mut icons: Vec<(IconName, Color)> = Vec::new();
    if panel.security.microphone {
        icons.push((IconName::Microphone, t.colors.error));
    }
    if panel.security.camera {
        icons.push((IconName::Camera, t.colors.error));
    }
    if panel.security.network {
        icons.push((IconName::Globe, t.colors.accent));
    }
    if icons.is_empty() {
        return;
    }
    let isz = 15.0;
    let gap = 6.0;
    let pad = 6.0;
    let width = 2.0 * pad + icons.len() as f64 * isz + (icons.len().saturating_sub(1) as f64) * gap;
    let right_limit = toggle_rects(bar, &t, ctx.text, panel)
        .first()
        .map(|r| r.x())
        .unwrap_or(bar.right() - 120.0);
    let x = clock_r.right() + t.spacing.lg;
    if x + width > right_limit - 8.0 {
        return;
    }
    let chip = Rect::from_xywh(x, bar.center().y - 13.0, width, 26.0);
    ctx.scene
        .fill_rounded_rect(chip, t.radius.small, t.colors.shade);
    ctx.scene
        .stroke_rounded_rect(chip, t.radius.small, 1.0, t.colors.border);
    let mut ix = chip.x() + pad;
    for (icon, color) in icons {
        paint_icon(
            ctx,
            icon,
            Rect::from_xywh(ix, chip.center().y - isz / 2.0, isz, isz),
            color,
        );
        ix += isz + gap;
    }
    if panel.security.alert {
        let dot = Rect::from_xywh(chip.right() - 7.0, chip.y() + 4.0, 5.0, 5.0);
        ctx.scene.fill_rounded_rect(dot, 2.5, t.colors.error);
    }
}

/// Modal „Luna Security” — prośba o zgodę na mikrofon/kamerę/sieć.
pub fn paint_security_dialog(
    ctx: &mut PaintCtx,
    layout: SecurityDialogLayout,
    icon: IconName,
    app: &str,
    capability: &str,
    message: &str,
    remote: Option<&str>,
    hover: Option<SecurityChoice>,
) {
    let t = *ctx.theme;
    let card = layout.card;
    ctx.scene
        .fill_rounded_rect(card, t.radius.large, t.colors.card_bg);
    ctx.scene
        .stroke_rounded_rect(card, t.radius.large, 1.0, t.colors.border);
    // Akcentowy pasek na górze karty — sygnał „to wymaga decyzji”.
    ctx.scene.fill_rounded_rect(
        Rect::from_xywh(
            card.x() + t.radius.large,
            card.y(),
            card.width() - 2.0 * t.radius.large,
            3.0,
        ),
        1.5,
        t.colors.accent,
    );

    let pad = t.spacing.lg;
    let icon_sz = 34.0;
    let icon_rect = Rect::from_xywh(card.x() + pad, card.y() + pad, icon_sz, icon_sz);
    ctx.scene
        .fill_rounded_rect(icon_rect, icon_sz / 2.0, t.colors.accent.scale_alpha(0.14));
    paint_icon(
        ctx,
        icon,
        Rect::from_xywh(
            icon_rect.center().x - 9.0,
            icon_rect.center().y - 9.0,
            18.0,
            18.0,
        ),
        t.colors.accent,
    );
    let text_x = icon_rect.right() + t.spacing.md;
    let cap = ellipsize(
        capability,
        card.right() - pad - text_x,
        t.typography.caption.size,
        weight::MEDIUM,
        ctx.text,
    );
    ctx.label_left(
        text_x,
        card.y() + pad + 6.0,
        &tr("Luna Security"),
        t.typography.heading.size,
        weight::BOLD,
        t.colors.text,
    );
    ctx.label_left(
        text_x,
        card.y() + pad + 24.0,
        &cap,
        t.typography.caption.size,
        weight::MEDIUM,
        t.colors.dim_text,
    );

    let body_top = icon_rect.bottom() + t.spacing.md;
    let app = ellipsize(
        app,
        card.width() - 2.0 * pad,
        t.typography.body.size,
        weight::BOLD,
        ctx.text,
    );
    ctx.label_left(
        card.x() + pad,
        body_top,
        &app,
        t.typography.body.size,
        weight::BOLD,
        t.colors.text,
    );
    let message = ellipsize(
        message,
        card.width() - 2.0 * pad,
        t.typography.body.size,
        weight::NORMAL,
        ctx.text,
    );
    ctx.label_left(
        card.x() + pad,
        body_top + 22.0,
        &message,
        t.typography.body.size,
        weight::NORMAL,
        t.colors.text,
    );
    if let Some(remote) = remote {
        let line = format!("{}: {remote}", tr("Remote server"));
        let line = ellipsize(
            &line,
            card.width() - 2.0 * pad,
            t.typography.caption.size,
            weight::NORMAL,
            ctx.text,
        );
        ctx.label_left(
            card.x() + pad,
            body_top + 46.0,
            &line,
            t.typography.caption.size,
            weight::NORMAL,
            t.colors.dim_text,
        );
    }
    ctx.label_left(
        card.x() + pad,
        layout.deny.y() - 10.0,
        &tr("The decision will be remembered"),
        t.typography.caption.size,
        weight::NORMAL,
        t.colors.dim_text,
    );
    paint_security_button(
        ctx,
        layout.deny,
        &tr("Deny"),
        false,
        hover == Some(SecurityChoice::Deny),
    );
    paint_security_button(
        ctx,
        layout.allow,
        &tr("Allow"),
        true,
        hover == Some(SecurityChoice::Allow),
    );
}

fn paint_security_button(
    ctx: &mut PaintCtx,
    rect: Rect,
    label: &str,
    primary: bool,
    hovered: bool,
) {
    let t = *ctx.theme;
    if primary {
        ctx.scene.fill_rounded_rect(
            rect,
            t.radius.small,
            if hovered {
                t.colors.accent.scale_alpha(0.86)
            } else {
                t.colors.accent
            },
        );
    } else {
        ctx.scene
            .fill_rounded_rect(rect, t.radius.small, t.colors.card_bg);
        ctx.scene.stroke_rounded_rect(
            rect,
            t.radius.small,
            1.0,
            if hovered {
                t.colors.accent
            } else {
                t.colors.border
            },
        );
    }
    ctx.label_center(
        rect.center().x,
        rect.center().y,
        label,
        t.typography.body.size,
        weight::MEDIUM,
        if primary {
            t.colors.accent_fg
        } else {
            t.colors.text
        },
    );
}

/// Przełącznik statusu w pasku: sama ikona (opcjonalnie krótka wartość), bez pigułki.
fn paint_toggle_pill(ctx: &mut PaintCtx, rect: Rect, toggle: &Toggle, hovered: bool) {
    let t = ctx.theme;
    if hovered {
        ctx.scene
            .fill_rounded_rect(rect, t.radius.small, libluna_widgets::hover_fill(t));
    }
    let fg = if toggle.active {
        t.colors.accent
    } else {
        t.colors.dim_text
    };
    let isz = 15.0;
    let ix = rect.x() + t.spacing.xs;
    panel_icon(
        ctx,
        toggle.icon,
        Rect::from_xywh(ix, rect.center().y - isz / 2.0, isz, isz),
        fg,
    );
    if let Some(value) = &toggle.value {
        ctx.label_left(
            ix + isz + t.spacing.xs,
            rect.center().y,
            value,
            t.typography.caption.size,
            weight::MEDIUM,
            fg,
        );
    }
}

/// Pigułka segmentu (np. przełącznik widoku launchera).
fn paint_pill(ctx: &mut PaintCtx, rect: Rect, label: &str, active: bool, hovered: bool) {
    let t = ctx.theme;
    let bg = if active {
        t.colors.accent
    } else if hovered {
        libluna_widgets::hover_fill(t)
    } else {
        t.colors.card_bg
    };
    let fg = if active {
        t.colors.accent_fg
    } else {
        t.colors.text
    };
    ctx.scene.fill_rounded_rect(rect, rect.height() / 2.0, bg);
    if active && hovered {
        // Aktywny segment ma już tło akcentu — hover zaznaczamy jaśniejszą obwódką.
        ctx.scene.stroke_rounded_rect(
            rect,
            rect.height() / 2.0,
            2.0,
            t.colors.accent_fg.scale_alpha(0.65),
        );
    }
    if !active {
        ctx.scene
            .stroke_rounded_rect(rect, rect.height() / 2.0, 1.0, t.colors.border);
    }
    ctx.label_center(
        rect.center().x,
        rect.center().y,
        label,
        t.typography.caption.size,
        weight::MEDIUM,
        fg,
    );
}

/// Rozmieszczenie elementów launchera.
#[derive(Debug, Clone)]
pub struct LauncherRects {
    pub header: Rect,
    pub search: Rect,
    /// Przycisk „×” czyszczący frazę (po prawej w polu wyszukiwania).
    pub search_clear: Rect,
    /// Całe segmentowe przełączniki widoku.
    pub switcher: Rect,
    /// Trzy przyciski widoku (kolejność jak [`LauncherView::ALL`]).
    pub views: [Rect; 3],
    /// Obszar na zawartość (lista/siatka/karuzela).
    pub content: Rect,
}

/// Liczy rozmieszczenie launchera w obszarze `area`.
pub fn launcher_rects(area: Rect, theme: &Theme, text: &TextEngine) -> LauncherRects {
    let pad = theme.spacing.lg;
    let header_h = 30.0;
    let header = Rect::from_xywh(
        area.x() + pad,
        area.y() + pad,
        area.width() - 2.0 * pad,
        header_h,
    );

    // Przełącznik widoku po prawej nagłówka.
    let cap = theme.typography.caption.size;
    let gap = theme.spacing.xs;
    let widths: Vec<f64> = LauncherView::ALL
        .iter()
        .map(|v| text.measure(&v.label(), cap, weight::MEDIUM).width + 2.0 * theme.spacing.md)
        .collect();
    let total: f64 = widths.iter().sum::<f64>() + gap * 2.0;
    let mut views = [Rect::ZERO; 3];
    let mut x = header.right() - total;
    for (i, w) in widths.iter().enumerate() {
        views[i] = Rect::from_xywh(x, header.y() + (header_h - 26.0) / 2.0, *w, 26.0);
        x += w + gap;
    }
    let switcher = Rect::from_xywh(header.right() - total, header.y(), total, 26.0);

    let search = Rect::from_xywh(
        area.x() + pad,
        header.bottom() + theme.spacing.sm,
        area.width() - 2.0 * pad,
        32.0,
    );
    let clear_sz = 20.0;
    let search_clear = Rect::from_xywh(
        search.right() - theme.spacing.sm - clear_sz,
        search.center().y - clear_sz / 2.0,
        clear_sz,
        clear_sz,
    );
    let content_y = search.bottom() + theme.spacing.lg;
    let content = Rect::from_xywh(
        area.x() + pad,
        content_y,
        area.width() - 2.0 * pad,
        (area.bottom() - pad - content_y).max(0.0),
    );

    LauncherRects {
        header,
        search,
        search_clear,
        switcher,
        views,
        content,
    }
}

/// Widok odpowiadający kliknięciu w przełącznik (jeśli trafiono).
pub fn launcher_view_at(
    area: Rect,
    theme: &Theme,
    text: &TextEngine,
    p: Point,
) -> Option<LauncherView> {
    let r = launcher_rects(area, theme, text);
    r.views
        .iter()
        .position(|v| v.contains(p))
        .map(|i| LauncherView::ALL[i])
}

/// Czy punkt `p` trafia w przycisk czyszczenia frazy („×”).
///
/// Wołający sprawdza dodatkowo, czy fraza nie jest pusta — przycisku nie ma,
/// gdy nie ma czego czyścić.
pub fn launcher_search_clear_at(area: Rect, theme: &Theme, text: &TextEngine, p: Point) -> bool {
    launcher_rects(area, theme, text).search_clear.contains(p)
}

/// Indeks aplikacji pod punktem `p` (wg aktualnego widoku).
pub fn launcher_app_at(
    apps: &[App],
    launcher: &Launcher,
    area: Rect,
    theme: &Theme,
    text: &TextEngine,
    p: Point,
) -> Option<usize> {
    let filtered = launcher.filter_apps(apps);
    let content = launcher_rects(area, theme, text).content;
    let sy = launcher.scroll;
    let filtered_idx = match launcher.view {
        LauncherView::Grid => (0..filtered.len()).find(|&i| {
            let t = grid_tile(content, i);
            Rect::from_xywh(t.x(), t.y() - sy, t.width(), t.height()).contains(p)
        }),
        LauncherView::List => (0..filtered.len()).find(|&i| {
            let t = list_row(content, i);
            Rect::from_xywh(t.x(), t.y() - sy, t.width(), t.height()).contains(p)
        }),
        LauncherView::Carousel => {
            (0..filtered.len()).find(|&i| carousel_card(content, i, launcher.carousel).contains(p))
        }
    }?;
    let target_app = filtered[filtered_idx];
    apps.iter().position(|a| std::ptr::eq(a, target_app))
}

/// Prostokąt kafla aplikacji o indeksie `index` w `apps` (po filtrze launchera).
///
/// Służy do zakotwiczenia menu kontekstowego (PPM) przy właściwym kaflu —
/// także po przewinięciu listy i w widoku karuzeli.
pub fn launcher_app_rect(
    apps: &[App],
    launcher: &Launcher,
    area: Rect,
    theme: &Theme,
    text: &TextEngine,
    index: usize,
) -> Option<Rect> {
    let target = apps.get(index)?;
    let filtered = launcher.filter_apps(apps);
    let pos = filtered.iter().position(|app| std::ptr::eq(*app, target))?;
    let content = launcher_rects(area, theme, text).content;
    let sy = launcher.scroll;
    let tile = match launcher.view {
        LauncherView::Grid => grid_tile(content, pos),
        LauncherView::List => list_row(content, pos),
        LauncherView::Carousel => carousel_card(content, pos, launcher.carousel),
    };
    Some(Rect::from_xywh(
        tile.x(),
        tile.y() - sy,
        tile.width(),
        tile.height(),
    ))
}

fn grid_tile(content: Rect, i: usize) -> Rect {
    let gap = 12.0;
    let cell_w = (content.width() - gap * (GRID_COLS as f64 - 1.0)) / GRID_COLS as f64;
    let c = (i % GRID_COLS) as f64;
    let r = (i / GRID_COLS) as f64;
    Rect::from_xywh(
        content.x() + c * (cell_w + gap),
        content.y() + r * (GRID_CELL_H + gap),
        cell_w,
        GRID_CELL_H,
    )
}

fn list_row(content: Rect, i: usize) -> Rect {
    let gap = 8.0;
    Rect::from_xywh(
        content.x(),
        content.y() + i as f64 * (LIST_ROW_H + gap),
        content.width(),
        LIST_ROW_H,
    )
}

fn carousel_card(content: Rect, i: usize, selected: usize) -> Rect {
    let gap = 16.0;
    let x = content.center().x - CARD_W / 2.0 + (i as f64 - selected as f64) * (CARD_W + gap);
    Rect::from_xywh(
        x,
        content.y() + (content.height() - CARD_H) / 2.0,
        CARD_W,
        CARD_H,
    )
}

/// Maksymalne pionowe przewinięcie dla danego widoku.
pub fn launcher_scroll_max(
    apps: &[App],
    launcher: &Launcher,
    area: Rect,
    theme: &Theme,
    text: &TextEngine,
) -> f64 {
    let filtered_len = launcher.filter_apps(apps).len();
    let content = launcher_rects(area, theme, text).content;
    let gap_list = 8.0;
    let gap_grid = 12.0;
    let total = match launcher.view {
        LauncherView::List => (filtered_len as f64) * (LIST_ROW_H + gap_list) - gap_list,
        LauncherView::Grid => {
            let rows = filtered_len.div_ceil(GRID_COLS) as f64;
            rows * (GRID_CELL_H + gap_grid) - gap_grid
        }
        LauncherView::Carousel => 0.0,
    };
    (total - content.height()).max(0.0)
}

/// Dopasowuje przewinięcie tak, aby zaznaczona pozycja była widoczna.
pub fn ensure_selection_visible(
    apps: &[App],
    launcher: &mut Launcher,
    area: Rect,
    theme: &Theme,
    text: &TextEngine,
) {
    let filtered_len = launcher.filter_apps(apps).len();
    ensure_selection_visible_count(filtered_len, launcher, area, theme, text);
}

/// Dopasowuje przewinięcie dla zadanej liczby pozycji.
pub fn ensure_selection_visible_count(
    count: usize,
    launcher: &mut Launcher,
    area: Rect,
    theme: &Theme,
    text: &TextEngine,
) {
    let Some(i) = launcher.selected else {
        return;
    };
    if i >= count {
        return;
    }
    let content = launcher_rects(area, theme, text).content;
    let (top, bottom) = match launcher.view {
        LauncherView::Grid => {
            let t = grid_tile(content, i);
            (t.y() - content.y(), t.bottom() - content.y())
        }
        LauncherView::List => {
            let t = list_row(content, i);
            (t.y() - content.y(), t.bottom() - content.y())
        }
        LauncherView::Carousel => return,
    };
    let view_h = content.height();
    if top < launcher.scroll {
        launcher.scroll = top;
    } else if bottom > launcher.scroll + view_h {
        launcher.scroll = bottom - view_h;
    }
    let max = launcher_scroll_max_count(count, launcher, area, theme, text);
    launcher.scroll = launcher.scroll.clamp(0.0, max);
}

/// Liczy maksymalne przewinięcie na podstawie liczby pozycji.
pub fn launcher_scroll_max_count(
    count: usize,
    launcher: &Launcher,
    area: Rect,
    theme: &Theme,
    text: &TextEngine,
) -> f64 {
    let content = launcher_rects(area, theme, text).content;
    let gap_list = 8.0;
    let gap_grid = 12.0;
    let total = match launcher.view {
        LauncherView::List => (count as f64) * (LIST_ROW_H + gap_list) - gap_list,
        LauncherView::Grid => {
            let rows = count.div_ceil(GRID_COLS) as f64;
            rows * (GRID_CELL_H + gap_grid) - gap_grid
        }
        LauncherView::Carousel => 0.0,
    };
    (total - content.height()).max(0.0)
}

/// Rysuje launcher w zadanym prostokącie.
pub fn paint_launcher(
    apps: &[App],
    launcher: &Launcher,
    hover: &Hover,
    icon_theme: IconTheme,
    ctx: &mut PaintCtx,
    area: Rect,
) {
    let t = *ctx.theme;
    let r = launcher_rects(area, &t, ctx.text);

    ctx.scene
        .fill_rounded_rect(area, t.radius.large, t.colors.card_bg);
    ctx.scene
        .stroke_rounded_rect(area, t.radius.large, 1.0, t.colors.border);

    let title = tr("Applications");
    ctx.label_left(
        r.header.x(),
        r.header.center().y,
        &title,
        t.typography.heading.size,
        weight::BOLD,
        t.colors.text,
    );

    // Podpowiedź obok tytułu: jak przypinać aplikacje (PPM). Nie wchodzi pod przełącznik.
    let hint_x =
        r.header.x() + ctx.measure(&title, t.typography.heading.size, weight::BOLD) + t.spacing.lg;
    let hint_max = r.switcher.x() - t.spacing.sm - hint_x;
    if hint_max > 40.0 {
        let hint = ellipsize(
            &tr("Right-click for options"),
            hint_max,
            t.typography.caption.size,
            weight::NORMAL,
            ctx.text,
        );
        ctx.label_left(
            hint_x,
            r.header.center().y,
            &hint,
            t.typography.caption.size,
            weight::NORMAL,
            t.colors.dim_text,
        );
    }

    // Przełącznik widoku.
    for (i, view) in LauncherView::ALL.iter().enumerate() {
        paint_pill(
            ctx,
            r.views[i],
            &view.label(),
            *view == launcher.view,
            hover.is_hovered(r.views[i]),
        );
    }

    // Pole wyszukiwania z aktywnym tekstem i kursorem
    ctx.scene
        .fill_rounded_rect(r.search, t.radius.medium, t.colors.view_bg);
    ctx.scene
        .stroke_rounded_rect(r.search, t.radius.medium, 1.0, t.colors.border);

    let s_icon_sz = 16.0;
    let s_icon_rect = Rect::from_xywh(
        r.search.x() + t.spacing.sm,
        r.search.center().y - s_icon_sz / 2.0,
        s_icon_sz,
        s_icon_sz,
    );
    panel_icon(ctx, IconName::Search, s_icon_rect, t.colors.dim_text);

    let search_x = s_icon_rect.right() + t.spacing.sm;
    if launcher.search.is_empty() {
        ctx.label_left(
            search_x,
            r.search.center().y,
            &tr("Search applications…"),
            t.typography.body.size,
            weight::NORMAL,
            t.colors.dim_text,
        );
    } else {
        // Przycisk „×” po prawej; tekst nie może pod niego wjechać.
        let clear = r.search_clear;
        ctx.scene.fill_rounded_rect(
            clear,
            clear.height() / 2.0,
            if hover.is_hovered(clear) {
                libluna_widgets::hover_fill(&t)
            } else {
                t.colors.shade
            },
        );
        let xsz = 10.0;
        panel_icon(
            ctx,
            IconName::Close,
            Rect::from_xywh(
                clear.center().x - xsz / 2.0,
                clear.center().y - xsz / 2.0,
                xsz,
                xsz,
            ),
            t.colors.text,
        );

        let text_max = (clear.x() - t.spacing.sm - search_x).max(0.0);
        let shown = ellipsize_left(
            &launcher.search,
            text_max,
            t.typography.body.size,
            weight::NORMAL,
            ctx.text,
        );
        ctx.label_left(
            search_x,
            r.search.center().y,
            &shown,
            t.typography.body.size,
            weight::NORMAL,
            t.colors.text,
        );
        let tw = ctx.measure(&shown, t.typography.body.size, weight::NORMAL);
        let cursor_x = (search_x + tw + 2.0).min(text_max + search_x);
        ctx.scene.fill_rect(
            Rect::from_xywh(cursor_x, r.search.center().y - 8.0, 2.0, 16.0),
            t.colors.accent,
        );
    }

    // Filtrowana lista aplikacji
    let filtered = launcher.filter_apps(apps);
    let scroll = launcher.scroll;
    ctx.scene.push_clip(r.content);

    if filtered.is_empty() {
        ctx.label_center(
            r.content.center().x,
            r.content.center().y,
            &tr("No applications found"),
            t.typography.body.size,
            weight::NORMAL,
            t.colors.dim_text,
        );
    } else {
        match launcher.view {
            LauncherView::Grid => {
                for (i, app) in filtered.iter().enumerate() {
                    let t = grid_tile(r.content, i);
                    let rect = Rect::from_xywh(t.x(), t.y() - scroll, t.width(), t.height());
                    paint_grid_tile(
                        app,
                        launcher.selected_index(filtered.len()) == Some(i),
                        launcher.hover == Some(i),
                        launcher.is_pinned(app),
                        icon_theme,
                        ctx,
                        rect,
                    );
                }
            }
            LauncherView::List => {
                for (i, app) in filtered.iter().enumerate() {
                    let t = list_row(r.content, i);
                    let rect = Rect::from_xywh(t.x(), t.y() - scroll, t.width(), t.height());
                    paint_list_row(
                        app,
                        launcher.selected_index(filtered.len()) == Some(i),
                        launcher.hover == Some(i),
                        launcher.is_pinned(app),
                        icon_theme,
                        ctx,
                        rect,
                    );
                }
            }
            LauncherView::Carousel => {
                for (i, app) in filtered.iter().enumerate() {
                    paint_carousel_card(
                        app,
                        i == launcher.carousel,
                        launcher.hover == Some(i),
                        launcher.is_pinned(app),
                        icon_theme,
                        ctx,
                        carousel_card(r.content, i, launcher.carousel),
                    );
                }
            }
        }
    }
    ctx.scene.pop_clip();
}

/// Kafelek siatki: ikona + nazwa; zaznaczony wyróżniony akcentem.
#[allow(clippy::too_many_arguments)]
fn paint_grid_tile(
    app: &App,
    selected: bool,
    hovered: bool,
    pinned: bool,
    icon_theme: IconTheme,
    ctx: &mut PaintCtx,
    rect: Rect,
) {
    let t = ctx.theme;
    let bg = if selected {
        t.colors.accent.scale_alpha(0.12)
    } else {
        t.colors.view_bg
    };
    ctx.scene.fill_rounded_rect(rect, t.radius.medium, bg);
    if hovered && !selected {
        ctx.scene
            .fill_rounded_rect(rect, t.radius.medium, libluna_widgets::hover_fill(t));
    }
    let border = if selected {
        t.colors.accent
    } else if hovered {
        t.colors.accent.scale_alpha(0.55)
    } else {
        t.colors.border
    };
    ctx.scene.stroke_rounded_rect(
        rect,
        t.radius.medium,
        if selected {
            2.0
        } else if hovered {
            1.5
        } else {
            1.0
        },
        border,
    );

    let icon = Rect::from_xywh(rect.center().x - 20.0, rect.y() + 10.0, 40.0, 40.0);
    let (icon_bg, icon_fg) = icon_colors(t, icon_theme);
    if !libluna_widgets::paint_named_icon(ctx, app.theme_icon.as_deref(), icon) {
        ctx.scene.fill_rounded_rect(icon, t.radius.small, icon_bg);
        paint_icon(ctx, app.icon, icon, icon_fg);
    }
    ctx.label_center(
        rect.center().x,
        icon.bottom() + 12.0,
        &app.name,
        t.typography.caption.size,
        weight::NORMAL,
        t.colors.text,
    );
    if pinned {
        let badge = Rect::from_xywh(rect.right() - 26.0, rect.y() + 6.0, 20.0, 20.0);
        ctx.scene
            .fill_circle(badge.center(), badge.width() / 2.0, t.colors.shade);
        panel_icon(
            ctx,
            IconName::Pin,
            Rect::from_xywh(
                badge.x() + 3.0,
                badge.y() + 3.0,
                badge.width() - 6.0,
                badge.height() - 6.0,
            ),
            t.colors.accent,
        );
    }
}

/// Wiersz listy: ikona + nazwa + kategoria; zaznaczony wyróżniony akcentem.
#[allow(clippy::too_many_arguments)]
fn paint_list_row(
    app: &App,
    selected: bool,
    hovered: bool,
    pinned: bool,
    icon_theme: IconTheme,
    ctx: &mut PaintCtx,
    rect: Rect,
) {
    let t = ctx.theme;
    let bg = if selected {
        t.colors.accent.scale_alpha(0.12)
    } else {
        t.colors.view_bg
    };
    ctx.scene.fill_rounded_rect(rect, t.radius.medium, bg);
    if hovered && !selected {
        ctx.scene
            .fill_rounded_rect(rect, t.radius.medium, libluna_widgets::hover_fill(t));
    }
    let border = if selected {
        t.colors.accent
    } else if hovered {
        t.colors.accent.scale_alpha(0.55)
    } else {
        t.colors.border
    };
    ctx.scene.stroke_rounded_rect(
        rect,
        t.radius.medium,
        if selected {
            2.0
        } else if hovered {
            1.5
        } else {
            1.0
        },
        border,
    );

    let icon = Rect::from_xywh(rect.x() + t.spacing.md, rect.center().y - 18.0, 36.0, 36.0);
    let (icon_bg, icon_fg) = icon_colors(t, icon_theme);
    if !libluna_widgets::paint_named_icon(ctx, app.theme_icon.as_deref(), icon) {
        ctx.scene.fill_rounded_rect(icon, t.radius.small, icon_bg);
        paint_icon(ctx, app.icon, icon, icon_fg);
    }
    ctx.label_left(
        icon.right() + t.spacing.md,
        rect.center().y,
        &app.name,
        t.typography.body.size,
        weight::MEDIUM,
        t.colors.text,
    );
    ctx.label_right(
        rect.right() - t.spacing.md,
        rect.center().y,
        &app.category,
        t.typography.caption.size,
        weight::NORMAL,
        t.colors.dim_text,
    );
    if pinned {
        // Pinezka tuż przed kategorią, żeby nie nachodziły na siebie.
        let cat_w = ctx.measure(&app.category, t.typography.caption.size, weight::NORMAL);
        let badge = Rect::from_xywh(
            rect.right() - t.spacing.md - cat_w - t.spacing.sm - 20.0,
            rect.center().y - 10.0,
            20.0,
            20.0,
        );
        ctx.scene
            .fill_circle(badge.center(), badge.width() / 2.0, t.colors.shade);
        panel_icon(
            ctx,
            IconName::Pin,
            Rect::from_xywh(
                badge.x() + 3.0,
                badge.y() + 3.0,
                badge.width() - 6.0,
                badge.height() - 6.0,
            ),
            t.colors.accent,
        );
    }
}

/// Karta karuzeli: duża ikona + nazwa; wybrana wyróżniona akcentem.
#[allow(clippy::too_many_arguments)]
fn paint_carousel_card(
    app: &App,
    selected: bool,
    hovered: bool,
    pinned: bool,
    icon_theme: IconTheme,
    ctx: &mut PaintCtx,
    rect: Rect,
) {
    let t = ctx.theme;
    let bg = if selected {
        t.colors.accent.scale_alpha(0.12)
    } else {
        t.colors.view_bg
    };
    ctx.scene.fill_rounded_rect(rect, t.radius.large, bg);
    if hovered && !selected {
        ctx.scene
            .fill_rounded_rect(rect, t.radius.large, libluna_widgets::hover_fill(t));
    }
    let border = if selected {
        t.colors.accent
    } else if hovered {
        t.colors.accent.scale_alpha(0.55)
    } else {
        t.colors.border
    };
    ctx.scene.stroke_rounded_rect(
        rect,
        t.radius.large,
        if selected {
            2.0
        } else if hovered {
            1.5
        } else {
            1.0
        },
        border,
    );

    let icon = Rect::from_xywh(rect.center().x - 34.0, rect.y() + 36.0, 68.0, 68.0);
    let (icon_bg, icon_fg) = icon_colors(t, icon_theme);
    if !libluna_widgets::paint_named_icon(ctx, app.theme_icon.as_deref(), icon) {
        ctx.scene.fill_rounded_rect(icon, t.radius.medium, icon_bg);
        paint_icon(ctx, app.icon, icon, icon_fg);
    }
    ctx.label_center(
        rect.center().x,
        icon.bottom() + 24.0,
        &app.name,
        t.typography.body.size,
        if selected {
            weight::BOLD
        } else {
            weight::MEDIUM
        },
        t.colors.text,
    );
    ctx.label_center(
        rect.center().x,
        icon.bottom() + 44.0,
        &app.category,
        t.typography.caption.size,
        weight::NORMAL,
        t.colors.dim_text,
    );
    if pinned {
        let badge = Rect::from_xywh(rect.right() - 30.0, rect.y() + 10.0, 22.0, 22.0);
        ctx.scene
            .fill_circle(badge.center(), badge.width() / 2.0, t.colors.shade);
        panel_icon(
            ctx,
            IconName::Pin,
            Rect::from_xywh(
                badge.x() + 4.0,
                badge.y() + 4.0,
                badge.width() - 8.0,
                badge.height() - 8.0,
            ),
            t.colors.accent,
        );
    }
}

/// Kolory tła i ikony dla wybranego stylu ikon.
fn icon_colors(theme: &Theme, icon_theme: IconTheme) -> (Color, Color) {
    if icon_theme.is_monochrome() {
        (theme.colors.shade, theme.colors.text)
    } else {
        (theme.colors.accent.scale_alpha(0.16), theme.colors.accent)
    }
}

/// Pełny podgląd pulpitu: tapeta, panel górny i launcher.
pub fn paint_desktop(
    panel: &PanelState,
    apps: &[App],
    launcher: &Launcher,
    ctx: &mut PaintCtx,
    size: Size,
) {
    let accent = ctx.theme.colors.accent;

    ctx.scene
        .fill_rect(Rect::from_size(size), ctx.theme.colors.window_bg);
    let blob = Point::new(size.width * 0.78, size.height * 0.74);
    ctx.scene.fill_rounded_rect(
        Rect::from_xywh(blob.x - 220.0, blob.y - 220.0, 440.0, 440.0),
        220.0,
        accent.scale_alpha(0.10),
    );

    paint_panel(
        panel,
        &Hover::new(),
        ctx,
        Rect::from_xywh(0.0, 0.0, size.width, THICKNESS),
    );

    let lw = LAUNCHER_W;
    let lh = LAUNCHER_H;
    let lrect = Rect::from_xywh(
        (size.width - lw) / 2.0,
        (size.height - lh) / 2.0 + 16.0,
        lw,
        lh,
    );
    paint_launcher(
        apps,
        launcher,
        &Hover::new(),
        IconTheme::default(),
        ctx,
        lrect,
    );
}

/// Wyciszony kolor akcentu (pomocniczo dla tapet/podświetleń).
pub fn accent_soft(theme: &Theme) -> Color {
    theme.colors.accent.scale_alpha(0.12)
}

#[cfg(test)]
mod app_menu_tests {
    use super::*;

    fn rows() -> Vec<AppMenuRow> {
        vec![
            AppMenuRow {
                label: "Open".into(),
                action: Some(AppMenuAction::Launch),
                enabled: true,
                danger: false,
            },
            AppMenuRow {
                label: "Add to favorites".into(),
                action: Some(AppMenuAction::ToggleFavorite),
                enabled: true,
                danger: false,
            },
            AppMenuRow::separator(),
            AppMenuRow {
                label: "Uninstall".into(),
                action: Some(AppMenuAction::Uninstall),
                enabled: false,
                danger: true,
            },
        ]
    }

    #[test]
    fn menu_hit_test_skips_separator_and_disabled_rows() {
        let theme = Theme::light();
        let anchor = Rect::from_xywh(400.0, 200.0, 120.0, 100.0);
        let bounds = Rect::from_xywh(0.0, 0.0, 1280.0, 680.0);
        let menu = app_menu_rect(anchor, &rows(), bounds, &theme);

        let y = |i: usize| {
            let mut y = menu.y() + 6.0;
            for row in rows().iter().take(i) {
                y += app_menu_row_h(row);
            }
            y + app_menu_row_h(&rows()[i]) / 2.0
        };
        assert_eq!(
            app_menu_item_at(menu, &rows(), Point::new(menu.center().x, y(0))),
            Some(0)
        );
        assert_eq!(
            app_menu_item_at(menu, &rows(), Point::new(menu.center().x, y(1))),
            Some(1)
        );
        assert_eq!(
            app_menu_item_at(menu, &rows(), Point::new(menu.center().x, y(2))),
            None,
            "separator nie może być klikalny"
        );
        assert_eq!(
            app_menu_item_at(menu, &rows(), Point::new(menu.center().x, y(3))),
            None,
            "wyłączona pozycja nie może być klikalna"
        );
    }

    #[test]
    fn menu_opens_on_the_left_when_there_is_no_room_on_the_right() {
        let theme = Theme::light();
        let bounds = Rect::from_xywh(0.0, 0.0, 1280.0, 680.0);
        let anchor = Rect::from_xywh(1200.0, 200.0, 60.0, 60.0);
        let menu = app_menu_rect(anchor, &rows(), bounds, &theme);
        assert!(
            menu.right() <= anchor.x(),
            "menu powinno zmieścić się po lewej"
        );
        assert!(menu.x() >= bounds.x());
        assert!(menu.bottom() <= bounds.bottom());
    }

    #[test]
    fn menu_is_clamped_at_the_bottom_edge() {
        let theme = Theme::light();
        let bounds = Rect::from_xywh(0.0, 0.0, 1280.0, 680.0);
        let anchor = Rect::from_xywh(400.0, 660.0, 120.0, 80.0);
        let menu = app_menu_rect(anchor, &rows(), bounds, &theme);
        assert!(menu.y() >= bounds.y());
        assert!(menu.bottom() <= bounds.bottom());
    }
}

#[cfg(test)]
mod security_ui_tests {
    use super::*;

    #[test]
    fn security_dialog_buttons_are_inside_card_and_hittable() {
        let theme = Theme::light();
        let size = Size::new(1280.0, 680.0);
        let layout = security_dialog_layout(size, &theme);
        assert!(layout.card.x() >= 0.0);
        assert!(layout.card.right() <= size.width);
        assert!(layout.allow.right() <= layout.card.right());
        assert!(layout.deny.x() >= layout.card.x());
        assert_eq!(
            security_dialog_hit(layout, layout.allow.center()),
            Some(SecurityChoice::Allow)
        );
        assert_eq!(
            security_dialog_hit(layout, layout.deny.center()),
            Some(SecurityChoice::Deny)
        );
        assert_eq!(
            security_dialog_hit(
                layout,
                Point::new(layout.card.center().x, layout.card.y() + 4.0)
            ),
            None
        );
    }

    #[test]
    fn security_indicators_default_to_none() {
        assert!(!SecurityIndicators::default().any());
        assert!(SecurityIndicators {
            microphone: true,
            ..Default::default()
        }
        .any());
    }

    #[test]
    fn security_dialog_paints_without_panic() {
        use libluna_render::{Scene, TextEngine};
        let theme = Theme::light();
        let text = TextEngine::none();
        let mut scene = Scene::new();
        let mut ctx = PaintCtx::new(&mut scene, &theme, &text);
        let layout = security_dialog_layout(Size::new(1280.0, 680.0), &theme);
        paint_security_dialog(
            &mut ctx,
            layout,
            IconName::Globe,
            "Firefox",
            &tr("Network"),
            "Firefox chce wysyłać dane poza ten komputer",
            Some("1.1.1.1:443"),
            Some(SecurityChoice::Allow),
        );
        assert!(!scene.is_empty());

        let bar = Rect::from_xywh(0.0, 0.0, 1280.0, THICKNESS);
        let mut panel = PanelState::sample();
        panel.security = SecurityIndicators {
            microphone: true,
            camera: true,
            network: true,
            alert: true,
        };
        let mut scene = Scene::new();
        let mut ctx = PaintCtx::new(&mut scene, &theme, &text);
        paint_panel(&panel, &Hover::new(), &mut ctx, bar);
        assert!(!scene.is_empty());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use libluna_render::{Scene, TextEngine};

    fn area() -> Rect {
        Rect::from_xywh(100.0, 60.0, LAUNCHER_W, LAUNCHER_H)
    }

    #[test]
    fn workspace_dots_hit_test_matches_layout() {
        let theme = Theme::dark();
        let text = TextEngine::none();
        let bar = Rect::from_xywh(0.0, 0.0, 1280.0, THICKNESS);
        let mut st = PanelState::sample();
        st.workspaces = 4;
        st.current_ws = 2;
        let rects = workspace_rects(bar, &theme, &text, &st);
        assert_eq!(rects.len(), 4);
        for (i, r) in rects.iter().enumerate() {
            assert_eq!(workspace_at(bar, &theme, &text, &st, r.center()), Some(i));
        }
        // Kropki idą w lewo (wyższy indeks bliżej prawej) i nie zachodzą na siebie.
        assert!(rects[3].x() > rects[0].x());
        assert!(rects[0].right() <= rects[1].x());
    }

    #[test]
    fn panel_icons_stay_monochrome_in_both_themes() {
        // Ikony paska (w tym przycisk Quick Panelu) nie mogą zależeć od pastelowych
        // ilustracji ani zewnętrznego zestawu PNG — w jasnym motywie ginęły na tle.
        let text = TextEngine::none();
        let count = |theme: &Theme| {
            let mut scene = Scene::new();
            let mut ctx = PaintCtx::new(&mut scene, theme, &text);
            panel_icon(
                &mut ctx,
                IconName::Sliders,
                Rect::from_xywh(0.0, 0.0, PANEL_ICON, PANEL_ICON),
                theme.colors.text,
            );
            scene.len()
        };
        let light = count(&Theme::light());
        let dark = count(&Theme::dark());
        assert!(light > 0, "ikona Quick Panelu musi coś rysować");
        assert_eq!(
            light, dark,
            "ten sam wektor w obu motywach (kolor bierze się z motywu)"
        );
    }

    #[test]
    fn quick_panel_hit_tests_match_layout() {
        let theme = Theme::light();
        let text = TextEngine::system();
        let bar = Rect::from_xywh(0.0, 0.0, 1280.0, THICKNESS);
        let st = PanelState::sample();
        let btn = quick_button_rect(bar, &theme, &text, &st);
        assert!(
            bar.contains(btn.center()),
            "przycisk Quick musi być w pasku"
        );
        let panel = quick_panel_rect(bar, &theme);
        assert!(panel.y() >= bar.bottom());
        assert!(panel.right() <= bar.right());
        let l = quick_layout(panel, &theme);
        assert_eq!(
            quick_hit(panel, &theme, l.header_buttons[0].center()),
            Some(QuickHit::Theme)
        );
        assert_eq!(
            quick_hit(panel, &theme, l.header_buttons[1].center()),
            Some(QuickHit::Settings)
        );
        let tile_hits = [
            QuickHit::Wifi,
            QuickHit::Bluetooth,
            QuickHit::Airplane,
            QuickHit::Dnd,
            QuickHit::NightLight,
            QuickHit::PowerSaver,
        ];
        for (r, hit) in l.tiles.iter().zip(tile_hits) {
            assert_eq!(quick_hit(panel, &theme, r.center()), Some(hit));
        }
        assert_eq!(
            quick_hit(panel, &theme, l.mute.center()),
            Some(QuickHit::Mute)
        );
        let p = Point::new(l.volume.x() + l.volume.width() * 0.25, l.volume.center().y);
        match quick_hit(panel, &theme, p) {
            Some(QuickHit::Volume(v)) => assert!((23..=27).contains(&v), "{v}"),
            other => panic!("oczekiwano Volume, jest {other:?}"),
        }
        let p = Point::new(
            l.brightness.x() + l.brightness.width() * 0.75,
            l.brightness.center().y,
        );
        match quick_hit(panel, &theme, p) {
            Some(QuickHit::Brightness(v)) => assert!((73..=77).contains(&v), "{v}"),
            other => panic!("oczekiwano Brightness, jest {other:?}"),
        }
        assert_eq!(
            quick_hit(panel, &theme, l.media_buttons[0].center()),
            Some(QuickHit::MediaPrevious)
        );
        assert_eq!(
            quick_hit(panel, &theme, l.media_buttons[1].center()),
            Some(QuickHit::MediaPlayPause)
        );
        assert_eq!(
            quick_hit(panel, &theme, l.media_buttons[2].center()),
            Some(QuickHit::MediaNext)
        );
        assert_eq!(
            quick_hit(panel, &theme, l.session[0].center()),
            Some(QuickHit::Lock)
        );
        assert_eq!(
            quick_hit(panel, &theme, l.session[1].center()),
            Some(QuickHit::Logout)
        );
        assert_eq!(
            quick_hit(panel, &theme, l.session[2].center()),
            Some(QuickHit::Reboot)
        );
        assert_eq!(
            quick_hit(panel, &theme, l.session[3].center()),
            Some(QuickHit::PowerOff)
        );
        assert_eq!(
            quick_hit(
                panel,
                &theme,
                Point::new(panel.right() + 40.0, panel.center().y)
            ),
            None
        );
    }

    #[test]
    #[ignore = "generuje podgląd do docs/images (uruchom recznie)"]
    fn render_quick_panel_preview() {
        use libluna_render::{Scene, SoftwareRenderer};
        let text = TextEngine::system();
        let width = 1920.0;
        let section = QUICK_H + THICKNESS + 28.0;
        let size = Size::new(width, section * 2.0);
        let mut scene = Scene::new();
        let state = PanelState::sample();
        for (i, theme) in [Theme::light(), Theme::dark()].into_iter().enumerate() {
            let top = i as f64 * section;
            scene.fill_rect(
                Rect::from_xywh(0.0, top, width, section),
                theme.colors.window_bg,
            );
            let mut ctx = PaintCtx::new(&mut scene, &theme, &text);
            let bar = Rect::from_xywh(0.0, top, width, THICKNESS);
            paint_panel(&state, &Hover::new(), &mut ctx, bar);
            let q = quick_panel_rect(bar, &theme);
            let data = QuickData {
                user: "patryks",
                preference: ThemePreference::System,
                dark: i == 1,
                wifi_on: true,
                ssid: Some("Dom_5G"),
                bluetooth: Some((true, 2)),
                airplane: false,
                dnd: false,
                night_light: 60,
                gamma_available: true,
                power_saver: false,
                power_fallback: true,
                volume: 65,
                muted: false,
                brightness: Some(80),
                sink: Some("Głośniki wbudowane"),
                battery: Some(BatteryStatus {
                    percent: 82,
                    charging: false,
                    minutes: Some(200),
                }),
                media: Some(QuickMedia {
                    title: "Nazwa utworu",
                    artist: "Wykonawca",
                    playing: true,
                }),
            };
            let qrect = Rect::from_xywh(q.x(), bar.bottom() + 12.0, q.width(), q.height());
            // Podgląd pokazuje hover na kafelku Wi-Fi (spójny z resztą motywu).
            let mut hover = Hover::new();
            hover.set_position(quick_layout(qrect, &theme).tiles[0].center());
            paint_quick_panel(&mut ctx, qrect, &data, &hover);
        }
        let mut renderer = SoftwareRenderer::new(size.width as u32, size.height as u32);
        renderer.draw(&scene, 1.0, Theme::light().colors.window_bg);
        let png = renderer.encode_png().expect("PNG");
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../docs/images/quick-panel.png"
        );
        std::fs::write(path, png).expect("zapis PNG");
        println!("saved {path}");
    }

    #[test]
    fn power_menu_items_hit_test() {
        let theme = Theme::dark();
        let bar = Rect::from_xywh(0.0, 0.0, 1280.0, THICKNESS);
        let n = 3;
        let menu = power_menu_rect(bar, &theme, n);
        let row = |i: usize| {
            Rect::from_xywh(
                menu.x() + 6.0,
                menu.y() + 6.0 + i as f64 * POWER_ITEM_H,
                menu.width() - 12.0,
                POWER_ITEM_H,
            )
        };
        for i in 0..n {
            assert_eq!(power_item_at(bar, &theme, n, row(i).center()), Some(i));
        }
        // Poza pozycjami (róg menu) brak trafienia.
        assert_eq!(
            power_item_at(bar, &theme, n, Point::new(menu.x() + 1.0, menu.y() + 1.0)),
            None
        );
        // Przycisk zasilania mieści się w pasku, przy prawej krawędzi.
        let p = power_rect(bar, &theme);
        assert!(p.right() <= bar.right());
        assert!(bar.contains(p.center()));
    }

    fn panel_commands(view: LauncherView) -> usize {
        let theme = Theme::light();
        let text = TextEngine::none();
        let mut scene = Scene::new();
        let mut ctx = PaintCtx::new(&mut scene, &theme, &text);
        paint_panel(
            &PanelState::sample(),
            &Hover::new(),
            &mut ctx,
            Rect::from_xywh(0.0, 0.0, 1280.0, THICKNESS),
        );
        let launcher = Launcher {
            view,
            carousel: 2,
            selected: None,
            scroll: 0.0,
            search: String::new(),
            hover: None,
            ..Launcher::default()
        };
        paint_launcher(
            &App::sample(),
            &launcher,
            &Hover::new(),
            IconTheme::default(),
            &mut ctx,
            area(),
        );
        scene.len()
    }

    #[test]
    fn panel_has_no_activities_label_but_burger_button() {
        // Panel ma burger po lewej; kropki przestrzeni i tytuł po prawej od niego.
        let theme = Theme::light();
        let panel = Rect::from_xywh(0.0, 0.0, 1280.0, THICKNESS);
        let burger = burger_rect(panel, &theme);
        assert!(panel.contains(burger.center()));
        assert_eq!(burger.width(), 28.0);
    }

    #[test]
    fn view_switcher_cycles_and_hit_tests() {
        let theme = Theme::light();
        let text = TextEngine::none();
        let r = launcher_rects(area(), &theme, &text);
        for (i, view) in LauncherView::ALL.iter().enumerate() {
            assert_eq!(
                launcher_view_at(area(), &theme, &text, r.views[i].center()),
                Some(*view)
            );
        }
        assert_eq!(LauncherView::List.next(), LauncherView::Grid);
        assert_eq!(LauncherView::Carousel.next(), LauncherView::List);
    }

    #[test]
    fn list_scroll_is_clamped_and_offsets_hit_test() {
        let theme = Theme::light();
        let text = TextEngine::none();
        let apps = App::sample();
        let mut launcher = Launcher {
            view: LauncherView::List,
            carousel: 0,
            selected: None,
            scroll: 0.0,
            search: String::new(),
            hover: None,
            ..Launcher::default()
        };
        let max = launcher_scroll_max(&apps, &launcher, area(), &theme, &text);
        assert!(max > 0.0, "lista 12 pozycji powinna się przewijać");

        // Po przewinięciu o jeden wiersz pierwsza pozycja jest nad obszarem → trafiamy w drugą.
        launcher.scroll = (LIST_ROW_H + 8.0).min(max);
        let r = launcher_rects(area(), &theme, &text);
        let top = Point::new(r.content.x() + 5.0, r.content.y() + 5.0);
        assert_eq!(
            launcher_app_at(&apps, &launcher, area(), &theme, &text, top),
            Some(1)
        );
    }

    #[test]
    fn all_three_views_paint() {
        assert!(panel_commands(LauncherView::List) > 20);
        assert!(panel_commands(LauncherView::Grid) > 20);
        assert!(panel_commands(LauncherView::Carousel) > 20);
    }

    #[test]
    fn launcher_search_clear_button_hit_test() {
        let theme = Theme::dark();
        let text = TextEngine::none();
        let area = area();
        let r = launcher_rects(area, &theme, &text);
        assert!(r.search.contains(r.search_clear.center()));
        assert!(launcher_search_clear_at(
            area,
            &theme,
            &text,
            r.search_clear.center()
        ));
        // Poza przyciskiem (np. przy lupce) nie ma trafienia.
        assert!(!launcher_search_clear_at(
            area,
            &theme,
            &text,
            Point::new(r.search.x() + 24.0, r.search.center().y),
        ));
    }

    #[test]
    fn clear_search_resets_query_scroll_and_selection() {
        let mut launcher = Launcher::new();
        launcher.search = "term".to_owned();
        launcher.scroll = 120.0;
        launcher.selected = Some(4);
        launcher.clear_search();
        assert!(launcher.search.is_empty());
        assert_eq!(launcher.scroll, 0.0);
        assert_eq!(launcher.selected, Some(0));
    }

    #[test]
    fn app_hit_test_finds_tiles_in_each_view() {
        let theme = Theme::light();
        let text = TextEngine::none();
        let apps = App::sample();
        for view in LauncherView::ALL {
            let launcher = Launcher {
                view,
                carousel: 2,
                selected: None,
                scroll: 0.0,
                search: String::new(),
                hover: None,
                ..Launcher::default()
            };
            let r = launcher_rects(area(), &theme, &text);
            // Punkt wnętrza znanej pozycji (pierwszy kafel/wiersz, albo środek wybranej karty).
            let (p, expected) = match view {
                LauncherView::Grid | LauncherView::List => {
                    (Point::new(r.content.x() + 5.0, r.content.y() + 5.0), 0)
                }
                LauncherView::Carousel => (r.content.center(), 2),
            };
            assert_eq!(
                launcher_app_at(&apps, &launcher, area(), &theme, &text, p),
                Some(expected),
                "widok {view:?}"
            );
        }
    }

    #[test]
    fn keyboard_navigation_moves_selection_per_view() {
        // Lista: +1/-1 po indeksie, z zawijaniem.
        let mut list = Launcher {
            view: LauncherView::List,
            ..Launcher::default()
        };
        assert_eq!(list.selected_index(0), None);
        list.navigate(5, 0, 1);
        assert_eq!(list.selected_index(5), Some(1));
        list.navigate(5, 0, -1);
        list.navigate(5, 0, -1);
        assert_eq!(list.selected_index(5), Some(4)); // zawija

        // Siatka: pion = ±GRID_COLS, poziom = ±1.
        let mut grid = Launcher {
            view: LauncherView::Grid,
            ..Launcher::default()
        };
        grid.navigate(24, 0, 1);
        assert_eq!(grid.selected_index(24), Some(GRID_COLS));
        grid.navigate(24, 1, 0);
        assert_eq!(grid.selected_index(24), Some(GRID_COLS + 1));
        grid.navigate(24, 0, -5);
        assert_eq!(grid.selected_index(24), Some(1)); // nie wychodzi nad wiersz 0

        // Karuzela: steruje indeksem karty.
        let mut car = Launcher {
            view: LauncherView::Carousel,
            ..Launcher::default()
        };
        car.navigate(6, 1, 0);
        assert_eq!(car.selected_index(6), Some(1));
        assert_eq!(car.carousel, 1);
    }

    #[test]
    fn selection_scrolls_into_view() {
        let theme = Theme::light();
        let text = TextEngine::none();
        let apps = App::sample();
        let mut launcher = Launcher {
            view: LauncherView::List,
            ..Launcher::default()
        };
        // Wybierz ostatnią pozycję i upewnij się, że lista się przewinęła.
        launcher.navigate(apps.len(), 0, -1);
        ensure_selection_visible(&apps, &mut launcher, area(), &theme, &text);
        assert!(
            launcher.scroll > 0.0,
            "zaznaczenie poza ekranem ma przewijać"
        );
    }

    #[test]
    fn clock_and_user_rects_and_session_hit() {
        let theme = Theme::dark();
        let text = TextEngine::none();
        let bar = Rect::from_xywh(0.0, 0.0, 1280.0, THICKNESS);
        let st = PanelState::sample();

        let cr = clock_rect(bar, &theme, &text, &st);
        assert!(bar.contains(cr.center()));

        let ur = user_rect(bar, &theme, &text, &st);
        assert!(bar.contains(ur.center()));

        let items = crate::power::items();
        let srect = session_menu_rect(bar, &theme);
        assert_eq!(
            session_item_at(bar, &theme, Point::new(srect.x() + 10.0, srect.y() + 20.0)),
            Some(items[0].action)
        );
    }

    #[test]
    fn every_session_item_is_inside_the_menu_and_clickable() {
        let theme = Theme::dark();
        let bar = Rect::from_xywh(0.0, 0.0, 1280.0, THICKNESS);
        let menu = session_menu_rect(bar, &theme);
        let items = crate::power::items();
        assert_eq!(items.len(), crate::power::ITEM_COUNT);
        for (i, item) in items.iter().enumerate() {
            let p = Point::new(
                menu.x() + 20.0,
                menu.y() + 6.0 + i as f64 * POWER_ITEM_H + POWER_ITEM_H / 2.0,
            );
            assert!(
                menu.contains(p),
                "pozycja {i} ({:?}) wypada poza menu",
                item.action
            );
            assert_eq!(session_item_at(bar, &theme, p), Some(item.action));
        }
    }

    #[test]
    fn volume_and_wifi_popup_hit_tests() {
        let theme = Theme::dark();
        let text = TextEngine::none();
        let bar = Rect::from_xywh(0.0, 0.0, 1280.0, THICKNESS);
        let st = PanelState::sample();

        let vrect = volume_popup_rect(bar, &theme, &text, &st);
        assert!(vrect.width() > 100.0);
        let vhit = volume_popup_hit(
            vrect,
            &theme,
            Point::new(vrect.center().x, vrect.y() + 56.0),
        );
        assert!(matches!(vhit, Some(VolumeAction::SetLevel(_))));

        let wrect = wifi_popup_rect(bar, &theme, &text, &st);
        assert!(wrect.width() > 100.0);
        let networks = vec![crate::network::WifiNetwork {
            ssid: "TestNet".into(),
            signal: 80,
            security: crate::network::Security::Wpa2,
            active: false,
            device: "/dev".into(),
            device_name: "wlp1".into(),
            ap_path: "/ap".into(),
            frequency: 2412,
            known: false,
        }];
        let whit = wifi_popup_hit(
            wrect,
            &theme,
            Point::new(wrect.x() + 20.0, wrect.y() + 90.0),
            None,
            &networks,
        );
        assert_eq!(whit, Some(WifiAction::SelectNetwork("TestNet".into())));
    }

    #[test]
    fn calendar_days_and_weekdays_calculation() {
        assert_eq!(days_in_month(2026, 10), 31);
        assert_eq!(days_in_month(2024, 2), 29); // leap year
        assert_eq!(days_in_month(2025, 2), 28); // non-leap year

        // 2026-10-01 to czwartek -> indeks 3 (0=Pn, 1=Wt, 2=Śr, 3=Cz)
        assert_eq!(first_weekday_of_month(2026, 10), 3);

        let theme = Theme::dark();
        let bar = Rect::from_xywh(0.0, 0.0, 1280.0, THICKNESS);
        let cal_rect = calendar_popup_rect(bar, &theme);
        assert!(cal_rect.width() > 200.0);

        let prev_hit = calendar_popup_hit(
            cal_rect,
            &theme,
            Point::new(cal_rect.x() + 20.0, cal_rect.y() + 85.0),
            2026,
            10,
        );
        assert_eq!(prev_hit, Some(CalendarAction::PrevMonth));

        let open = calendar_open_button_rect(cal_rect, &theme);
        assert_eq!(
            calendar_popup_hit(cal_rect, &theme, open.center(), 2026, 10),
            Some(CalendarAction::OpenApp)
        );
    }

    #[test]
    fn calendar_popup_paints_events_and_open_button() {
        let theme = Theme::dark();
        let text = TextEngine::none();
        let bar = Rect::from_xywh(0.0, 0.0, 1280.0, THICKNESS);
        let rect = calendar_popup_rect(bar, &theme);
        let events = vec![CalendarEvent {
            date: "2026-10-08".to_owned(),
            start_minute: Some(9 * 60 + 30),
            title: "Lekarz".to_owned(),
        }];
        let mut scene = Scene::new();
        let mut ctx = PaintCtx::new(&mut scene, &theme, &text);
        paint_calendar_popup_with_events(
            &mut ctx,
            rect,
            "12:00",
            "czwartek, 08.10.2026",
            2026,
            10,
            2026,
            10,
            8,
            Some(8),
            &events,
            "PL",
            true,
        );
        assert!(
            scene.len() > 20,
            "kalendarz z planem dnia powinien coś narysować"
        );
    }

    #[test]
    fn notification_center_hit_tests_match_layout() {
        let theme = Theme::dark();
        let bar = Rect::from_xywh(0.0, 0.0, 1280.0, THICKNESS);
        let cal = calendar_popup_rect(bar, &theme);
        let rect = notification_center_rect(bar, &theme, 2);
        assert!(rect.y() >= bar.bottom());
        // Na 1280 px karta mieści się obok kalendarza, bez nakładania.
        assert!(
            rect.x() >= cal.right(),
            "centrum nie może zasłaniać kalendarza"
        );
        assert!(rect.right() <= bar.right());

        let l = notification_layout(rect, &theme, 2);
        assert_eq!(l.rows.len(), 2);
        assert_eq!(
            notification_center_hit(rect, &theme, 2, l.clear.center()),
            Some(NotificationHit::ClearAll)
        );
        assert_eq!(
            notification_center_hit(rect, &theme, 2, l.rows[0].center()),
            Some(NotificationHit::Row(0))
        );
        assert_eq!(
            notification_center_hit(rect, &theme, 2, l.rows[1].center()),
            Some(NotificationHit::Row(1))
        );
        // Puste centrum nie ma aktywnego „Wyczyść”.
        assert_eq!(
            notification_center_hit(rect, &theme, 0, l.clear.center()),
            None
        );
        assert_eq!(
            notification_center_hit(
                rect,
                &theme,
                2,
                Point::new(rect.x() + 2.0, rect.bottom() - 2.0),
            ),
            None
        );
    }

    #[test]
    fn notification_center_paints_history_and_empty_state() {
        use crate::notifications::{Notification, Notifications};

        let theme = Theme::dark();
        let text = TextEngine::none();
        let bar = Rect::from_xywh(0.0, 0.0, 1280.0, THICKNESS);

        let mut notes = Notifications::new();
        notes
            .push(Notification::new("Pliki", "Kopiowanie", "12 plików").with_icon(IconName::Files));
        notes.push(Notification::new("System", "Aktualizacja", "Gotowe"));
        let rect = notification_center_rect(bar, &theme, notes.history_len());
        let mut scene = Scene::new();
        let mut ctx = PaintCtx::new(&mut scene, &theme, &text);
        paint_notification_center(&mut ctx, rect, notes.history(), false, Some(0));
        assert!(scene.len() > 12, "historia powinna narysować wiersze");

        let empty = Notifications::new();
        let mut scene = Scene::new();
        let mut ctx = PaintCtx::new(&mut scene, &theme, &text);
        paint_notification_center(&mut ctx, rect, empty.history(), true, None);
        assert!(scene.len() > 5, "pusty stan też jest rysowany");
    }

    /// Benchmark powłoki: pełny koszt klatki `chrome` (1920x680) — render sceny plus
    /// konwersja RGBA→BGRA do `wl_shm` (ta sama co w `libluna-platform::present`).
    ///
    /// Uruchom: `cargo test --release -p luna bench_shell_chrome -- --nocapture --ignored`
    #[test]
    #[ignore = "benchmark reczny"]
    fn bench_shell_chrome_1920x680() {
        use libluna_core::{Color, Size};
        use libluna_render::SoftwareRenderer;
        use libluna_widgets::PaintCtx;
        use std::time::Instant;

        let (w, h) = (1920u32, 680u32);
        let theme = Theme::dark();
        let text = TextEngine::system();
        let apps = App::sample();
        let mut launcher = Launcher::new();
        launcher.view = LauncherView::Grid;
        let mut st = PanelState::sample();
        st.tasks.clear();

        let bar = Rect::from_xywh(0.0, 0.0, w as f64, THICKNESS);
        let larea = Rect::from_xywh(
            (w as f64 - LAUNCHER_W) / 2.0,
            THICKNESS + 24.0,
            LAUNCHER_W,
            LAUNCHER_H,
        );

        let mut renderer = SoftwareRenderer::new(w, h);
        let mut scene = Scene::new();
        let n = 60u32;

        // 1) Render sceny (to, co robi `luna-shell` w każdej klatce animacji).
        let t = Instant::now();
        for _ in 0..n {
            scene.clear();
            let mut ctx = PaintCtx::new(&mut scene, &theme, &text);
            ctx.scene.fill_rect(
                Rect::from_size(Size::new(w as f64, h as f64)),
                Color::from_rgba8(0, 0, 0, 75),
            );
            paint_panel(&st, &Hover::new(), &mut ctx, bar);
            paint_launcher(
                &apps,
                &launcher,
                &Hover::new(),
                IconTheme::default(),
                &mut ctx,
                larea,
            );
            renderer.draw(&scene, 1.0, Color::rgba(0.0, 0.0, 0.0, 0.0));
        }
        let ms_render = t.elapsed().as_secs_f64() * 1000.0 / n as f64;

        // 2) Konwersja do BGRA (jak `present()` w `libluna-platform`).
        let mut dst = vec![0u8; w as usize * h as usize * 4];
        let pixels = renderer.data();
        let t = Instant::now();
        for _ in 0..n {
            for (d, s) in dst.chunks_exact_mut(4).zip(pixels.chunks_exact(4)) {
                d[0] = s[2];
                d[1] = s[1];
                d[2] = s[0];
                d[3] = s[3];
            }
        }
        let ms_swizzle = t.elapsed().as_secs_f64() * 1000.0 / n as f64;

        // 3) Dowód regresji: ta sama klatka **bez czyszczenia sceny** — komendy sumują
        //    się i koszt rośnie liniowo (tak powłoka spadała do 1–5 FPS po sekundzie).
        scene.clear();
        let mut no_clear_ms = 0.0;
        for frame in 0..120u32 {
            let mut ctx = PaintCtx::new(&mut scene, &theme, &text);
            ctx.scene.fill_rect(
                Rect::from_size(Size::new(w as f64, h as f64)),
                Color::from_rgba8(0, 0, 0, 75),
            );
            paint_panel(&st, &Hover::new(), &mut ctx, bar);
            paint_launcher(
                &apps,
                &launcher,
                &Hover::new(),
                IconTheme::default(),
                &mut ctx,
                larea,
            );
            let t = Instant::now();
            renderer.draw(&scene, 1.0, Color::rgba(0.0, 0.0, 0.0, 0.0));
            if frame == 119 {
                no_clear_ms = t.elapsed().as_secs_f64() * 1000.0;
            }
        }

        println!(
            "shell chrome 1920x680: render {ms_render:.2} ms + swizzle {ms_swizzle:.2} ms = {:.2} ms/klatke ({} fps)",
            ms_render + ms_swizzle,
            if ms_render + ms_swizzle > 0.0 {
                1000.0 / (ms_render + ms_swizzle)
            } else {
                0.0
            }
        );
        println!(
            "  [regresja] 120. klatka BEZ clear(): {no_clear_ms:.1} ms ({:.1} fps)",
            if no_clear_ms > 0.0 {
                1000.0 / no_clear_ms
            } else {
                0.0
            }
        );
    }

    // ── Regresje układu paska ───────────────────────────────────────────────────

    /// Pigułki przełączników muszą iść jedna za drugą (bez nachodzenia) i kończyć się
    /// przed prawą krawędzią paska. Pigułka bez wartości tekstowej mieści samą ikonę.
    #[test]
    fn toggle_pills_never_overlap() {
        let theme = Theme::dark();
        let text = TextEngine::system();
        let bar = Rect::from_xywh(0.0, 0.0, 1920.0, THICKNESS);
        let st = PanelState::sample();
        let rects = toggle_rects(bar, &theme, &text, &st);
        assert_eq!(rects.len(), st.toggles.len());
        for w in rects.windows(2) {
            assert!(
                w[0].right() <= w[1].x(),
                "pigułki nachodzą: {:?} / {:?}",
                w[0],
                w[1]
            );
        }
        assert!(rects.last().unwrap().right() <= bar.right() - theme.spacing.md + 0.5);
        // Sieć i dźwięk: sama ikona (bez nazwy sieci / „Dźwięk”).
        let wifi = &st.toggles[0];
        assert!(wifi.value.is_none());
        assert!(rects[0].width() < 40.0, "pigułka Wi-Fi powinna być wąska");
    }

    /// Kropki przestrzeni roboczych leżą **na lewo** od całej grupy pigułek i nie
    /// nachodzą na siebie ani na pigułki (wcześniej rysowały się na pigułce dźwięku).
    #[test]
    fn workspace_dots_sit_left_of_toggles() {
        let theme = Theme::dark();
        let text = TextEngine::system();
        let bar = Rect::from_xywh(0.0, 0.0, 1920.0, THICKNESS);
        let mut st = PanelState::sample();
        st.workspaces = 4;
        st.current_ws = 2;
        let toggles = toggle_rects(bar, &theme, &text, &st);
        let dots = workspace_rects(bar, &theme, &text, &st);
        assert_eq!(dots.len(), 4);
        assert!(dots.last().unwrap().right() <= toggles[0].x());
        for w in dots.windows(2) {
            assert!(w[0].right() <= w[1].x());
        }
        for (i, d) in dots.iter().enumerate() {
            assert_eq!(workspace_at(bar, &theme, &text, &st, d.center()), Some(i));
        }
    }

    /// Kliknięcie w listę/siatkę trafia we właściwą aplikację także po przewinięciu.
    #[test]
    fn launcher_click_respects_scroll() {
        let theme = Theme::dark();
        let text = TextEngine::system();
        let area = Rect::from_xywh(100.0, 60.0, LAUNCHER_W, LAUNCHER_H);
        let apps = App::sample();
        let content = launcher_rects(area, &theme, &text).content;
        let probe = Rect::from_xywh(content.x() + 8.0, content.y() + 8.0, 8.0, 8.0).center();

        let mut l = Launcher::new();
        l.view = LauncherView::List;
        assert_eq!(
            launcher_app_at(&apps, &l, area, &theme, &text, probe),
            Some(0)
        );
        l.scroll = LIST_ROW_H + 8.0;
        assert_eq!(
            launcher_app_at(&apps, &l, area, &theme, &text, probe),
            Some(1)
        );
    }

    /// Nawigacja strzałkami w siatce: w bok po kolumnach, w dół po wierszach,
    /// bez wychodzenia poza zakres.
    #[test]
    fn launcher_grid_navigation() {
        let mut l = Launcher::new();
        l.view = LauncherView::Grid;
        l.selected = Some(0);
        l.navigate(12, 1, 0);
        assert_eq!(l.selected, Some(1));
        l.navigate(12, -1, 0);
        assert_eq!(l.selected, Some(0));
        l.navigate(12, 0, 1);
        assert_eq!(l.selected, Some(GRID_COLS));
        l.selected = Some(GRID_COLS);
        l.navigate(12, -1, 0);
        assert_eq!(l.selected, Some(GRID_COLS));
    }

    /// Przewijanie kółkiem: jeden ząbek ≈ jeden wiersz, ograniczone do zakresu.
    #[test]
    fn launcher_wheel_scroll_is_bounded() {
        let mut l = Launcher::new();
        l.view = LauncherView::List;
        l.scroll_by_notches(1.0, 64.0, 500.0);
        assert_eq!(l.scroll, 64.0);
        l.scroll_by_notches(1.0, 64.0, 500.0);
        assert_eq!(l.scroll, 128.0);
        l.scroll_by_notches(-5.0, 64.0, 500.0);
        assert_eq!(l.scroll, 0.0);
        l.scroll_by_notches(100.0, 64.0, 500.0);
        assert_eq!(l.scroll, 500.0);
    }

    /// Przypięcie przenosi aplikację na początek listy, odpina przywraca porządek,
    /// a wyszukiwanie nadal znajduje przypięte pozycje.
    #[test]
    fn pinned_apps_move_to_top() {
        let apps = App::sample();
        let pinned = &apps[6]; // Terminal
        let mut l = Launcher::new();
        assert!(!l.is_pinned(pinned));
        assert!(l.toggle_pin(pinned));
        assert!(l.is_pinned(pinned));

        let filtered = l.filter_apps(&apps);
        assert_eq!(filtered.len(), apps.len(), "przypięcie nie gubi pozycji");
        assert!(std::ptr::eq(filtered[0], pinned));

        l.search = "term".to_owned();
        let hits = l.filter_apps(&apps);
        assert!(!hits.is_empty());
        assert!(std::ptr::eq(hits[0], pinned));
        l.search.clear();

        assert!(!l.toggle_pin(pinned));
        let filtered = l.filter_apps(&apps);
        assert!(std::ptr::eq(filtered[0], &apps[0]));
    }

    /// Ostatnio używane: bez duplikatów, najnowsza pierwsza, po przypiętych.
    #[test]
    fn recent_apps_follow_pinned() {
        let apps = App::sample();
        let mut l = Launcher::new();
        l.remember_used(&apps[2]);
        l.remember_used(&apps[3]);
        l.remember_used(&apps[2]);
        assert_eq!(
            l.recent,
            vec![apps[2].key(), apps[3].key()],
            "powtórne uruchomienie nie tworzy duplikatu"
        );

        let filtered = l.filter_apps(&apps);
        assert!(std::ptr::eq(filtered[0], &apps[2]));
        assert!(std::ptr::eq(filtered[1], &apps[3]));

        // Przypięta wygrywa kolejnością z „ostatnio używanymi”.
        l.toggle_pin(&apps[3]);
        let filtered = l.filter_apps(&apps);
        assert!(std::ptr::eq(filtered[0], &apps[3]));
        assert!(std::ptr::eq(filtered[1], &apps[2]));
    }

    /// Lista „ostatnio używane” jest krótka i wypycha najstarsze wpisy.
    #[test]
    fn recent_list_is_limited() {
        let apps = App::sample();
        assert!(apps.len() > Launcher::RECENT_LIMIT);
        let mut l = Launcher::new();
        for app in &apps {
            l.remember_used(app);
        }
        assert_eq!(l.recent.len(), Launcher::RECENT_LIMIT);
        assert_eq!(l.recent[0], apps[apps.len() - 1].key());
        assert!(!l.recent.contains(&apps[0].key()));
    }

    fn tray_item(i: usize) -> TrayItem {
        TrayItem {
            service: format!(":1.{i}"),
            path: "/StatusNotifierItem".to_owned(),
            id: format!("app{i}"),
            title: format!("App {i}"),
            status: TrayStatus::Active,
            icon: TrayIcon::None,
            menu: None,
        }
    }

    #[test]
    fn tray_layout_shows_overflow_after_limit() {
        let theme = Theme::light();
        let text = TextEngine::system();
        let bar = Rect::from_xywh(0.0, 0.0, 1920.0, THICKNESS);
        let mut st = PanelState::sample();
        st.tray = (0..6).map(tray_item).collect();
        let layout = tray_layout(bar, &theme, &text, &st);
        assert_eq!(layout.icons.len(), TRAY_VISIBLE_MAX - 1);
        assert_eq!(layout.hidden_from, TRAY_VISIBLE_MAX - 1);
        let overflow = layout.overflow.expect("overflow");
        assert!(bar.contains(overflow.center()));
        assert_eq!(
            tray_at(bar, &theme, &text, &st, overflow.center()),
            Some(TrayHit::Overflow)
        );
        let (rect, index) = layout.icons[1];
        assert_eq!(index, 1);
        assert_eq!(
            tray_at(bar, &theme, &text, &st, rect.center()),
            Some(TrayHit::Icon(1))
        );
    }

    #[test]
    fn tray_overflow_menu_lists_hidden_items() {
        let theme = Theme::light();
        let text = TextEngine::system();
        let bar = Rect::from_xywh(0.0, 0.0, 1920.0, THICKNESS);
        let mut st = PanelState::sample();
        st.tray = (0..6).map(tray_item).collect();
        let rect = tray_menu_rect(bar, &theme, &text, &st);
        assert!(rect.height() > 0.0);
        let hidden_from = TRAY_VISIBLE_MAX - 1;
        let p = Point::new(rect.x() + 20.0, rect.y() + 6.0 + TRAY_ROW_H * 0.5);
        assert_eq!(tray_menu_hit(bar, &theme, &text, &st, p), Some(hidden_from));
    }
}
