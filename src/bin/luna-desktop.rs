//! Pulpit Luny — ikony plików, folderów i skrótów `.desktop`.
//!
//! Działa jako powierzchnia warstwowa **pod oknami** (`Layer::Background`):
//! rysuje tylko ikony, a resztę zostawia przezroczystą, więc tapeta kompozytora
//! pozostaje widoczna. Kliknięcia w puste miejsce nie przechodzą do okien —
//! pulpit jest warstwą najniższą, więc nic pod nim nie ma.
//!
//! Zachowanie:
//! - dwuklik otwiera plik (`gio open`), folder (`luna-files`) albo skrót (`gio launch`),
//! - przeciągnięcie ikony zmienia jej pozycję (zapis w `desktop.conf`),
//! - `Shift` + przeciągnięcie wysyła plik do innej aplikacji (Wayland DnD),
//! - upuszczenie plików z Plików kopiuje/przenosi je na pulpit,
//! - klik prawym przyciskiem otwiera menu kontekstowe.
//!
//! Uruchom: `cargo run -p luna --bin luna-desktop`.

use std::time::{Duration, Instant};

use libluna_core::desktop::{self, DesktopEntry, DesktopItemKind, DesktopSettings};
use libluna_core::{ButtonState, Event, Point, PointerButton, Rect, ScrollEvent, Size, Theme};
use libluna_platform::{run, AppConfig, Frame, InputRegion, LayerConfig};
use libluna_widgets::PaintCtx;
use luna::desktop as view;
use luna::panel::THICKNESS;

/// Margines pulpitu względem krawędzi ekranu.
const MARGIN: f64 = 16.0;
/// Odstęp między kliknięciami traktowanymi jako dwuklik.
const DOUBLE_CLICK: Duration = Duration::from_millis(420);
/// Próg ruchu rozpoczynający przeciąganie.
const DRAG_THRESHOLD: f64 = 6.0;
/// Jak często sprawdzamy zmiany w katalogach.
const RESCAN_EVERY: Duration = Duration::from_secs(1);

/// Pozycja menu kontekstowego.
struct Menu {
    at: Point,
    target: Option<usize>,
    rows: Vec<MenuRow>,
}

struct MenuRow {
    label: &'static str,
    action: MenuAction,
}

#[derive(Clone, Copy)]
enum MenuAction {
    Open,
    ShowInFiles,
    Trash,
    NewFolder,
    Refresh,
}

/// Trwające wciśnięcie przycisku na ikonie.
struct Press {
    index: Option<usize>,
    start: Point,
    dragging: bool,
    shift: bool,
    exported: bool,
}

struct DesktopApp {
    settings: DesktopSettings,
    positions: desktop::DesktopPositions,
    entries: Vec<DesktopEntry>,
    next_scan: Instant,
    /// Ostatnia znana modyfikacja `desktop.conf` (zmiany z Ustawień).
    settings_stamp: Option<std::time::SystemTime>,
    scroll: f64,
    pointer: Option<Point>,

    hovered: Option<usize>,
    selected: Option<usize>,
    last_click: Option<(usize, Instant)>,
    press: Option<Press>,
    menu: Option<Menu>,
    /// Pliki przeciągane nad pulpit (podświetlenie miejsca docelowego).
    drop_hint: Option<Point>,
}

impl DesktopApp {
    fn new() -> Self {
        let settings = DesktopSettings::load();
        Self {
            positions: desktop::load_positions(),
            settings,
            entries: Vec::new(),
            next_scan: Instant::now(),
            settings_stamp: settings_stamp(),
            scroll: 0.0,
            pointer: None,
            hovered: None,
            selected: None,
            last_click: None,
            press: None,
            menu: None,
            drop_hint: None,
        }
    }

    /// Obszar siatki (pod paskiem Luny, z marginesami).
    fn area(&self, size: Size) -> Rect {
        Rect::from_xywh(
            MARGIN,
            THICKNESS + MARGIN,
            (size.width - 2.0 * MARGIN).max(1.0),
            (size.height - THICKNESS - 2.0 * MARGIN).max(1.0),
        )
    }

