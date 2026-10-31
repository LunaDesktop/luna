//! Powiadomienia (toasty) w powłoce Luny — kolejka, animacja, rysowanie i historia.
//!
//! To warstwa prezentacji: przyjmuje gotowe powiadomienie, animuje wejście, po
//! upływie czasu samoistnie je zamyka. Każde powiadomienie trafia też do **historii**
//! centrum powiadomień (karta obok kalendarza, otwierana kliknięciem w zegar).
//! Miejsce stosu (róg ekranu) jest ustawieniem użytkownika (`notifications.conf`).
//! Usługa D-Bus `org.freedesktop.Notifications` żyje w [`crate::dbus_notifications`].

use std::path::PathBuf;

use libluna_core::style::weight;
use libluna_core::{config, Point, Rect, Theme};
use libluna_render::TextEngine;
use libluna_widgets::{paint_icon, IconName, PaintCtx};

/// Czas animacji wejścia/wyjścia (s).
const SLIDE: f64 = 0.22;
/// Odstęp powiadomień od krawędzi ekranu (żeby wyglądały jak karta w rogu).
const MARGIN: f64 = 12.0;
/// Ile powiadomień trzymamy w historii centrum (najstarsze wypadają).
pub const HISTORY_MAX: usize = 50;

/// Szerokość pojedynczego powiadomienia.
pub const WIDTH: f64 = 340.0;
/// Wysokość pojedynczego powiadomienia.
pub const HEIGHT: f64 = 96.0;
/// Odstęp pionowy między powiadomieniami.
const GAP: f64 = 10.0;
/// Wysokość przycisku akcji.
const ACTION_H: f64 = 24.0;
/// Margines przycisków akcji od krawędzi.
const ACTION_PAD: f64 = 12.0;
/// Odstęp między przyciskami akcji.
const ACTION_GAP: f64 = 8.0;

/// Róg ekranu, w którym pojawiają się toasty.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Placement {
    #[default]
    TopRight,
    TopLeft,
    BottomRight,
    BottomLeft,
}

impl Placement {
    pub const ALL: [Placement; 4] = [
        Placement::TopLeft,
        Placement::TopRight,
        Placement::BottomLeft,
        Placement::BottomRight,
    ];

    pub const fn name(self) -> &'static str {
        match self {
            Placement::TopRight => "top-right",
            Placement::TopLeft => "top-left",
            Placement::BottomRight => "bottom-right",
            Placement::BottomLeft => "bottom-left",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|p| p.name() == name)
    }

    pub const fn is_left(self) -> bool {
        matches!(self, Placement::TopLeft | Placement::BottomLeft)
    }

    pub const fn is_bottom(self) -> bool {
        matches!(self, Placement::BottomLeft | Placement::BottomRight)
    }
}

/// Plik ustawień powiadomień: `$XDG_CONFIG_HOME/luna/notifications.conf`.
pub fn config_path() -> Option<PathBuf> {
    config::config_file("notifications.conf")
}

/// Miejsce toastów zapisane przez Ustawienia (domyślnie prawy górny róg).
pub fn load_placement() -> Placement {
    config_path()
        .and_then(|path| std::fs::read_to_string(path).ok())
        .and_then(|text| {
            config::parse_pairs(&text)
                .into_iter()
                .find(|(key, _)| key == "position")
                .and_then(|(_, value)| Placement::from_name(&value))
        })
        .unwrap_or_default()
}

/// Zapisuje miejsce toastów (bez uprawnień administratora).
pub fn save_placement(placement: Placement) -> std::io::Result<()> {
    let Some(path) = config_path() else {
        return Ok(());
    };
    config::save_atomic(
        &path,
        &format!(
            "# Luna — powiadomienia\n# top-left | top-right | bottom-left | bottom-right\nposition = {}\n",
            placement.name()
        ),
    )
}

/// Pojedyncze powiadomienie.
#[derive(Debug, Clone)]
pub struct Notification {
    /// Nazwa aplikacji (np. „Pliki”).
    pub app: String,
    pub title: String,
    pub body: String,
    /// Ikona z pakietu Luny.
    pub icon: IconName,
    /// Czas życia (s); 0 = nie zamykaj automatycznie.
    pub timeout: f64,
    /// Identyfikator z magistrali D‑Bus (0 = powiadomienie lokalne).
    pub id: u32,
    /// Klucze i etykiety akcji (`ActionInvoked`); puste, gdy brak akcji.
    pub actions: Vec<(String, String)>,
    /// Czy serwer wysłał już `NotificationClosed` dla tego wpisu historii.
    ///
    /// Pozwala uniknąć podwójnego sygnału, gdy powiadomienie wygasło, a potem
    /// użytkownik czyści centrum powiadomień.
    pub closed: bool,
}

