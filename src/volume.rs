//! Podgląd głośności — źródło zdarzeń dla OSD.
//!
//! Dominujący panel pokazuje OSD po zmianie głośności/wyciszenia (np. klawisze
//! multimedialne). Nie mamy tu magistrali PipeWire, więc stan odczytujemy w tle
//! przez `wpctl` (patrz [`crate::system::read_volume`]) i zgłaszamy zmianę licznikiem
//! [`VolumeWatcher::current`] — UI porównuje licznik i pokazuje OSD.

use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Odstęp odpytywania głośności (s).
const POLL: Duration = Duration::from_millis(1200);

#[derive(Default)]
struct Shared {
    seq: u64,
    volume: Option<(u8, bool)>,
}

/// Pilnuje realnych zmian głośności.
///
/// Po starcie sesji PipeWire odpowiada dopiero po chwili, więc pierwszy udany odczyt
/// **nie** jest zmianą — inaczej powłoka witała użytkownika toastem „Głośność”.
/// Chwilowy brak odczytu (`None`) też nie liczy się jako zmiana.
#[derive(Default)]
struct Baseline {
    seen: Option<(u8, bool)>,
}

impl Baseline {
    /// `true`, gdy wartość naprawdę się zmieniła (pierwszy odczyt i zaniki nie liczą się).
    fn observe(&mut self, now: Option<(u8, bool)>) -> bool {
        let Some(now) = now else { return false };
        let first = self.seen.is_none();
        let changed = self.seen != Some(now);
        self.seen = Some(now);
        !first && changed
    }
}

/// Obserwator głośności (własny wątek odpytywania).
pub struct VolumeWatcher {
    shared: Arc<Mutex<Shared>>,
}

impl VolumeWatcher {
    /// Startuje wątek i zwraca uchwyt.
    pub fn spawn() -> Self {
        let shared: Arc<Mutex<Shared>> = Arc::new(Mutex::new(Shared::default()));
        let writer = Arc::clone(&shared);
        std::thread::spawn(move || {
            let mut baseline = Baseline::default();
            loop {
                std::thread::sleep(POLL);
                let now = crate::system::read_volume();
                if baseline.observe(now) {
                    if let Ok(mut s) = writer.lock() {
                        s.seq += 1;
                        s.volume = now;
                    }
                }
            }
        });
        Self { shared }
    }

    /// Bieżący `(licznik zmian, głośność)`. Licznik rośnie przy każdej zmianie.
    pub fn current(&self) -> (u64, Option<(u8, bool)>) {
        let s = self.shared.lock().unwrap_or_else(|e| e.into_inner());
        (s.seq, s.volume)
    }
}

/// Etykieta OSD dla stanu głośności.
pub fn label(muted: bool) -> String {
    if muted {
        libluna_i18n::tr("Muted")
    } else {
        libluna_i18n::tr("Volume")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn label_reflects_mute() {
        assert_eq!(label(false), "Głośność");
        assert_eq!(label(true), "Wyciszono");
    }

    #[test]
    fn fresh_watcher_has_no_change_yet() {
        // Nie startujemy wątku — interesuje nas tylko stan początkowy.
        let w = VolumeWatcher {
            shared: Arc::new(Mutex::new(Shared::default())),
        };
        assert_eq!(w.current(), (0, None));
    }

    #[test]
    fn first_read_is_not_a_change() {
        let mut b = Baseline::default();
        // Pierwszy udany odczyt (np. PipeWire wstaje po starcie) → bez OSD.
        assert!(!b.observe(Some((80, false))));
        // Ten sam poziom → bez OSD.
        assert!(!b.observe(Some((80, false))));
        // Realna zmiana → OSD.
        assert!(b.observe(Some((50, false))));
        // Chwilowy brak odczytu nie jest zmianą i nie psuje bazy.
        assert!(!b.observe(None));
        assert!(!b.observe(Some((50, false))));
        // Wyciszenie to też zmiana.
        assert!(b.observe(Some((50, true))));
    }
}
