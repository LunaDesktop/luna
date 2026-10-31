//! Wykrywanie zainstalowanych aplikacji na podstawie plików `.desktop` (XDG).
//!
//! Skanujemy katalogi `applications/` z `XDG_DATA_HOME` i `XDG_DATA_DIRS`, parsujemy
//! sekcję `[Desktop Entry]` i zwracamy listę aplikacji do pokazania w launcherze.
//! Kolejność katalogów = priorytet (użytkownik nad systemem); duplikaty po `id` wygrywa
//! pierwszy napotkany.
//!
//! Parsowanie jest czyste ([`parse`]) — łatwe do testów bez dostępu do systemu plików.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{OnceLock, RwLock};

use libluna_widgets::IconName;

/// Aplikacja z pliku `.desktop`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopApp {
    pub name: String,
    pub exec: String,
    pub icon: Option<String>,
    pub comment: Option<String>,
    pub terminal: bool,
    pub categories: Vec<String>,
    /// `TryExec` — polecenie, którego obecność w `PATH` decyduje o pokazaniu wpisu.
    /// `None` = wpis nie deklaruje wymagania.
    pub try_exec: Option<String>,
    /// `OnlyShowIn` — lista pulpitów, w których wpis ma się pokazywać (pusta = wszystkie).
    pub only_show_in: Vec<String>,
    /// `NotShowIn` — lista pulpitów, w których wpis ma być ukryty.
    pub not_show_in: Vec<String>,
    /// `DBusActivatable=true` — aplikacja startuje przez D-Bus (może nie mieć binarki).
    pub dbus_activatable: bool,
    /// Ścieżka pliku `.desktop` — pozwala uruchomić aplikację przez `gio launch`,
    /// które obsługuje `Path=`, kody pól (`%u`) i aktywację D-Bus.
    pub desktop_path: Option<PathBuf>,
}

impl DesktopApp {
    /// Czy wpis warto pokazywać. Gdy deklaruje `TryExec`, program musi istnieć w `PATH`;
    /// w przeciwnym razie sprawdzamy pierwszy człon `Exec` (aplikacje `DBusActivatable`
    /// pomijamy — mogą nie mieć binarki i wystartować przez magistralę).
    pub fn available(&self) -> bool {
        if let Some(cmd) = &self.try_exec {
            return command_in_path(cmd);
        }
        if self.dbus_activatable {
            return true;
        }
        exec_is_runnable(&self.exec)
    }

    /// Czy wpis ma się pokazywać w pulpicie `desktop`.
    ///
    /// `OnlyShowIn` traktujemy jako **podpowiedź**, nie filtr: użytkownik chce mieć
    /// w launcherze wszystkie aplikacje (m.in. narzędzia XFCE, z których korzysta
    /// w Lunie). Respektujemy tylko `NotShowIn` — to jawne „nie uruchamiaj tutaj”.
    pub fn visible_in(&self, desktop: &str) -> bool {
        let has = |list: &[String]| list.iter().any(|s| s.eq_ignore_ascii_case(desktop));
        !has(&self.not_show_in)
    }
}

/// „Pośredniki”, których nie oceniamy po nazwie — uruchamiają dalej, co im podano.
const EXEC_LAUNCHERS: &[&str] = &[
    "env",
    "sh",
    "bash",
    "dash",
    "flatpak",
    "gio",
    "gtk-launch",
    "exo-open",
    "xdg-open",
];

/// Czy pierwszy człon polecenia `Exec` da się uruchomić.
fn exec_is_runnable(exec: &str) -> bool {
    let Some(first) = exec.split_whitespace().next() else {
        return false;
    };
    let first = first.trim_matches('"').trim_matches('\'');
    // Przypisania środowiska (`FOO=bar app`) i znane pośredniki — nie oceniamy.
    if first.is_empty() || first.contains('=') || EXEC_LAUNCHERS.contains(&first) {
        return true;
    }
    command_in_path(first)
}

