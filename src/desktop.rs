//! Rysowanie pulpitu: siatka ikon, trafianie kursorem i akcje na plikach.
//!
//! Moduł jest niezależny od Waylanda — [`layout`] i [`hit_test`] można testować
//! bez okna. Aplikacja `luna-desktop` dostarcza zdarzenia wejścia i wykonuje
//! akcje systemowe ([`open`], [`trash`], [`drop_paths`]).

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Command;

use libluna_core::desktop::{
    shortcut_icon, shortcut_name, DesktopEntry, DesktopItemKind, DesktopPositions, DesktopSettings,
};
use libluna_core::{Color, Point, Rect, Theme};
use libluna_widgets::{paint_icon, IconName, PaintCtx};

/// Odstęp ikony od górnej krawędzi komórki.
const ICON_TOP: f64 = 10.0;
/// Wysokość paska etykiety pod ikoną.
const LABEL_H: f64 = 26.0;

/// Ikona umieszczona w siatce pulpitu.
#[derive(Debug, Clone)]
pub struct Placed {
    pub entry: DesktopEntry,
    /// Cała komórka siatki (tło zaznaczenia/hoveru).
    pub cell: Rect,
    /// Prostokąt samej ikony.
    pub icon: Rect,
    /// Kolumna i wiersz w siatce (do zapisu pozycji).
    pub col: i32,
    pub row: i32,
}

/// Liczba kolumn mieszczących się w obszarze.
pub fn columns(area: Rect, grid: f64) -> usize {
    ((area.width() / grid.max(1.0)).floor() as usize).max(1)
}

/// Liczba wierszy mieszczących się w obszarze.
pub fn rows(area: Rect, grid: f64) -> usize {
    ((area.height() / grid.max(1.0)).floor() as usize).max(1)
}

fn cell_rect(area: Rect, col: i32, row: i32, grid: f64) -> Rect {
    Rect::from_xywh(
        area.x() + col as f64 * grid,
        area.y() + row as f64 * grid,
        grid,
        grid,
    )
}

fn icon_rect(cell: Rect, icon_size: f64) -> Rect {
    let size = icon_size.min(cell.width() - 8.0).max(24.0);
    Rect::from_xywh(
        cell.x() + (cell.width() - size) / 2.0,
        cell.y() + ICON_TOP,
        size,
        size,
    )
}

/// Układa ikony w siatce.
///
/// Najpierw respektuje zapisane pozycje (`positions`), potem wypełnia pierwsze
/// wolne komórki w kolejności „góra → dół, potem następna kolumna”.
pub fn layout(
    entries: &[DesktopEntry],
    area: Rect,
    settings: &DesktopSettings,
    positions: &DesktopPositions,
) -> Vec<Placed> {
    let grid = settings.grid.max(64.0) as f64;
    let cols = columns(area, grid);
    let rows_n = rows(area, grid);
    let mut occupied = vec![false; cols * rows_n];
    let mut out: Vec<Placed> = Vec::with_capacity(entries.len());

    let place =
        |entry: &DesktopEntry, col: usize, row: usize, occupied: &mut Vec<bool>| -> Placed {
            occupied[row * cols + col] = true;
            let cell = cell_rect(area, col as i32, row as i32, grid);
            Placed {
                entry: entry.clone(),
                cell,
                icon: icon_rect(cell, settings.icon_size as f64),
                col: col as i32,
                row: row as i32,
            }
        };

    let mut pending: Vec<&DesktopEntry> = Vec::new();
    for entry in entries {
        if let Some((col, row)) = positions.get(&entry.path) {
            let (col, row) = (*col as usize, *row as usize);
            if col < cols && row < rows_n && !occupied[row * cols + col] {
                out.push(place(entry, col, row, &mut occupied));
                continue;
            }
        }
        pending.push(entry);
    }

    let mut slot = 0usize;
    for entry in pending {
        let mut placed_entry = false;
        while slot < occupied.len() {
            let col = slot / rows_n;
            let row = slot % rows_n;
            slot += 1;
            if !occupied[row * cols + col] {
                out.push(place(entry, col, row, &mut occupied));
                placed_entry = true;
                break;
            }
        }
        if !placed_entry {
            break;
        }
    }
    // Kolejność wpisów pozostaje stabilna (jak w katalogu); pozycje są w `cell`.
    out
}

