//! Harmonogram automatycznego motywu ustawiany w `luna-clock`.
//!
//! Powłoka czyta te same `~/.config/luna/clock.conf` co aplikacja, więc zmiana
//! jasny/ciemny działa **także po zamknięciu zegara** — wystarczy działająca
//! sesja Luny. Zapisujemy wyłącznie standardowy stan systemowy
//! (`~/.config/luna/system-dark`), bez uprawnień roota i bez sieci.

use std::path::PathBuf;

use libluna_core::config::{config_file, parse_pairs};

/// Godziny przełączania motywu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThemeSchedule {
    pub enabled: bool,
    pub dark_hour: u32,
    pub light_hour: u32,
    /// Ostatnio zastosowany wariant (`None` = jeszcze nie sprawdzaliśmy).
    applied: Option<bool>,
}

impl Default for ThemeSchedule {
    fn default() -> Self {
        Self {
            enabled: false,
            dark_hour: 19,
            light_hour: 7,
            applied: None,
        }
    }
}

impl ThemeSchedule {
    pub fn path() -> Option<PathBuf> {
        config_file("clock.conf")
    }

    /// Znacznik czasu konfiguracji — harmonogram przeładowujemy po zmianie.
    pub fn stamp() -> Option<std::time::SystemTime> {
        Self::path()
            .and_then(|path| std::fs::metadata(path).ok())
            .and_then(|meta| meta.modified().ok())
    }

    pub fn load() -> Self {
        let text = Self::path()
            .and_then(|path| std::fs::read_to_string(path).ok())
            .unwrap_or_default();
        Self::parse(&text)
    }

    pub fn parse(text: &str) -> Self {
        let mut schedule = Self::default();
        for (key, value) in parse_pairs(text) {
            match key.as_str() {
                "auto_theme" => {
                    schedule.enabled = matches!(value.as_str(), "1" | "true" | "yes" | "on")
                }
                "dark_hour" => {
                    if let Ok(hour) = value.parse::<u32>() {
                        schedule.dark_hour = hour.min(23);
                    }
                }
                "light_hour" => {
                    if let Ok(hour) = value.parse::<u32>() {
                        schedule.light_hour = hour.min(23);
                    }
                }
                _ => {}
            }
        }
        schedule.applied = Some(libluna_core::appearance::system_dark());
        schedule
    }

    /// Czy o `hour` ma obowiązywać ciemny wariant.
    pub fn is_dark_at(&self, hour: u32) -> bool {
        if self.dark_hour == self.light_hour {
            return false;
        }
        let hour = hour.min(23);
        if self.dark_hour < self.light_hour {
            hour >= self.dark_hour && hour < self.light_hour
        } else {
            hour >= self.dark_hour || hour < self.light_hour
        }
    }

    /// Zapisuje systemowy wariant, jeśli zmienił się względem poprzedniego.
    /// Zwraca `true`, gdy plik został zapisany.
    pub fn apply(&mut self, hour: u32) -> bool {
        if !self.enabled {
            return false;
        }
        let dark = self.is_dark_at(hour);
        if self.applied == Some(dark) {
            return false;
        }
        self.applied = Some(dark);
        libluna_core::appearance::set_system_dark(dark).is_ok()
    }
}

/// Bieżąca godzina lokalna (0–23) bez zależności od formatu zegara w panelu.
pub fn current_hour() -> u32 {
    unsafe {
        let now = libc::time(std::ptr::null_mut());
        let mut tm = std::mem::MaybeUninit::<libc::tm>::uninit();
        let result = libc::localtime_r(&now, tm.as_mut_ptr());
        if result.is_null() {
            0
        } else {
            (*result).tm_hour.clamp(0, 23) as u32
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_clock_config_and_clamps_hours() {
        let schedule =
            ThemeSchedule::parse("auto_theme=1\ndark_hour=19\nlight_hour=7\nshow_seconds=0\n");
        assert!(schedule.enabled);
        assert_eq!(schedule.dark_hour, 19);
        assert_eq!(schedule.light_hour, 7);

        let schedule = ThemeSchedule::parse("auto_theme=1\ndark_hour=99\nlight_hour=99\n");
        assert_eq!(schedule.dark_hour, 23);
        assert_eq!(schedule.light_hour, 23);
        assert!(!schedule.is_dark_at(12));
    }

    #[test]
    fn disabled_schedule_never_applies() {
        let mut schedule = ThemeSchedule::parse("auto_theme=0\ndark_hour=19\nlight_hour=7\n");
        assert!(!schedule.apply(22));
    }

    #[test]
    fn dark_interval_crosses_midnight() {
        let schedule = ThemeSchedule::parse("auto_theme=1\ndark_hour=19\nlight_hour=7\n");
        assert!(schedule.is_dark_at(19));
        assert!(schedule.is_dark_at(0));
        assert!(!schedule.is_dark_at(7));
        assert!(!schedule.is_dark_at(18));
    }

    #[test]
    fn current_hour_is_sane() {
        assert!(current_hour() < 24);
    }
}