    /// Obszar układania z uwzględnieniem przewijania i liczby wpisów.
    fn layout(&self, size: Size) -> (Vec<view::Placed>, Rect) {
        let area = self.area(size);
        let grid = self.settings.grid.max(64.0) as f64;
        let cols = view::columns(area, grid);
        let needed_rows = self.entries.len().div_ceil(cols.max(1));
        let content_h = (needed_rows as f64 * grid).max(area.height());
        let content = Rect::from_xywh(area.x(), area.y() - self.scroll, area.width(), content_h);
        let placed = view::layout(&self.entries, content, &self.settings, &self.positions);
        (placed, area)
    }

    fn max_scroll(&self, size: Size) -> f64 {
        let area = self.area(size);
        let grid = self.settings.grid.max(64.0) as f64;
        let cols = view::columns(area, grid);
        let needed_rows = self.entries.len().div_ceil(cols.max(1));
        (needed_rows as f64 * grid - area.height()).max(0.0)
    }

    fn rescan(&mut self) {
        // Domyślny katalog pulpitu może jeszcze nie istnieć (świeże konto) —
        // tworzymy go, żeby „Dodaj do pulpitu” i upuszczanie plików miały cel.
        if let Some(first) = self.settings.folders.first() {
            if !first.is_dir() {
                let _ = std::fs::create_dir_all(first);
            }
        }
        let stamp = settings_stamp();
        if stamp != self.settings_stamp {
            // Zmiana z Ustawień: przeładuj katalogi, rozmiar ikon i pozycje.
            self.settings = DesktopSettings::load();
            self.positions = desktop::load_positions();
            self.settings_stamp = stamp;
        }
        self.entries = desktop::scan(&self.settings.folders);
        self.next_scan = Instant::now() + RESCAN_EVERY;
    }

    fn save_positions(&mut self) {
        if desktop::save(&self.settings, &self.positions).is_ok() {
            self.settings_stamp = settings_stamp();
        }
    }

    fn open_at(&self, index: Option<usize>, placed: &[view::Placed]) {
        if let Some(item) = index.and_then(|i| placed.get(i)) {
            view::open(&item.entry);
        }
    }

    fn menu_for(&self, target: Option<usize>, at: Point, placed: &[view::Placed]) -> Menu {
        let mut rows = Vec::new();
        match target.and_then(|i| placed.get(i)) {
            Some(item) => {
                rows.push(MenuRow {
                    label: "Otwórz",
                    action: MenuAction::Open,
                });
                if item.entry.kind != DesktopItemKind::Dir {
                    rows.push(MenuRow {
                        label: "Pokaż w Plikach",
                        action: MenuAction::ShowInFiles,
                    });
                }
                rows.push(MenuRow {
                    label: "Przenieś do kosza",
                    action: MenuAction::Trash,
                });
            }
            None => {
                rows.push(MenuRow {
                    label: "Nowy folder",
                    action: MenuAction::NewFolder,
                });
                rows.push(MenuRow {
                    label: "Odśwież",
                    action: MenuAction::Refresh,
                });
            }
        }
        Menu { at, target, rows }
    }

    fn run_menu_action(&mut self, action: MenuAction, placed: &[view::Placed]) {
        let target = self.menu.as_ref().and_then(|menu| menu.target);
        match action {
            MenuAction::Open => self.open_at(target, placed),
            MenuAction::ShowInFiles => {
                if let Some(item) = target.and_then(|i| placed.get(i)) {
                    let _ = std::process::Command::new("luna-files")
                        .arg(&item.entry.path)
                        .spawn();
                }
            }
            MenuAction::Trash => {
                if let Some(item) = target.and_then(|i| placed.get(i)) {
                    let _ = view::trash(&item.entry.path);
                    self.positions.remove(&item.entry.path);
                    self.save_positions();
                }
            }
            MenuAction::NewFolder => {
                if let Some(folder) = self.settings.folders.first().cloned() {
                    let _ = view::new_folder(&folder);
                }
            }
            MenuAction::Refresh => {}
        }
        self.menu = None;
        self.rescan();
    }