/// Indeks ikony pod punktem.
pub fn hit_test(items: &[Placed], p: Point) -> Option<usize> {
    items.iter().position(|item| item.cell.contains(p))
}

/// Ikona wektorowa odpowiadająca wpisowi.
pub fn icon_for_entry(entry: &DesktopEntry) -> IconName {
    match entry.kind {
        DesktopItemKind::Dir => IconName::Files,
        DesktopItemKind::Shortcut => {
            let stem = entry
                .path
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default();
            let icon = shortcut_icon(&entry.path);
            crate::apps::icon_for_hint(&stem, &shortcut_name(&entry.path), icon.as_deref(), &[], "")
        }
        DesktopItemKind::File => icon_for_file(&entry.name),
    }
}

/// Ikona pliku na podstawie rozszerzenia.
fn icon_for_file(name: &str) -> IconName {
    let ext = name
        .rsplit_once('.')
        .map(|(_, ext)| ext.to_ascii_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "png" | "jpg" | "jpeg" | "webp" | "gif" | "bmp" | "svg" | "tiff" | "heic" => {
            IconName::Image
        }
        "mp3" | "flac" | "ogg" | "wav" | "m4a" | "opus" => IconName::Music,
        "mp4" | "mkv" | "webm" | "avi" | "mov" => IconName::Video,
        "zip" | "tar" | "gz" | "xz" | "bz2" | "7z" | "rar" | "deb" | "rpm" => IconName::Archive,
        "rs" | "c" | "h" | "cpp" | "py" | "js" | "ts" | "go" | "java" | "sh" | "toml" | "json"
        | "yaml" | "yml" => IconName::Code,
        "pdf" | "txt" | "md" | "odt" | "doc" | "docx" | "epub" => IconName::Document,
        "exe" | "appimage" | "bin" => IconName::Terminal,
        _ => IconName::Document,
    }
}

/// Rysuje siatkę ikon z zaznaczeniem, hoverem i etykietami.
pub fn paint(
    ctx: &mut PaintCtx,
    items: &[Placed],
    settings: &DesktopSettings,
    hovered: Option<usize>,
    selected: Option<usize>,
    dragging: Option<(usize, Point)>,
) {
    let t = ctx.theme;
    for (i, item) in items.iter().enumerate() {
        let is_selected = selected == Some(i);
        let is_hovered = hovered == Some(i);
        if is_selected || is_hovered {
            let alpha = if is_selected { 0.28 } else { 0.16 };
            let fill = t.colors.accent.with_alpha(alpha);
            let inset = Rect::from_xywh(
                item.cell.x() + 3.0,
                item.cell.y() + 3.0,
                item.cell.width() - 6.0,
                item.cell.height() - 6.0,
            );
            ctx.scene.fill_rounded_rect(inset, t.radius.medium, fill);
        }
        let offset = match dragging {
            Some((index, p)) if index == i => {
                // Podgląd przeciągania: ikona lekko za kursorem, przygaszona.
                let dx = p.x - item.icon.center().x;
                let dy = p.y - item.icon.center().y;
                (dx, dy)
            }
            _ => (0.0, 0.0),
        };
        let icon = Rect::from_xywh(
            item.icon.x() + offset.0,
            item.icon.y() + offset.1,
            item.icon.width(),
            item.icon.height(),
        );
        paint_icon(ctx, icon_for_entry(&item.entry), icon, t.colors.text);
        if settings.labels {
            let label = ellipsize(
                &item.entry.name,
                item.cell.width() - 10.0,
                t.typography.caption.size,
                ctx.text,
            );
            let band = Rect::from_xywh(
                item.cell.x() + 3.0,
                icon.bottom() + 2.0,
                item.cell.width() - 6.0,
                LABEL_H
                    .min(item.cell.bottom() - icon.bottom() - 4.0)
                    .max(0.0),
            );
            if band.height() > 0.0 {
                // Ciemna wstęga pod etykietą: tekst jest czytelny na dowolnej tapecie.
                ctx.scene
                    .fill_rounded_rect(band, t.radius.small, Color::rgba(0.0, 0.0, 0.0, 0.42));
                ctx.label_center(
                    band.center().x,
                    band.center().y,
                    &label,
                    t.typography.caption.size,
                    libluna_core::style::weight::NORMAL,
                    Color::WHITE,
                );
            }
        }
    }
}