impl Notification {
    pub fn new(app: impl Into<String>, title: impl Into<String>, body: impl Into<String>) -> Self {
        Self {
            app: app.into(),
            title: title.into(),
            body: body.into(),
            icon: IconName::Info,
            timeout: 5.0,
            id: 0,
            actions: Vec::new(),
            closed: false,
        }
    }

    pub fn with_icon(mut self, icon: IconName) -> Self {
        self.icon = icon;
        self
    }

    /// Ustawia identyfikator z magistrali (do wysłania `NotificationClosed`).
    pub fn with_id(mut self, id: u32) -> Self {
        self.id = id;
        self
    }

    /// Ustawia akcje jako pary `(klucz, etykieta)` (do wysłania `ActionInvoked`).
    pub fn with_actions(mut self, actions: Vec<(String, String)>) -> Self {
        self.actions = actions;
        self
    }
}

/// Kolejka powiadomień z animacją oraz historia dla centrum powiadomień.
#[derive(Default)]
pub struct Notifications {
    /// Widoczne toasty (powiadomienie, wiek w sekundach).
    items: Vec<(Notification, f64)>,
    /// Wszystkie powiadomienia od startu sesji (najstarsze pierwsze).
    history: Vec<Notification>,
    /// Róg ekranu, w którym rysujemy toasty.
    placement: Placement,
}

impl Notifications {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn placement(&self) -> Placement {
        self.placement
    }

    pub fn set_placement(&mut self, placement: Placement) {
        self.placement = placement;
    }

    /// Czy stos powiadomień nie jest pusty (potrzebna animacja/przerysowanie).
    pub fn visible(&self) -> bool {
        !self.items.is_empty()
    }

    /// Dodaje powiadomienie na wierzch stosu i zapisuje je w historii.
    ///
    /// Wpis o tym samym ID (np. `replaces_id` z D-Busa) zastępuje poprzedni
    /// toast i wpis w historii.
    pub fn push(&mut self, n: Notification) {
        self.forget_id(n.id);
        self.record(n.clone());
        self.items.push((n, 0.0));
    }

    /// Usuwa wszystkie wpisy o danym ID (toast + historia).
    fn forget_id(&mut self, id: u32) {
        if id == 0 {
            return;
        }
        self.items.retain(|(n, _)| n.id != id);
        self.history.retain(|n| n.id != id);
    }

    /// Usuwa powiadomienie o danym ID z toastów i historii.
    ///
    /// Zwraca `true`, jeśli cokolwiek usunięto (np. dla `CloseNotification`).
    pub fn remove_by_id(&mut self, id: u32) -> bool {
        if id == 0 {
            return false;
        }
        let before = self.items.len() + self.history.len();
        self.forget_id(id);
        self.items.len() + self.history.len() != before
    }

    /// Dopisuje powiadomienie wyłącznie do historii (bez toasta).
    ///
    /// Używane w trybie „nie przeszkadzać”, żeby powiadomienie nie zniknęło
    /// zupełnie — czeka w centrum powiadomień.
    pub fn record(&mut self, n: Notification) {
        if n.id != 0 {
            self.history.retain(|h| h.id != n.id);
        }
        self.history.push(n);
        if self.history.len() > HISTORY_MAX {
            self.history.remove(0);
        }
    }

    /// Usuwa toasty **oraz** historię (np. przy włączaniu trybu „nie przeszkadzać”).
    pub fn clear(&mut self) {
        self.items.clear();
        self.history.clear();
    }

    /// Usuwa tylko widoczne toasty — historia zostaje w centrum.
    ///
    /// Zwraca zdjęte toasty, żeby powłoka mogła wysłać im `NotificationClosed`.
    pub fn clear_toasts(&mut self) -> Vec<Notification> {
        let toasts: Vec<Notification> = std::mem::take(&mut self.items)
            .into_iter()
            .map(|(n, _)| n)
            .collect();
        for n in &toasts {
            if n.id != 0 {
                if let Some(h) = self.history.iter_mut().find(|h| h.id == n.id) {
                    h.closed = true;
                }
            }
        }
        toasts
    }