    fn handle(&mut self, frame: &mut Frame, placed: &[view::Placed]) {
        let size = frame.size;
        for event in frame.events {
            match event {
                Event::PointerMotion { position, .. } => {
                    self.pointer = Some(*position);
                    if let Some(press) = &mut self.press {
                        if !press.dragging && distance(press.start, *position) > DRAG_THRESHOLD {
                            press.dragging = true;
                        }
                        if press.dragging && press.shift && !press.exported {
                            if let Some(index) = press.index {
                                if let Some(item) = placed.get(index) {
                                    frame.start_file_drag(vec![item.entry.path.clone()]);
                                    press.exported = true;
                                }
                            }
                        }
                    } else {
                        self.hovered = view::hit_test(placed, *position);
                    }
                }
                Event::PointerLeave => {
                    self.pointer = None;
                    self.hovered = None;
                }
                Event::PointerButton(button) => {
                    if button.button != PointerButton::Left && button.button != PointerButton::Right
                    {
                        continue;
                    }
                    if button.state != ButtonState::Pressed {
                        self.release(frame, placed, button.position);
                        continue;
                    }
                    // Klik w otwarte menu: wykonaj pozycję albo zamknij.
                    if let Some(menu) = &self.menu {
                        let rect = menu_rect(menu, size, &frame.theme);
                        if let Some(index) = menu_item_at(rect, &menu.rows, button.position) {
                            let action = menu.rows[index].action;
                            self.run_menu_action(action, placed);
                        } else {
                            self.menu = None;
                        }
                        continue;
                    }
                    let hit = view::hit_test(placed, button.position);
                    if button.button == PointerButton::Right {
                        self.selected = hit;
                        self.menu = Some(self.menu_for(hit, button.position, placed));
                        continue;
                    }
                    self.selected = hit;
                    self.press = Some(Press {
                        index: hit,
                        start: button.position,
                        dragging: false,
                        shift: button.modifiers.shift,
                        exported: false,
                    });
                    if hit.is_none() {
                        self.last_click = None;
                    }
                }
                Event::Scroll(ScrollEvent { dy, .. }) => {
                    let max = self.max_scroll(size);
                    self.scroll = (self.scroll - dy).clamp(0.0, max);
                }
                Event::DragHover { position, .. } => self.drop_hint = Some(*position),
                Event::DragLeave => self.drop_hint = None,
                Event::Drop { paths, copy, .. } => {
                    self.drop_hint = None;
                    if let Some(folder) = self.settings.folders.first().cloned() {
                        if view::drop_paths(paths, &folder, *copy) > 0 {
                            self.rescan();
                        }
                    }
                }
                _ => {}
            }
        }
        // Przewijanie mogło się zmniejszyć po usunięciu plików.
        let max = self.max_scroll(size);
        self.scroll = self.scroll.clamp(0.0, max);
    }

    fn release(&mut self, frame: &mut Frame, placed: &[view::Placed], at: Point) {
        let Some(press) = self.press.take() else {
            return;
        };
        if press.exported {
            return;
        }
        if press.dragging {
            // Przeciągnięcie w obrębie pulpitu: przenieś ikonę do komórki pod kursorem.
            if let Some(index) = press.index {
                if let Some(item) = placed.get(index) {
                    let grid = self.settings.grid.max(64.0) as f64;
                    let area = self.area(frame.size);
                    let col = ((at.x - area.x()) / grid).floor() as i32;
                    let row = ((at.y + self.scroll - area.y()) / grid).floor() as i32;
                    if col >= 0 && row >= 0 {
                        self.positions.insert(item.entry.path.clone(), (col, row));
                        self.save_positions();
                    }
                }
            }
            return;
        }
        // Klik bez ruchu: zaznacz albo otwórz przy dwukliku.
        if let Some(index) = press.index {
            let now = Instant::now();
            let double = self
                .last_click
                .map(|(last, when)| last == index && now.duration_since(when) <= DOUBLE_CLICK)
                .unwrap_or(false);
            self.selected = Some(index);
            if double {
                self.open_at(Some(index), placed);
                self.last_click = None;
            } else {
                self.last_click = Some((index, now));
            }
        }
    }

    fn paint(&self, ctx: &mut PaintCtx, placed: &[view::Placed], size: Size) {
        view::paint(
            ctx,
            placed,
            &self.settings,
            self.hovered,
            self.selected,
            self.press
                .as_ref()
                .filter(|p| p.dragging && !p.exported)
                .and_then(|p| p.index)
                .zip(self.pointer),
        );
        if let Some(menu) = &self.menu {
            paint_menu(ctx, menu, size, &self.pointer);
        }
        if let Some(point) = self.drop_hint {
            // Delikatna ramka miejsca docelowego upuszczenia.
            let side = self.settings.grid.max(64.0) as f64;
            let cell = Rect::from_xywh(point.x - side / 2.0, point.y - side / 2.0, side, side);
            ctx.scene.stroke_rounded_rect(
                cell,
                ctx.theme.radius.medium,
                2.0,
                ctx.theme.colors.accent.with_alpha(0.8),
            );
        }
    }
}

