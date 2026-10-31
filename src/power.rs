//! Menu sesji powłoki: ustawienia, blokada, wylogowanie, restart i wyłączenie.
//!
//! Wylogowanie idzie przez `loginctl` (kończy sesję po identyfikatorze), restart
//! i wyłączenie przez `systemctl` (logind obsługuje je przez D-Bus/polkit).
//! Blokadę otwiera natywny `luna-lock` (overlay layer-shell); gdy go brak, zostaje
//! `loginctl lock-session` dla zewnętrznego lockera.

use std::process::Command;

/// Akcja pozycji menu zasilania.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PowerAction {
    /// Otwórz centrum ustawień Luny.
    Settings,
    /// Zablokuj ekran.
    Lock,
    /// Zakończ bieżącą sesję.
    Logout,
    /// Uruchom ponownie komputer.
    Reboot,
    /// Wyłącz komputer.
    PowerOff,
}

/// Liczba pozycji menu sesji — spójna z [`items`].
///
/// Menu rysuje się i reaguje na kliknięcia na podstawie tej liczby, więc
/// dodanie pozycji wymaga jej zwiększenia (pilnuje tego test `items_match_count`).
pub const ITEM_COUNT: usize = 5;

/// Pojedyncza pozycja menu.
#[derive(Debug, Clone)]
pub struct PowerItem {
    pub label: String,
    pub action: PowerAction,
}

/// Pozycje menu (z lokalizacją).
pub fn items() -> Vec<PowerItem> {
    vec![
        PowerItem {
            label: libluna_i18n::tr("Settings"),
            action: PowerAction::Settings,
        },
        PowerItem {
            label: libluna_i18n::tr("Lock screen"),
            action: PowerAction::Lock,
        },
        PowerItem {
            label: libluna_i18n::tr("Log out"),
            action: PowerAction::Logout,
        },
        PowerItem {
            label: libluna_i18n::tr("Restart"),
            action: PowerAction::Reboot,
        },
        PowerItem {
            label: libluna_i18n::tr("Shut down"),
            action: PowerAction::PowerOff,
        },
    ]
}

/// Wykonuje akcję (`loginctl` w tle; powłoka zaraz zniknie).
pub fn perform(action: PowerAction) {
    // Ślad w logu sesji: bez tego nie da się odróżnić świadomego wylogowania
    // (klik w menu) od sesji ubeionej sygnałem przez system.
    eprintln!(
        "luna-shell: akcja sesji: {action:?} (XDG_SESSION_ID={:?})",
        std::env::var("XDG_SESSION_ID").ok()
    );
    match action {
        PowerAction::Settings => {
            spawn("luna-settings", &[]);
        }
        PowerAction::Lock => {
            // Preferujemy natywny ekran blokady. Gdy go brak (stara instalacja),
            // zostaje sygnał logind — zadziała, jeśli sesja ma inny locker.
            if !spawn("luna-lock", &[]) {
                spawn("loginctl", &["lock-session".to_owned()]);
            }
        }
        PowerAction::Logout => {
            if let Some(args) = logout_args(
                std::env::var("XDG_SESSION_ID").ok().as_deref(),
                std::env::var("USER").ok().as_deref(),
            ) {
                spawn("loginctl", &args);
            }
        }
        // UWAGA: `loginctl` **nie ma** poleceń `reboot`/`poweroff` (tylko systemctl).
        PowerAction::Reboot | PowerAction::PowerOff => {
            if let Some((program, verb)) = power_command(action) {
                spawn(program, &[verb.to_owned()]);
            }
        }
    }
}

/// Program i polecenie dla restartu/wyłączenia (testowalne bez uruchamiania).
fn power_command(action: PowerAction) -> Option<(&'static str, &'static str)> {
    match action {
        PowerAction::Reboot => Some(("systemctl", "reboot")),
        PowerAction::PowerOff => Some(("systemctl", "poweroff")),
        _ => None,
    }
}

/// Argumenty `loginctl` do wylogowania: najpierw sesja, w ostateczności cały user.
///
/// Rozdzielone od [`perform`], żeby dało się to przetestować bez uruchamiania czegokolwiek.
fn logout_args(session_id: Option<&str>, user: Option<&str>) -> Option<Vec<String>> {
    match (
        session_id.filter(|s| !s.is_empty()),
        user.filter(|u| !u.is_empty()),
    ) {
        (Some(id), _) => Some(vec!["terminate-session".to_owned(), id.to_owned()]),
        (None, Some(u)) => Some(vec!["terminate-user".to_owned(), u.to_owned()]),
        _ => None,
    }
}

fn spawn(program: &str, args: &[String]) -> bool {
    match Command::new(program).args(args).spawn() {
        Ok(_) => true,
        Err(err) => {
            // Log sesji (`luna-session` przekierowuje stderr) — bez tego brak
            // binarki kończył się „nic się nie stało” i cichym błędem.
            eprintln!("luna: nie udało się uruchomić {program}: {err}");
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn logout_prefers_the_session() {
        assert_eq!(
            logout_args(Some("c3"), Some("patryks")),
            Some(vec!["terminate-session".into(), "c3".into()])
        );
    }

    #[test]
    fn logout_falls_back_to_user() {
        assert_eq!(
            logout_args(None, Some("patryks")),
            Some(vec!["terminate-user".into(), "patryks".into()])
        );
        assert_eq!(
            logout_args(Some(""), Some("patryks")),
            Some(vec!["terminate-user".into(), "patryks".into()])
        );
    }

    #[test]
    fn logout_without_any_target_is_a_noop() {
        assert_eq!(logout_args(None, None), None);
        assert_eq!(logout_args(Some(""), None), None);
    }

    #[test]
    fn reboot_and_poweroff_use_systemctl() {
        assert_eq!(
            power_command(PowerAction::Reboot),
            Some(("systemctl", "reboot"))
        );
        assert_eq!(
            power_command(PowerAction::PowerOff),
            Some(("systemctl", "poweroff"))
        );
        assert_eq!(power_command(PowerAction::Lock), None);
    }

    #[test]
    fn items_match_count() {
        assert_eq!(items().len(), ITEM_COUNT);
    }

    #[test]
    fn menu_has_logout_reboot_poweroff() {
        let actions: Vec<_> = items().iter().map(|i| i.action).collect();
        assert_eq!(
            actions,
            vec![
                PowerAction::Settings,
                PowerAction::Lock,
                PowerAction::Logout,
                PowerAction::Reboot,
                PowerAction::PowerOff
            ]
        );
    }
}