    /// Historia powiadomień (najstarsze pierwsze).
    pub fn history(&self) -> &[Notification] {
        &self.history
    }

    /// Liczba powiadomień w historii.
    pub fn history_len(&self) -> usize {
        self.history.len()
    }

    /// Powiadomienie z historii (indeks liczony od najstarszego).
    pub fn history_get(&self, i: usize) -> Option<&Notification> {
        self.history.get(i)
    }

    /// Usuwa wpis z historii.
    pub fn remove_history(&mut self, i: usize) {
        if i < self.history.len() {
            self.history.remove(i);
        }
    }

    /// Usuwa całą historię (centrum powiadomień).
    pub fn clear_history(&mut self) {
        self.history.clear();
    }

    /// Przesuwa czas i usuwa wygasłe powiadomienia z toastów.
    ///
    /// Zwraca wygasłe powiadomienia (nadal obecne w historii), żeby powłoka mogła
    /// wysłać `NotificationClosed` z powodem „wygasło” tylko raz.
    pub fn advance(&mut self, dt: f64) -> Vec<Notification> {
        for (_, age) in &mut self.items {
            *age += dt;
        }
        let mut expired = Vec::new();
        self.items.retain(|(n, age)| {
            let keep = n.timeout <= 0.0 || *age < n.timeout;
            if !keep {
                expired.push(n.clone());
            }
            keep
        });
        if !expired.is_empty() {
            for n in &expired {
                if n.id != 0 {
                    if let Some(h) = self.history.iter_mut().find(|h| h.id == n.id) {
                        h.closed = true;
                    }
                }
            }
        }
        expired
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Indeks powiadomienia pod punktem `p` (od wierzchu stosu).
    pub fn hit(&self, area: Rect, p: Point) -> Option<usize> {
        (0..self.items.len())
            .rev()
            .find(|&i| self.rect(area, i).contains(p))
    }

    /// Powiadomienie o indeksie `i`.
    pub fn get(&self, i: usize) -> Option<&Notification> {
        self.items.get(i).map(|(n, _)| n)
    }

    /// Usuwa toast o indeksie `i` (bez ruszania historii).
    pub fn remove(&mut self, i: usize) {
        if i < self.items.len() {
            self.items.remove(i);
        }
    }

    /// Zamyka powiadomienie: usuwa toast i odpowiadający mu wpis historii.
    pub fn dismiss(&mut self, i: usize) {
        let Some((n, _)) = self.items.get(i) else {
            return;
        };
        let id = n.id;
        self.items.remove(i);
        if id != 0 {
            self.history.retain(|h| h.id != id);
        }
    }

    /// Prostokąt `i`-tego powiadomienia w kolumnie zakotwiczonej w wybranym rogu.
    pub fn rect(&self, area: Rect, i: usize) -> Rect {
        Self::rect_in(area, i, self.placement)
    }

    /// Prostokąt `i`-tego powiadomienia dla jawnego położenia (testy/podglądy).
    pub fn rect_in(area: Rect, i: usize, placement: Placement) -> Rect {
        let x = if placement.is_left() {
            area.x() + MARGIN
        } else {
            area.right() - WIDTH - MARGIN
        };
        let y = if placement.is_bottom() {
            area.bottom() - MARGIN - HEIGHT - i as f64 * (HEIGHT + GAP)
        } else {
            area.y() + MARGIN + i as f64 * (HEIGHT + GAP)
        };
        Rect::from_xywh(x, y, WIDTH, HEIGHT)
    }

    /// Prostokąty przycisków akcji toasta `i` (od prawej do lewej, u dołu).
    pub fn action_rects(
        &self,
        area: Rect,
        i: usize,
        theme: &Theme,
        text: &TextEngine,
    ) -> Vec<Rect> {
        let Some((n, _)) = self.items.get(i) else {
            return Vec::new();
        };
        if n.actions.is_empty() {
            return Vec::new();
        }
        let rect = self.rect(area, i);
        let cap = theme.typography.caption.size;
        let y = rect.bottom() - ACTION_PAD - ACTION_H;
        let mut right = rect.right() - ACTION_PAD;
        let mut out = vec![Rect::ZERO; n.actions.len()];
        for (k, (_, label)) in n.actions.iter().enumerate().rev() {
            let w = text.measure(label, cap, weight::MEDIUM).width + 2.0 * theme.spacing.sm;
            let r = Rect::from_xywh(right - w, y, w, ACTION_H);
            out[k] = r;
            right = r.x() - ACTION_GAP;
        }
        out
    }

    /// Trafienie przycisku akcji: `(indeks toasta, indeks akcji)`.
    pub fn action_at(
        &self,
        area: Rect,
        p: Point,
        theme: &Theme,
        text: &TextEngine,
    ) -> Option<(usize, usize)> {
        for i in (0..self.items.len()).rev() {
            if let Some(k) = self
                .action_rects(area, i, theme, text)
                .iter()
                .position(|r| r.contains(p))
            {
                return Some((i, k));
            }
        }
        None
    }

    /// Prostokąt obejmujący cały stos powiadomień (do regionu wejścia powierzchni).
    pub fn stack_rect(&self, area: Rect) -> Rect {
        Self::stack_rect_for(area, self.items.len(), self.placement)
    }

    /// Wariant dla jawnego położenia (testy/podglądy).
    pub fn stack_rect_for(area: Rect, count: usize, placement: Placement) -> Rect {
        let n = count.max(1) as f64;
        let x = if placement.is_left() {
            area.x()
        } else {
            area.right() - WIDTH - MARGIN
        };
        let h = MARGIN + n * (HEIGHT + GAP);
        let y = if placement.is_bottom() {
            area.bottom() - h
        } else {
            area.y()
        };
        Rect::from_xywh(x, y, WIDTH + MARGIN, h)
    }

    /// Rysuje stos powiadomień w obszarze `area` (od góry, do prawej).
    pub fn paint(&self, ctx: &mut PaintCtx, area: Rect) {
        let t = ctx.theme;
        for (i, (n, age)) in self.items.iter().enumerate().rev() {
            let mut rect = self.rect(area, i);
            // Wjazd z boku + fade.
            let p = (*age / SLIDE).clamp(0.0, 1.0);
            let slide = (1.0 - p) * 28.0 * if self.placement.is_left() { -1.0 } else { 1.0 };
            rect = Rect::from_xywh(rect.x() + slide, rect.y(), rect.width(), rect.height());
            let a = p as f32;

            ctx.scene
                .fill_rounded_rect(rect, t.radius.medium, t.colors.card_bg.scale_alpha(a));
            ctx.scene.stroke_rounded_rect(
                rect,
                t.radius.medium,
                1.0,
                t.colors.border.scale_alpha(a),
            );

            let pad = t.spacing.md;
            let icon = Rect::from_xywh(rect.x() + pad, rect.y() + pad, 32.0, 32.0);
            ctx.scene.fill_rounded_rect(
                icon,
                t.radius.small,
                t.colors.accent.scale_alpha(0.18 * a),
            );
            let inner = Rect::from_xywh(icon.x() + 7.0, icon.y() + 7.0, 18.0, 18.0);
            paint_icon(ctx, n.icon, inner, t.colors.accent.scale_alpha(a));

            let tx = icon.right() + pad;
            ctx.label_left(
                tx,
                rect.y() + 26.0,
                &n.title,
                t.typography.body.size,
                weight::MEDIUM,
                t.colors.text.scale_alpha(a),
            );
            ctx.label_left(
                tx,
                rect.y() + 48.0,
                &n.body,
                t.typography.caption.size,
                weight::NORMAL,
                t.colors.dim_text.scale_alpha(a),
            );
            ctx.label_right(
                rect.right() - pad,
                rect.y() + 26.0,
                &n.app,
                t.typography.caption.size,
                weight::NORMAL,
                t.colors.dim_text.scale_alpha(a),
            );

            // Przyciski akcji (np. „Otwórz”, „Odpisz”).
            if !n.actions.is_empty() {
                let mut acts = self.action_rects(area, i, t, ctx.text);
                for ((_, label), r) in n.actions.iter().zip(acts.iter_mut()) {
                    *r = Rect::from_xywh(r.x() + slide, r.y(), r.width(), r.height());
                    ctx.scene.fill_rounded_rect(
                        *r,
                        r.height() / 2.0,
                        t.colors.accent.scale_alpha(a),
                    );
                    ctx.label_center(
                        r.center().x,
                        r.center().y,
                        label,
                        t.typography.caption.size,
                        weight::MEDIUM,
                        t.colors.accent_fg.scale_alpha(a),
                    );
                }
            }

            // Pasek czasu (kurczy się) — tylko gdy nie ma akcji.
            if n.timeout > 0.0 && n.actions.is_empty() {
                let frac = (1.0 - (*age / n.timeout)).clamp(0.0, 1.0);
                let track = Rect::from_xywh(
                    rect.x() + pad,
                    rect.bottom() - 8.0,
                    rect.width() - 2.0 * pad,
                    3.0,
                );
                ctx.scene
                    .fill_rounded_rect(track, 1.5, t.colors.shade.scale_alpha(a));
                let fill =
                    Rect::from_xywh(track.x(), track.y(), track.width() * frac, track.height());
                ctx.scene
                    .fill_rounded_rect(fill, 1.5, t.colors.accent.scale_alpha(a));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use libluna_core::Theme;
    use libluna_render::{Scene, TextEngine};

    #[test]
    fn expires_after_timeout() {
        let mut n = Notifications::new();
        n.push(Notification::new("Pliki", "Kopiowanie", "12 plików"));
        assert_eq!(n.len(), 1);
        n.advance(3.0);
        assert_eq!(n.len(), 1);
        n.advance(3.0);
        assert!(n.is_empty());
    }

    #[test]
    fn zero_timeout_is_sticky() {
        let mut n = Notifications::new();
        let mut item = Notification::new("System", "Aktualizacja", "W toku");
        item.timeout = 0.0;
        n.push(item);
        n.advance(100.0);
        assert_eq!(n.len(), 1);
    }

    #[test]
    fn push_with_same_id_replaces_toast_and_history() {
        let mut n = Notifications::new();
        n.push(Notification::new("A", "stare", "b").with_id(5));
        n.push(Notification::new("A", "nowe", "b").with_id(5));
        assert_eq!(n.len(), 1, "replaces_id nie tworzy drugiego toasta");
        assert_eq!(n.history_len(), 1, "ani drugiego wpisu historii");
        assert_eq!(n.history()[0].title, "nowe");
        assert_eq!(n.get(0).unwrap().title, "nowe");
    }

    #[test]
    fn remove_by_id_removes_toast_and_history() {
        let mut n = Notifications::new();
        n.push(Notification::new("A", "t", "b").with_id(5));
        assert!(n.remove_by_id(5));
        assert!(n.is_empty());
        assert_eq!(n.history_len(), 0);
        assert!(!n.remove_by_id(5), "drugie zamknięcie nic nie zmienia");
        assert!(!n.remove_by_id(0), "ID 0 nie jest używane");
    }

    #[test]
    fn clear_removes_all_toasts() {
        let mut n = Notifications::new();
        n.push(Notification::new("A", "t", "b"));
        n.push(Notification::new("B", "t", "b"));
        n.clear();
        assert!(n.is_empty());
        assert_eq!(n.history_len(), 0);
    }

    #[test]
    fn history_survives_toast_timeout() {
        let mut n = Notifications::new();
        n.push(Notification::new("Pliki", "Kopiowanie", "12 plików").with_id(9));
        let expired = n.advance(100.0);
        assert_eq!(expired.len(), 1);
        assert_eq!(expired[0].id, 9);
        assert!(n.is_empty(), "toast znika po czasie");
        assert_eq!(n.history_len(), 1, "ale zostaje w centrum");
        assert!(n.history()[0].closed, "wygasły wpis jest już zamknięty");
    }

    #[test]
    fn record_skips_toast_but_keeps_history() {
        let mut n = Notifications::new();
        n.record(Notification::new("A", "t", "b"));
        assert!(!n.visible());
        assert_eq!(n.history_len(), 1);
    }

    #[test]
    fn dismiss_removes_toast_and_history() {
        let mut n = Notifications::new();
        n.push(Notification::new("A", "t", "b").with_id(7));
        n.dismiss(0);
        assert!(n.is_empty());
        assert_eq!(n.history_len(), 0);
    }

    #[test]
    fn history_is_bounded() {
        let mut n = Notifications::new();
        for i in 0..(HISTORY_MAX + 5) {
            n.push(Notification::new("A", format!("t{i}"), "b"));
        }
        assert_eq!(n.history_len(), HISTORY_MAX);
        assert_eq!(n.history()[0].title, "t5", "najstarsze wypadają");
        assert_eq!(
            n.history().last().unwrap().title,
            format!("t{}", HISTORY_MAX + 4)
        );
    }

    #[test]
    fn clear_history_leaves_toasts_alone() {
        let mut n = Notifications::new();
        n.push(Notification::new("A", "t", "b"));
        n.clear_history();
        assert_eq!(n.history_len(), 0);
        assert!(n.visible());
    }

    #[test]
    fn rects_stack_downward_from_top_right() {
        let area = Rect::from_xywh(0.0, 40.0, 1280.0, 200.0);
        let p = Placement::TopRight;
        let a = Notifications::rect_in(area, 0, p);
        let b = Notifications::rect_in(area, 1, p);
        assert_eq!(a.right(), area.right() - MARGIN);
        assert!(b.y() > a.y());
    }

    #[test]
    fn placement_mirrors_columns_and_rows() {
        let area = Rect::from_xywh(0.0, 40.0, 1280.0, 700.0);
        let tl = Notifications::rect_in(area, 0, Placement::TopLeft);
        assert_eq!(tl.x(), area.x() + MARGIN);
        let br = Notifications::rect_in(area, 0, Placement::BottomRight);
        assert_eq!(br.right(), area.right() - MARGIN);
        assert_eq!(br.bottom(), area.bottom() - MARGIN);
        let bl = Notifications::rect_in(area, 0, Placement::BottomLeft);
        assert_eq!(bl.x(), area.x() + MARGIN);
        assert_eq!(bl.bottom(), area.bottom() - MARGIN);
    }

    #[test]
    fn stack_rect_covers_visible_toasts_only() {
        let area = Rect::from_xywh(0.0, 48.0, 1920.0, 700.0);
        for placement in Placement::ALL {
            let r = Notifications::stack_rect_for(area, 2, placement);
            assert!(r.contains(Notifications::rect_in(area, 0, placement).center()));
            assert!(r.contains(Notifications::rect_in(area, 1, placement).center()));
            // Poza stosem nie blokujemy kliknięć okien.
            assert!(!r.contains(Point::new(500.0, 200.0)));
        }
    }

    #[test]
    fn hit_and_remove_by_point() {
        let mut n = Notifications::new();
        n.push(
            Notification::new("A", "t", "b")
                .with_id(7)
                .with_actions(vec![("default".to_owned(), "Otwórz".to_owned())]),
        );
        n.advance(0.3);
        let area = Rect::from_xywh(0.0, 40.0, 1280.0, 200.0);
        let rect = Notifications::rect_in(area, 0, Placement::TopRight);
        assert_eq!(n.hit(area, rect.center()), Some(0));
        assert_eq!(n.hit(area, Point::new(-10.0, -10.0)), None);
        let n0 = n.get(0).unwrap();
        assert_eq!(n0.id, 7);
        assert_eq!(
            n0.actions,
            vec![("default".to_owned(), "Otwórz".to_owned())]
        );
        n.remove(0);
        assert!(n.is_empty());
    }

    #[test]
    fn action_buttons_hit_test() {
        let theme = Theme::dark();
        let text = TextEngine::none();
        let area = Rect::from_xywh(0.0, 40.0, 1280.0, 300.0);
        let mut n = Notifications::new();
        n.push(Notification::new("A", "t", "b").with_actions(vec![
            ("open".to_owned(), "Otwórz".to_owned()),
            ("dismiss".to_owned(), "Zamknij".to_owned()),
        ]));
        n.advance(0.3);
        let rects = n.action_rects(area, 0, &theme, &text);
        assert_eq!(rects.len(), 2);
        assert_eq!(
            n.action_at(area, rects[0].center(), &theme, &text),
            Some((0, 0))
        );
        assert_eq!(
            n.action_at(area, rects[1].center(), &theme, &text),
            Some((0, 1))
        );
        // Środek toasta (poza przyciskami) nie jest trafieniem akcji.
        assert_eq!(
            n.action_at(
                area,
                Notifications::rect_in(area, 0, Placement::TopRight).center(),
                &theme,
                &text
            ),
            None
        );
    }

    #[test]
    fn paints_without_window() {
        let theme = Theme::dark();
        let text = TextEngine::none();
        let mut scene = Scene::new();
        let mut ctx = PaintCtx::new(&mut scene, &theme, &text);
        let mut n = Notifications::new();
        n.push(Notification::new("Pliki", "Kopiowanie", "12 plików").with_icon(IconName::Files));
        n.advance(0.3);
        n.paint(&mut ctx, Rect::from_xywh(0.0, 40.0, 1280.0, 200.0));
        assert!(scene.len() > 5);
    }
}