fn settings_stamp() -> Option<std::time::SystemTime> {
    libluna_core::config::config_file("desktop.conf")
        .and_then(|path| std::fs::metadata(path).ok())
        .and_then(|meta| meta.modified().ok())
}

fn distance(a: Point, b: Point) -> f64 {
    ((a.x - b.x).powi(2) + (a.y - b.y).powi(2)).sqrt()
}

fn menu_rect(menu: &Menu, size: Size, _theme: &Theme) -> Rect {
    const W: f64 = 200.0;
    const ROW_H: f64 = 32.0;
    const PAD: f64 = 6.0;
    let h = PAD * 2.0 + menu.rows.len() as f64 * ROW_H;
    let x = menu.at.x.clamp(4.0, (size.width - W - 4.0).max(4.0));
    let y = menu.at.y.clamp(4.0, (size.height - h - 4.0).max(4.0));
    Rect::from_xywh(x, y, W, h)
}

fn menu_item_at(rect: Rect, rows: &[MenuRow], p: Point) -> Option<usize> {
    if !rect.contains(p) {
        return None;
    }
    const ROW_H: f64 = 32.0;
    const PAD: f64 = 6.0;
    let local = p.y - rect.y() - PAD;
    if local < 0.0 {
        return None;
    }
    let index = (local / ROW_H).floor() as usize;
    (index < rows.len()).then_some(index)
}

fn paint_menu(ctx: &mut PaintCtx, menu: &Menu, size: Size, pointer: &Option<Point>) {
    let t = ctx.theme;
    let rect = menu_rect(menu, size, t);
    ctx.scene
        .fill_rounded_rect(rect, t.radius.medium, t.colors.card_bg);
    ctx.scene
        .stroke_rounded_rect(rect, t.radius.medium, 1.0, t.colors.border);
    const ROW_H: f64 = 32.0;
    const PAD: f64 = 6.0;
    let hovered = pointer.and_then(|p| menu_item_at(rect, &menu.rows, p));
    for (i, row) in menu.rows.iter().enumerate() {
        let r = Rect::from_xywh(
            rect.x() + PAD,
            rect.y() + PAD + i as f64 * ROW_H,
            rect.width() - 2.0 * PAD,
            ROW_H,
        );
        if hovered == Some(i) {
            ctx.scene
                .fill_rounded_rect(r, t.radius.small, libluna_widgets::hover_fill(t));
        }
        let color = if matches!(row.action, MenuAction::Trash) {
            t.colors.error
        } else {
            t.colors.text
        };
        ctx.label_left(
            r.x() + 10.0,
            r.center().y,
            row.label,
            t.typography.body.size,
            libluna_core::style::weight::NORMAL,
            color,
        );
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut app = DesktopApp::new();
    let config = AppConfig::new("Pulpit")
        .app_id("eu.paganlinux.Luna.Desktop")
        .layer(LayerConfig::background("luna-desktop"))
        .transparent(true)
        .initial_size(1920, 1080);
    run(config, move |frame| {
        if Instant::now() >= app.next_scan {
            app.rescan();
        }
        let (placed, _area) = app.layout(frame.size);
        app.handle(frame, &placed);
        // Cała powierzchnia przyjmuje kliknięcia — pulpit jest najniższą warstwą,
        // więc nie odbiera ich oknom (one mają wyższe `z`).
        frame.set_input_region(InputRegion::Rects(vec![Rect::from_xywh(
            0.0,
            0.0,
            frame.size.width.max(1.0),
            frame.size.height.max(1.0),
        )]));
        app.paint_in_frame(frame, &placed);
        if app.menu.is_some() || app.press.is_some() {
            frame.set_motion_redraw(true);
        }
        frame.request_frame_after(RESCAN_EVERY);
    })
}

impl DesktopApp {
    /// Rysuje klatkę, pożyczając scenę, motyw i silnik tekstu z `Frame`.
    fn paint_in_frame(&self, frame: &mut Frame, placed: &[view::Placed]) {
        let size = frame.size;
        let mut ctx = PaintCtx::new(&mut *frame.scene, &frame.theme, frame.text);
        self.paint(&mut ctx, placed, size);
    }
}