/// Skraca napis do szerokości (`…` na końcu).
fn ellipsize(text: &str, max_w: f64, size: f64, engine: &libluna_render::TextEngine) -> String {
    if engine
        .measure(text, size, libluna_core::style::weight::NORMAL)
        .width
        <= max_w
    {
        return text.to_owned();
    }
    let mut out = String::new();
    for ch in text.chars() {
        let mut candidate = out.clone();
        candidate.push(ch);
        candidate.push('…');
        if engine
            .measure(&candidate, size, libluna_core::style::weight::NORMAL)
            .width
            > max_w
        {
            break;
        }
        out.push(ch);
    }
    out.push('…');
    out
}

/// Otwiera wpis pulpitu właściwą aplikacją.
pub fn open(entry: &DesktopEntry) {
    match entry.kind {
        DesktopItemKind::Dir => {
            if !spawn("luna-files", &[entry.path.as_os_str()]) {
                let _ = spawn("gio", &[OsStr::new("open"), entry.path.as_os_str()]);
            }
        }
        DesktopItemKind::Shortcut => {
            if !spawn("gio", &[OsStr::new("launch"), entry.path.as_os_str()]) {
                // Awaryjnie: uruchom pole `Exec` przez powłokę.
                if let Some(exec) = crate::apps::desktop_exec(&entry.path) {
                    let _ = Command::new("sh").arg("-c").arg(exec).spawn();
                }
            }
        }
        DesktopItemKind::File => {
            let _ = spawn("gio", &[OsStr::new("open"), entry.path.as_os_str()]);
        }
    }
}

/// Przenosi plik do kosza (`gio trash`).
pub fn trash(path: &Path) -> bool {
    spawn("gio", &[OsStr::new("trash"), path.as_os_str()])
}

/// Przenosi wpis do kosza lub usuwa katalog (z potwierdzeniem po stronie UI).
pub fn remove(path: &Path) -> bool {
    if path.is_dir() {
        std::fs::remove_dir_all(path).is_ok()
    } else {
        std::fs::remove_file(path).is_ok()
    }
}

/// Wstawia upuszczone pliki do katalogu pulpitu.
///
/// `copy = true` kopiuje, w przeciwnym razie przenosi. Zwraca liczbę operacji,
/// które się udały.
pub fn drop_paths(paths: &[PathBuf], target_dir: &Path, copy: bool) -> usize {
    let mut done = 0;
    let _ = std::fs::create_dir_all(target_dir);
    for source in paths {
        let Some(name) = source.file_name() else {
            continue;
        };
        let mut target = target_dir.join(name);
        let mut n = 2;
        while target.exists() {
            let stem = name.to_string_lossy();
            let (base, ext) = match stem.rsplit_once('.') {
                Some((base, ext)) if !base.is_empty() => (base.to_owned(), format!(".{ext}")),
                _ => (stem.to_string(), String::new()),
            };
            target = target_dir.join(format!("{base}-{n}{ext}"));
            n += 1;
        }
        let ok = if copy {
            let status = Command::new("cp")
                .arg("-a")
                .arg("--")
                .arg(source)
                .arg(&target)
                .status();
            status.map(|s| s.success()).unwrap_or(false)
        } else {
            std::fs::rename(source, &target).is_ok() || {
                let status = Command::new("cp")
                    .arg("-a")
                    .arg("--")
                    .arg(source)
                    .arg(&target)
                    .status();
                if status.map(|s| s.success()).unwrap_or(false) {
                    let _ = remove(source);
                    true
                } else {
                    false
                }
            }
        };
        if ok {
            done += 1;
        }
    }
    done
}

fn spawn(program: &str, args: &[&std::ffi::OsStr]) -> bool {
    Command::new(program)
        .args(args)
        .spawn()
        .map(|_| true)
        .unwrap_or(false)
}

