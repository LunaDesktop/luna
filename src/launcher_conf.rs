//! Trwały stan launchera: przypięte i ostatnio używane aplikacje.
//!
//! Plik `$XDG_CONFIG_HOME/luna/launcher.conf` jest prostą listą par
//! `klucz = wartość`:
//!
//! ```text
//! # Luna launcher
//! pinned=org.gnome.Nautilus
//! pinned=eu.paganlinux.Terminal
//! recent=firefox
//! ```
//!
//! Klucze to nazwy plików `.desktop` (patrz [`App::key`](crate::panel::App::key)),
//! więc nie zależą od języka interfejsu. Parsowanie jest czyste i testowalne;
//! zapis jest atomowy (`libluna_core::config::save_atomic`).

use libluna_core::config::{config_file, parse_pairs, save_atomic};

use crate::panel::{App, Launcher};

/// Nazwa pliku w katalogu konfiguracji Luny.
pub const FILE: &str = "launcher.conf";
/// Ile przypiętych pozycji zapisujemy (ochrona przed ręcznie edytowanym plikiem).
pub const PINNED_LIMIT: usize = 32;

/// Wczytany stan launchera (jeszcze bez powiązania z listą aplikacji).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Prefs {
    pub pinned: Vec<String>,
    pub recent: Vec<String>,
}

impl Prefs {
    /// Klucze przypięte, które istnieją na liście aplikacji, w kolejności zapisu.
    pub fn pinned_apps<'a>(&self, apps: &'a [App]) -> Vec<&'a App> {
        self.pinned
            .iter()
            .filter_map(|key| apps.iter().find(|a| &a.key() == key))
            .collect()
    }
}

/// Parsuje treść pliku. Puste i powtórzone wpisy są pomijane, nadmiar obcinany.
pub fn parse(text: &str) -> Prefs {
    let mut prefs = Prefs::default();
    for (key, value) in parse_pairs(text) {
        let list = match key.as_str() {
            "pinned" => &mut prefs.pinned,
            "recent" => &mut prefs.recent,
            _ => continue,
        };
        if value.is_empty() || list.contains(&value) {
            continue;
        }
        list.push(value);
    }
    prefs.pinned.truncate(PINNED_LIMIT);
    prefs.recent.truncate(Launcher::RECENT_LIMIT);
    prefs
}

/// Serializuje stan do formatu pliku (stabilna kolejność, komentarz nagłówka).
pub fn to_text(prefs: &Prefs) -> String {
    let mut out = String::from("# Luna launcher — przypięte i ostatnio używane aplikacje.\n");
    for key in &prefs.pinned {
        out.push_str("pinned=");
        out.push_str(key);
        out.push('\n');
    }
    for key in &prefs.recent {
        out.push_str("recent=");
        out.push_str(key);
        out.push('\n');
    }
    out
}

/// Wczytuje stan do launchera. Brak pliku / błąd odczytu zostawia stan bez zmian.
pub fn load_into(launcher: &mut Launcher) {
    let Some(path) = config_file(FILE) else {
        return;
    };
    let Ok(text) = std::fs::read_to_string(path) else {
        return;
    };
    let prefs = parse(&text);
    launcher.pinned = prefs.pinned;
    launcher.recent = prefs.recent;
}

/// Zapisuje bieżący stan launchera (błędy zapisu są ignorowane — to tylko wygoda).
pub fn save_from(launcher: &Launcher) {
    let Some(path) = config_file(FILE) else {
        return;
    };
    let prefs = Prefs {
        pinned: launcher.pinned.clone(),
        recent: launcher.recent.clone(),
    };
    let _ = save_atomic(&path, &to_text(&prefs));
}

#[cfg(test)]
mod tests {
    use super::*;
    use libluna_widgets::IconName;

    fn sample_app(key: &str) -> App {
        App {
            icon: IconName::App,
            theme_icon: None,
            name: key.to_owned(),
            category: "System".to_owned(),
            exec: None,
            terminal: false,
            desktop_path: None,
        }
    }

    #[test]
    fn parses_pinned_and_recent_in_order() {
        let text = "# komentarz\npinned=firefox\npinned=org.gnome.Nautilus\nrecent=code\nfoo=bar\n";
        let prefs = parse(text);
        assert_eq!(prefs.pinned, vec!["firefox", "org.gnome.Nautilus"]);
        assert_eq!(prefs.recent, vec!["code"]);
    }

    #[test]
    fn ignores_empty_and_duplicate_entries() {
        let prefs = parse("pinned=\npinned=firefox\npinned=firefox\nrecent=  \nrecent=code\n");
        assert_eq!(prefs.pinned, vec!["firefox"]);
        assert_eq!(prefs.recent, vec!["code"]);
    }

    #[test]
    fn truncates_oversized_file() {
        let mut text = String::new();
        for i in 0..(PINNED_LIMIT + 5) {
            text.push_str(&format!("pinned=app{i}\n"));
        }
        for i in 0..(Launcher::RECENT_LIMIT + 5) {
            text.push_str(&format!("recent=app{i}\n"));
        }
        let prefs = parse(&text);
        assert_eq!(prefs.pinned.len(), PINNED_LIMIT);
        assert_eq!(prefs.recent.len(), Launcher::RECENT_LIMIT);
    }

    #[test]
    fn roundtrips_through_text() {
        let prefs = Prefs {
            pinned: vec!["a".to_owned(), "b".to_owned()],
            recent: vec!["c".to_owned()],
        };
        assert_eq!(parse(&to_text(&prefs)), prefs);
    }

    #[test]
    fn filters_pinned_to_installed_apps() {
        let apps = vec![sample_app("a"), sample_app("b")];
        let prefs = Prefs {
            pinned: vec!["name:b".to_owned(), "usunięta".to_owned()],
            recent: Vec::new(),
        };
        let found = prefs.pinned_apps(&apps);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].key(), "name:b");
    }
}