/// Dzieli wartość listy `.desktop` (`A;B;C;`) na elementy.
fn split_list(value: &str) -> Vec<String> {
    value
        .split(';')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect()
}

/// Czy `cmd` (bez argumentów) da się znaleźć i uruchomić.
fn command_in_path(cmd: &str) -> bool {
    let cmd = cmd.trim();
    if cmd.is_empty() {
        return false;
    }
    // Ścieżka jawna: wystarczy sprawdzić plik.
    if cmd.contains('/') {
        return Path::new(cmd).is_file();
    }
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path).any(|dir| {
        let p = dir.join(cmd);
        p.is_file()
    })
}

/// Język z otoczenia (`LANG`/`LC_MESSAGES`), np. `pl` z `pl_PL.UTF-8`.
pub fn current_lang() -> Option<String> {
    let raw = std::env::var("LC_MESSAGES")
        .or_else(|_| std::env::var("LANG"))
        .ok()?;
    let lang = raw.split(['_', '.', '@']).next()?.trim();
    if lang.is_empty() || lang == "C" || lang == "POSIX" {
        None
    } else {
        Some(lang.to_ascii_lowercase())
    }
}

/// Usuwa kody pól (`%U`, `%f`, `%i`…) z polecenia `Exec`.
fn strip_field_codes(exec: &str) -> String {
    exec.split_whitespace()
        .filter(|tok| !(tok.starts_with('%') && tok.len() == 2))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Parsuje treść pliku `.desktop`. Zwraca `None`, gdy to nie jest wyświetlana aplikacja.
pub fn parse(content: &str, lang: Option<&str>) -> Option<DesktopApp> {
    let mut in_entry = false;
    let mut name = None;
    let mut name_loc = None;
    let mut comment = None;
    let mut exec = None;
    let mut icon = None;
    let mut terminal = false;
    let mut ty = None;
    let mut nodisplay = false;
    let mut hidden = false;
    let mut try_exec: Option<String> = None;
    let mut only_show_in: Vec<String> = Vec::new();
    let mut not_show_in: Vec<String> = Vec::new();
    let mut dbus_activatable = false;
    let mut categories = Vec::new();

    let name_key = lang.map(|l| format!("Name[{l}]"));
    let comment_key = lang.map(|l| format!("Comment[{l}]"));

    for raw in content.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.starts_with('[') {
            in_entry = line == "[Desktop Entry]";
            continue;
        }
        if !in_entry {
            continue;
        }
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        let (k, v) = (k.trim(), v.trim());
        match k {
            "Type" => ty = Some(v.to_owned()),
            "Name" => name = Some(v.to_owned()),
            "Comment" => comment = Some(v.to_owned()),
            "Exec" => exec = Some(v.to_owned()),
            "Icon" => icon = Some(v.to_owned()),
            "Terminal" => terminal = v.eq_ignore_ascii_case("true"),
            "TryExec" => try_exec = Some(v.to_owned()),
            "OnlyShowIn" => only_show_in = split_list(v),
            "NotShowIn" => not_show_in = split_list(v),
            "DBusActivatable" => dbus_activatable = v.eq_ignore_ascii_case("true"),
            "NoDisplay" => nodisplay = v.eq_ignore_ascii_case("true"),
            "Hidden" => hidden = v.eq_ignore_ascii_case("true"),
            "Categories" => {
                categories = v
                    .split(';')
                    .filter(|s| !s.is_empty())
                    .map(|s| s.to_owned())
                    .collect();
            }
            _ => {
                if name_key.as_deref() == Some(k) {
                    name_loc = Some(v.to_owned());
                } else if comment_key.as_deref() == Some(k) {
                    comment = Some(v.to_owned());
                }
            }
        }
    }

    if ty.as_deref() != Some("Application") || nodisplay || hidden {
        return None;
    }
    Some(DesktopApp {
        name: name_loc.or(name)?,
        exec: strip_field_codes(&exec?),
        icon,
        comment,
        terminal,
        categories,
        try_exec,
        only_show_in,
        not_show_in,
        dbus_activatable,
        desktop_path: None,
    })
}