/// Nowy folder o unikalnej nazwie („Nowy folder”, „Nowy folder 2”, …).
pub fn new_folder(dir: &Path) -> Option<PathBuf> {
    let _ = std::fs::create_dir_all(dir);
    for n in 1..100 {
        let name = if n == 1 {
            "Nowy folder".to_owned()
        } else {
            format!("Nowy folder {n}")
        };
        let path = dir.join(name);
        if !path.exists() {
            return std::fs::create_dir(&path).ok().map(|()| path);
        }
    }
    None
}

/// Kolor tła pulpitu dobierany do motywu (używany tylko w podglądach/testach).
pub fn backdrop(theme: &Theme) -> Color {
    theme.colors.window_bg.scale_alpha(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn entry(name: &str, kind: DesktopItemKind) -> DesktopEntry {
        DesktopEntry {
            path: PathBuf::from(format!("/tmp/{name}")),
            name: name.to_owned(),
            kind,
        }
    }

    fn settings() -> DesktopSettings {
        DesktopSettings {
            folders: vec![],
            icon_size: 64.0,
            grid: 100.0,
            labels: true,
        }
    }

    #[test]
    fn layout_fills_columns_top_to_bottom() {
        let entries: Vec<_> = (0..5)
            .map(|i| entry(&format!("f{i}.txt"), DesktopItemKind::File))
            .collect();
        let area = Rect::from_xywh(0.0, 0.0, 300.0, 300.0);
        let placed = layout(&entries, area, &settings(), &HashMap::new());
        assert_eq!(placed.len(), 5);
        assert_eq!(placed[0].col, 0);
        assert_eq!(placed[0].row, 0);
        assert_eq!(placed[1].row, 1);
        assert_eq!(placed[2].row, 2);
        assert_eq!(placed[3].col, 1);
        assert_eq!(placed[3].row, 0);
    }

    #[test]
    fn saved_positions_are_respected() {
        let entries = vec![
            entry("a.txt", DesktopItemKind::File),
            entry("b.txt", DesktopItemKind::File),
        ];
        let area = Rect::from_xywh(0.0, 0.0, 300.0, 300.0);
        let mut positions = HashMap::new();
        positions.insert(PathBuf::from("/tmp/b.txt"), (1, 1));
        let placed = layout(&entries, area, &settings(), &positions);
        let b = placed.iter().find(|p| p.entry.name == "b.txt").unwrap();
        assert_eq!((b.col, b.row), (1, 1));
        let a = placed.iter().find(|p| p.entry.name == "a.txt").unwrap();
        assert_eq!((a.col, a.row), (0, 0));
    }

    #[test]
    fn hit_test_matches_cell() {
        let entries = vec![entry("a.txt", DesktopItemKind::File)];
        let area = Rect::from_xywh(0.0, 0.0, 300.0, 300.0);
        let placed = layout(&entries, area, &settings(), &HashMap::new());
        assert_eq!(hit_test(&placed, Point::new(10.0, 10.0)), Some(0));
        assert_eq!(hit_test(&placed, Point::new(250.0, 250.0)), None);
    }

    #[test]
    fn file_icons_follow_extension() {
        assert_eq!(icon_for_file("zdjecie.PNG"), IconName::Image);
        assert_eq!(icon_for_file("piosenka.flac"), IconName::Music);
        assert_eq!(icon_for_file("kod.rs"), IconName::Code);
        assert_eq!(icon_for_file("bez-rozszerzenia"), IconName::Document);
    }

    #[test]
    fn drop_creates_unique_name() {
        let dir = std::env::temp_dir().join(format!("luna-drop-{}", std::process::id()));
        let source_dir = dir.join("src");
        let target = dir.join("pulpit");
        std::fs::create_dir_all(&source_dir).unwrap();
        std::fs::write(source_dir.join("a.txt"), "1").unwrap();
        assert_eq!(drop_paths(&[source_dir.join("a.txt")], &target, true), 1);
        std::fs::write(source_dir.join("a.txt"), "2").unwrap();
        assert_eq!(drop_paths(&[source_dir.join("a.txt")], &target, true), 1);
        assert!(target.join("a.txt").exists());
        assert!(target.join("a-2.txt").exists());
        let _ = std::fs::remove_dir_all(dir);
    }
}
