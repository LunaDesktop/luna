//! Odczyt kalendarza `luna-clock` dla panelu.
//!
//! Panel tylko czyta plik wydarzeń aplikacji — nie zapisuje go i nie wysyła
//! nigdzie danych. Format jest ten sam co w `apps/luna-clock/src/store.rs`:
//! `id<TAB>RRRR-MM-DD<TAB>start<TAB>minuty<TAB>tytuł<TAB>notatka`.

use std::path::PathBuf;

use libluna_core::config::{config_file, parse_pairs};
use libluna_core::holidays;

/// Wydarzenie widoczne w panelu (bez notatki — panel pokazuje tylko plan dnia).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CalendarEvent {
    /// `RRRR-MM-DD`.
    pub date: String,
    /// Minuta od północy; `None` = cały dzień.
    pub start_minute: Option<u32>,
    pub title: String,
}

/// `$XDG_DATA_HOME/luna/clock/events.tsv` (albo `~/.local/share/...`).
pub fn events_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_DATA_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share"))
        })?;
    Some(base.join("luna").join("clock").join("events.tsv"))
}

/// Kraj świąt wybrany w `luna-clock` (albo wykryty z systemu).
///
/// Respektuje przełącznik „Pokazuj święta” z `clock.conf`. Gdy aplikacja
/// nigdy nie była uruchamiana, kraj jest wykrywany z locale/strefy.
pub fn preferred_country() -> Option<&'static str> {
    let text = config_file("clock.conf").and_then(|path| std::fs::read_to_string(path).ok());
    match text {
        Some(text) => country_from_config(&text),
        None => holidays::detect_country(),
    }
}

/// Wydzielone z [`preferred_country`] dla testów bez systemu plików.
pub fn country_from_config(text: &str) -> Option<&'static str> {
    let mut show = true;
    let mut selected = None;
    for (key, value) in parse_pairs(text) {
        match key.as_str() {
            "show_holidays" => show = matches!(value.trim(), "1" | "true" | "yes" | "on"),
            "country" => selected = holidays::normalize_country(&value),
            _ => {}
        }
    }
    if show {
        selected.or_else(holidays::detect_country)
    } else {
        None
    }
}

/// Znacznik czasu pliku — pozwala odświeżyć listę tylko po zmianie.
pub fn stamp() -> Option<std::time::SystemTime> {
    events_path()
        .and_then(|path| std::fs::metadata(path).ok())
        .and_then(|meta| meta.modified().ok())
}

pub fn load_events() -> Vec<CalendarEvent> {
    events_path()
        .and_then(|path| std::fs::read_to_string(path).ok())
        .map(|text| parse_events(&text))
        .unwrap_or_default()
}

/// Parsuje plik wydarzeń (uszkodzone linie są pomijane).
pub fn parse_events(text: &str) -> Vec<CalendarEvent> {
    let mut events = Vec::new();
    for line in text.lines() {
        let line = line.trim_end_matches('\r');
        if line.trim().is_empty() || line.trim_start().starts_with('#') {
            continue;
        }
        let mut fields = line.split('\t');
        let Some(_id) = fields.next().and_then(|id| id.trim().parse::<u64>().ok()) else {
            continue;
        };
        let Some(date) = fields.next().map(str::trim) else {
            continue;
        };
        if !valid_date(date) {
            continue;
        }
        let start = fields.next().unwrap_or("").trim();
        let start_minute = if start == "-" {
            None
        } else {
            match start.parse::<u32>().ok().filter(|minute| *minute < 1_440) {
                Some(minute) => Some(minute),
                None => continue,
            }
        };
        let _duration = fields.next();
        let title = unescape(fields.next().unwrap_or(""));
        if title.trim().is_empty() {
            continue;
        }
        events.push(CalendarEvent {
            date: date.to_owned(),
            start_minute,
            title,
        });
    }
    events.sort_by_key(|event| {
        (
            event.date.clone(),
            event.start_minute.unwrap_or(0),
            event.title.clone(),
        )
    });
    events
}

fn valid_date(date: &str) -> bool {
    let bytes = date.as_bytes();
    if bytes.len() != 10 || bytes[4] != b'-' || bytes[7] != b'-' {
        return false;
    }
    if !bytes
        .iter()
        .enumerate()
        .all(|(index, byte)| matches!(index, 4 | 7) || byte.is_ascii_digit())
    {
        return false;
    }
    let month = date[5..7].parse::<u32>().unwrap_or(0);
    let day = date[8..10].parse::<u32>().unwrap_or(0);
    (1..=12).contains(&month) && (1..=31).contains(&day)
}

fn unescape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            out.push(ch);
            continue;
        }
        match chars.next() {
            Some('t') => out.push('\t'),
            Some('n') => out.push('\n'),
            Some('\\') => out.push('\\'),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_events_and_skips_bad_lines() {
        let text = "# komentarz\n\
                    1\t2026-10-31\t570\t45\tLekarz\\tbilans\tnotatka\n\
                    2\t2026-10-31\t-\t0\tCały dzień\t\n\
                    zła\t2026-10-31\t-\t0\tPominięte\t\n\
                    3\t2026-13-01\t-\t0\tZły miesiąc\t\n";
        let events = parse_events(text);
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].start_minute, None);
        assert_eq!(events[1].start_minute, Some(570));
        assert_eq!(events[1].title, "Lekarz\tbilans");
    }

    #[test]
    fn date_validation() {
        assert!(valid_date("2026-10-31"));
        assert!(!valid_date("2026-13-01"));
        assert!(!valid_date("31.10.2026"));
    }

    #[test]
    fn reads_country_and_holiday_switch_from_clock_config() {
        assert_eq!(country_from_config("country=PL\n"), Some("PL"));
        assert_eq!(country_from_config("country=de\n"), Some("DE"));
        assert_eq!(country_from_config("country=PL\nshow_holidays=0\n"), None);
        // Nieznany kod degraduje się do wykrywania z systemu.
        assert_eq!(
            country_from_config("country=XX\n"),
            holidays::detect_country()
        );
    }
}