/// Skanuje system w poszukiwaniu aplikacji (posortowane po nazwie).
pub fn scan() -> Vec<DesktopApp> {
    let lang = current_lang();
    let mut seen: HashSet<String> = HashSet::new();
    let mut seen_apps: HashSet<(String, String)> = HashSet::new();
    let mut apps = Vec::new();

    for dir in application_dirs() {
        for path in desktop_files(&dir, 0) {
            let id = path
                .strip_prefix(&dir)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('/', "-");
            if !seen.insert(id) {
                continue;
            }
            if let Ok(content) = std::fs::read_to_string(&path) {
                if let Some(mut app) = parse(&content, lang.as_deref()) {
                    app.desktop_path = Some(path.clone());
                    // Zgodnie ze specyfikacją: nie pokazujemy wpisów, których nie ma jak
                    // uruchomić (`TryExec`/`Exec`). `OnlyShowIn` **nie** ukrywa wpisów —
                    // launcher Luny ma pokazywać wszystkie zainstalowane aplikacje.
                    if !app.available() || !app.visible_in("Luna") {
                        continue;
                    }
                    // Ten sam program bywa opisany kilkoma plikami (np. `google-chrome`
                    // i `com.google.Chrome`) — pokazujemy go raz.
                    if !seen_apps.insert((app.name.to_lowercase(), app.exec.clone())) {
                        continue;
                    }
                    apps.push(app);
                }
            }
        }
    }

    apps.sort_by_key(|a| a.name.to_lowercase());
    apps
}

/// Znacznik stanu katalogów `applications/` (mtime i rozmiar plików `.desktop`).
///
/// Powłoka porównuje go co klatkę i wznawia skanowanie, gdy zmieni się zestaw
/// zainstalowanych aplikacji — np. zaraz po instalacji pakietu w Sklepie.
/// Liczymy tylko metadane, a nie pełne parsowanie plików.
pub fn stamp() -> u64 {
    stamp_of(&application_dirs())
}

/// Wariant [`stamp`] dla jawnie podanych katalogów (testowalny bez zmiany `XDG_*`).
fn stamp_of(dirs: &[PathBuf]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    let mut mix = |value: u64| {
        hash ^= value;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    };
    for dir in dirs {
        if let Ok(meta) = std::fs::metadata(dir) {
            mix(modified_nanos(&meta));
        }
        let mut files = desktop_files(dir, 0);
        files.sort();
        for path in files {
            let Ok(meta) = std::fs::metadata(&path) else {
                continue;
            };
            mix(modified_nanos(&meta));
            mix(meta.len());
            if let Some(name) = path.file_name() {
                for byte in name.to_string_lossy().bytes() {
                    mix(u64::from(byte));
                }
            }
        }
    }
    hash
}

fn modified_nanos(meta: &std::fs::Metadata) -> u64 {
    meta.modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
}

/// Katalogi `applications/` w kolejności priorytetu (użytkownik pierwszy).
fn application_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Ok(home) = std::env::var("XDG_DATA_HOME") {
        dirs.push(PathBuf::from(home).join("applications"));
    } else if let Ok(home) = std::env::var("HOME") {
        dirs.push(PathBuf::from(home).join(".local/share/applications"));
    }
    let data_dirs =
        std::env::var("XDG_DATA_DIRS").unwrap_or_else(|_| "/usr/local/share:/usr/share".to_owned());
    for d in data_dirs.split(':').filter(|d| !d.is_empty()) {
        dirs.push(PathBuf::from(d).join("applications"));
    }
    dirs
}

/// Zbiera pliki `.desktop` z katalogu (rekurencyjnie do głębokości 2).
fn desktop_files(dir: &Path, depth: usize) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if depth < 2 {
                out.extend(desktop_files(&path, depth + 1));
            }
        } else if path.extension().map(|e| e == "desktop").unwrap_or(false) {
            out.push(path);
        }
    }
    out
}

static ICON_NAMES: OnceLock<RwLock<Option<HashMap<String, String>>>> = OnceLock::new();

fn icon_names_lock() -> &'static RwLock<Option<HashMap<String, String>>> {
    ICON_NAMES.get_or_init(|| RwLock::new(None))
}

fn icon_names_from(apps: &[DesktopApp]) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for app in apps {
        let Some(icon) = app.icon.as_deref().filter(|icon| !icon.trim().is_empty()) else {
            continue;
        };
        if let Some(stem) = app
            .desktop_path
            .as_ref()
            .and_then(|path| path.file_stem())
            .and_then(|stem| stem.to_str())
        {
            map.entry(stem.to_lowercase())
                .or_insert_with(|| icon.to_owned());
        }
        map.entry(icon.to_lowercase())
            .or_insert_with(|| icon.to_owned());
        map.entry(app.name.to_lowercase())
            .or_insert_with(|| icon.to_owned());
    }
    map
}

/// Czyści mapę `app_id` → nazwa ikony (po ponownym skanowaniu `.desktop`).
pub fn refresh_icon_names() {
    if let Ok(mut cache) = icon_names_lock().write() {
        *cache = None;
    }
}

/// Oryginalna nazwa ikony (`Icon=`) dla `app_id` okna.
///
/// Taskbar dostaje z kompozytora tylko `app_id`, a nie cały wpis `.desktop`.
/// Dzięki tej mapie okno `gparted` albo `net.lutris.Lutris` może pokazać ikonę
/// dostarczoną przez samą aplikację, zamiast ogólnej ikony wektorowej.
pub fn icon_name_for_app_id(app_id: &str) -> Option<String> {
    let key = app_id.trim().to_lowercase();
    if key.is_empty() {
        return None;
    }
    let lock = icon_names_lock();
    if let Ok(cache) = lock.read() {
        if let Some(map) = cache.as_ref() {
            return map.get(&key).cloned();
        }
    }
    let map = icon_names_from(&scan());
    let hit = map.get(&key).cloned();
    if let Ok(mut cache) = lock.write() {
        *cache = Some(map);
    }
    hit
}

/// Dobiera ikonę z pakietu Luny na podstawie kategorii/nazwy (heurystyka).
pub fn icon_for(app: &DesktopApp) -> IconName {
    let id = app
        .desktop_path
        .as_ref()
        .and_then(|p| p.file_stem())
        .and_then(|s| s.to_str())
        .unwrap_or_default();
    icon_for_hint(
        id,
        &app.name,
        app.icon.as_deref(),
        &app.categories,
        &app.exec,
    )
}

/// Wspólna heurystyka ikon dla launchera **i taskbara**.
///
/// Bierzemy wszystko, co może opisać aplikację: `app_id`, tytuł okna, nazwę ikony
/// z `.desktop`, kategorie i polecenie. Kolejność reguł jest od najbardziej
/// szczegółowych do ogólnych — dzięki temu „Network Settings” trafia w ustawienia,
/// a nie w ikonę Wi-Fi.
pub fn icon_for_hint(
    app_id: &str,
    title: &str,
    icon: Option<&str>,
    categories: &[String],
    exec: &str,
) -> IconName {
    let hay = format!(
        "{} {} {} {} {}",
        app_id,
        title,
        icon.unwrap_or_default(),
        categories.join(" "),
        exec
    )
    .to_lowercase();
    const RULES: &[(&[&str], IconName)] = &[
        (
            &[
                "terminal",
                "console",
                "alacritty",
                "kitty",
                "foot",
                "konsole",
                "xterm",
                "karton",
            ],
            IconName::Terminal,
        ),
        (
            &[
                "filemanager",
                "nautilus",
                "thunar",
                "nemo",
                "dolphin",
                "pcmanfm",
                "caja",
                "files",
                "pliki",
                "katalog",
            ],
            IconName::Files,
        ),
        (
            &[
                "browser",
                "firefox",
                "chromium",
                "chrome",
                "brave",
                "epiphany",
                "falkon",
                "opera",
                "vivaldi",
                "zen-browser",
            ],
            IconName::Globe,
        ),
        (
            &["mail", "thunderbird", "evolution", "geary", "outlook"],
            IconName::Mail,
        ),
        (
            &[
                "chat",
                "discord",
                "slack",
                "telegram",
                "signal",
                "whatsapp",
                "element",
                "matrix",
                "messenger",
            ],
            IconName::Chat,
        ),
        (
            &["zed", "vscode", "code-insiders", "code-oss", "sublime"],
            IconName::Code,
        ),
        (
            &[
                "mousepad",
                "gedit",
                "kate",
                "texteditor",
                "editor",
                "edytor",
                "notepad",
            ],
            IconName::Edit,
        ),
        (
            &[
                "image",
                "photo",
                "gimp",
                "krita",
                "inkscape",
                "ristretto",
                "eog",
                "shotwell",
                "darktable",
            ],
            IconName::Image,
        ),
        (
            &[
                "video",
                "media",
                "multimedia",
                "vlc",
                "mpv",
                "totem",
                "celluloid",
                "mplayer",
                "youtube",
            ],
            IconName::Video,
        ),
        (
            &[
                "music",
                "audio",
                "spotify",
                "rhythmbox",
                "elisa",
                "clementine",
                "audacious",
                "odtwarzacz",
            ],
            IconName::Music,
        ),
        (
            &[
                "document",
                "office",
                "writer",
                "libreoffice",
                "onlyoffice",
                "evince",
                "reader",
                "pdf",
            ],
            IconName::Document,
        ),
        (
            &[
                "calculator",
                "galculator",
                "qalculate",
                "kcalc",
                "kalkulator",
            ],
            IconName::Calculator,
        ),
        (
            &["game", "steam", "lutris", "heroic", "gry"],
            IconName::Game,
        ),
        (
            &[
                "store",
                "sklep",
                "pamac",
                "software",
                "discover",
                "synaptic",
                "shop",
                "karton-shop",
            ],
            IconName::Store,
        ),
        (
            &[
                "screenshot",
                "screenshooter",
                "flameshot",
                "spectacle",
                "zrzut",
            ],
            IconName::Screenshot,
        ),
        (&["printer", "print", "cups"], IconName::Printer),
        (&["bluetooth", "blueman"], IconName::Bluetooth),
        (
            &[
                "monitor",
                "htop",
                "btop",
                "missioncenter",
                "hardinfo",
                "gparted",
                "lmission",
                "taskmanager",
                "task manager",
            ],
            IconName::Monitor,
        ),
        (
            &["archive", "file-roller", "xarchiver", "7z", "zip", "ark"],
            IconName::Archive,
        ),
        (
            &["clock", "zegar", "calendar", "kalendarz"],
            IconName::Clock,
        ),
        (
            &[
                "settings",
                "preferences",
                "control",
                "ustawienia",
                "pavucontrol",
                "nm-connection",
                "luna.settings",
                "xfce4-settings",
            ],
            IconName::Settings,
        ),
        (&["wifi", "wi-fi", "network", "nm-"], IconName::Wifi),
        (
            &["volume", "pipewire", "dźwięk", "dzwiek"],
            IconName::Volume,
        ),
        (&["home", "katalog domowy"], IconName::Home),
        (
            &["help", "info", "about", "pomoc", "welcome"],
            IconName::Info,
        ),
        (&["user", "account", "konta"], IconName::User),
    ];
    for (needles, icon) in RULES {
        if needles.iter().any(|n| hay.contains(n)) {
            return *icon;
        }
    }
    IconName::App
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIREFOX: &str = "\
[Desktop Entry]
Type=Application
Name=Firefox
Name[pl]=Firefox
GenericName=Web Browser
Comment=Browse the Web
Exec=firefox %u
Icon=firefox
Terminal=false
Categories=Network;WebBrowser;
";

    #[test]
    fn maps_app_id_to_desktop_icon_name() {
        let mut app = parse(FIREFOX, None).unwrap();
        app.desktop_path = Some(PathBuf::from("/usr/share/applications/firefox.desktop"));
        let map = icon_names_from(&[app]);
        assert_eq!(map.get("firefox").map(String::as_str), Some("firefox"));
    }

    #[test]
    fn parses_application_entry() {
        let app = parse(FIREFOX, Some("pl")).unwrap();
        assert_eq!(app.name, "Firefox");
        assert_eq!(app.exec, "firefox"); // %u usunięte
        assert!(!app.terminal);
        assert!(app.categories.contains(&"WebBrowser".to_owned()));
        assert_eq!(icon_for(&app), IconName::Globe);
    }

    #[test]
    fn icon_hint_uses_app_id_and_icon_name() {
        // Taskbar nie ma kategorii `.desktop`, ale ma `app_id` i tytuł.
        assert_eq!(
            icon_for_hint("org.mozilla.firefox", "Strona startowa", None, &[], ""),
            IconName::Globe
        );
        assert_eq!(
            icon_for_hint("dev.zed.Zed", "projekt — Zed", None, &[], ""),
            IconName::Code
        );
        assert_eq!(
            icon_for_hint("org.telegram.desktop", "Telegram", None, &[], ""),
            IconName::Chat
        );
        assert_eq!(
            icon_for_hint("com.spotify.Client", "Spotify", None, &[], ""),
            IconName::Music
        );
    }

    #[test]
    fn prefers_localized_name() {
        let content =
            "[Desktop Entry]\nType=Application\nName=Files\nName[pl]=Pliki\nExec=nautilus\n";
        assert_eq!(parse(content, Some("pl")).unwrap().name, "Pliki");
        // Bez dopasowania języka → nazwa bazowa.
        assert_eq!(parse(content, Some("de")).unwrap().name, "Files");
    }

    #[test]
    fn skips_non_application_and_hidden() {
        assert!(parse("[Desktop Entry]\nType=Link\nName=x\nExec=x\n", None).is_none());
        assert!(parse(
            "[Desktop Entry]\nType=Application\nName=x\nExec=x\nNoDisplay=true\n",
            None
        )
        .is_none());
        assert!(parse(
            "[Desktop Entry]\nType=Application\nName=x\nExec=x\nHidden=true\n",
            None
        )
        .is_none());
        assert!(parse("[Desktop Entry]\nType=Application\nName=x\n", None).is_none());
    }

    #[test]
    fn ignores_unrelated_sections() {
        let content = "[Desktop Action New]\nName=Nowy\nExec=touch\n\n[Desktop Entry]\nType=Application\nName=Pliki\nExec=nautilus\n";
        assert_eq!(parse(content, None).unwrap().name, "Pliki");
    }

    #[test]
    fn reads_try_exec_and_availability() {
        // Program, którego na pewno nie ma — wpis jest niedostępny.
        let content = "[Desktop Entry]\nType=Application\nName=X\nExec=x\nTryExec=__luna_brak_tego_programu__\n";
        let app = parse(content, None).unwrap();
        assert_eq!(app.try_exec.as_deref(), Some("__luna_brak_tego_programu__"));
        assert!(!app.available());

        // `TryExec=sh` istnieje w PATH.
        let sh = parse(
            "[Desktop Entry]\nType=Application\nName=Z\nExec=z\nTryExec=sh\n",
            None,
        )
        .unwrap();
        assert!(sh.available());
    }

    #[test]
    fn hides_entries_with_missing_exec_binary() {
        // `Exec` na binarkę, której nie ma na tej maszynie (nie hardkodujemy
        // `/usr/bin/google-chrome-stable` — po instalacji Chrome test padał).
        let chrome = parse(
            "[Desktop Entry]\nType=Application\nName=Chrome\nExec=/usr/bin/__luna_brak_tego_programu__ %U\n",
            None,
        )
        .unwrap();
        assert!(!chrome.available());

        // Zwykłe polecenie z PATH — pokazujemy.
        let plain = parse(
            "[Desktop Entry]\nType=Application\nName=A\nExec=sh -c x\n",
            None,
        )
        .unwrap();
        assert!(plain.available());

        // Pośredniki i przypisania środowiska — nie oceniamy, pokazujemy.
        for exec in [
            "flatpak run org.example.App",
            "env GDK_BACKEND=x11 foo",
            "exo-open --launch MailReader",
        ] {
            let content = format!("[Desktop Entry]\nType=Application\nName=B\nExec={exec}\n");
            let app = parse(&content, None).unwrap();
            assert!(app.available(), "powinno być dostępne: {exec}");
        }
    }

    #[test]
    fn only_show_in_is_a_hint_not_a_filter() {
        // Narzędzia XFCE mają `OnlyShowIn=XFCE;` — w Lunie nadal mają być widoczne.
        let content = "[Desktop Entry]\nType=Application\nName=Edytor\nExec=xfce4-settings-editor\nOnlyShowIn=XFCE;\n";
        let app = parse(content, None).unwrap();
        assert!(app.visible_in("Luna"));
        // `NotShowIn` nadal ukrywa.
        let hidden = "[Desktop Entry]\nType=Application\nName=X\nExec=x\nNotShowIn=Luna;\n";
        assert!(!parse(hidden, None).unwrap().visible_in("Luna"));
    }

    #[test]
    fn icon_heuristics_cover_common_categories() {
        let mk = |cat: &str, name: &str| DesktopApp {
            name: name.into(),
            exec: "x".into(),
            icon: None,
            comment: None,
            terminal: false,
            categories: cat
                .split(';')
                .filter(|s| !s.is_empty())
                .map(String::from)
                .collect(),
            try_exec: None,
            only_show_in: Vec::new(),
            not_show_in: Vec::new(),
            dbus_activatable: false,
            desktop_path: None,
        };
        assert_eq!(
            icon_for(&mk("System;FileManager;", "Pliki")),
            IconName::Files
        );
        assert_eq!(
            icon_for(&mk("System;TerminalEmulator;", "Terminal")),
            IconName::Terminal
        );
        assert_eq!(
            icon_for(&mk("AudioVideo;Player;", "Odtwarzacz")),
            IconName::Video
        );
        assert_eq!(
            icon_for(&mk("Audio;Music;Player;", "Odtwarzacz muzyki")),
            IconName::Music
        );
        assert_eq!(
            icon_for(&mk("Office;Writer;", "Dokumenty")),
            IconName::Document
        );
        assert_eq!(
            icon_for(&mk("Graphics;Photography;", "Zdjęcia")),
            IconName::Image
        );
        assert_eq!(icon_for(&mk("Game;", "Gra")), IconName::Game);
        assert_eq!(
            icon_for(&mk("System;Settings;", "Ustawienia")),
            IconName::Settings
        );
        assert_eq!(
            icon_for(&mk("Utility;Calculator;", "Kalkulator")),
            IconName::Calculator
        );
        assert_eq!(icon_for(&mk("Utility;", "Costam")), IconName::App);
    }

    #[test]
    fn stamp_changes_when_an_application_is_installed() {
        let base = std::env::temp_dir().join(format!(
            "luna-apps-stamp-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();

        let before = stamp_of(std::slice::from_ref(&base));
        let entry = "[Desktop Entry]\nType=Application\nName=Firefox\nExec=firefox\n";
        std::fs::write(base.join("firefox.desktop"), entry).unwrap();
        let after = stamp_of(std::slice::from_ref(&base));
        assert_ne!(before, after, "nowy plik .desktop musi zmienić znacznik");
        assert_eq!(
            after,
            stamp_of(std::slice::from_ref(&base)),
            "znacznik musi być stabilny bez zmian"
        );

        let _ = std::fs::remove_dir_all(&base);
    }
}
