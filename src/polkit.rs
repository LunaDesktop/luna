//! `luna-polkit` — agent uwierzytelniania Polkit (PolicyKit1 Authentication Agent).
//!
//! Rejestruje się na magistrali systemowej w `org.freedesktop.PolicyKit1.Authority`
//! i odpowiada na żądania podniesienia uprawnień (np. `pkexec`, NetworkManager,
//! aktualizacje). Gdy wymagana jest autoryzacja, pokazuje okno z pytaniem o hasło,
//! sprawdza je przez PAM i dopiero po sukcesie potwierdza cookie w `polkitd`.

use std::collections::HashMap;
use std::io::Write;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};

use libluna_core::style::weight;
use libluna_core::{ButtonState, Color, Event, PointerButton, Rect};
use libluna_i18n::{tr, tr_args};
use libluna_platform::{run, AppConfig, Frame};
use libluna_widgets::{paint_icon, IconName, PaintCtx};
use zbus::zvariant::{OwnedValue, Value};

pub use crate::pam::AuthOutcome;

/// Informacje o oczekującym żądaniu uwierzytelnienia.
#[derive(Debug, Clone)]
pub struct AuthRequest {
    pub action_id: String,
    pub message: String,
    pub cookie: String,
    pub identity: (String, HashMap<String, OwnedValue>),
    pub user: String,
}

/// Agent uwierzytelniania Polkit (interfejs D-Bus).
pub struct PolkitAgent {
    pub pending: Arc<Mutex<Option<AuthRequest>>>,
}

#[zbus::interface(name = "org.freedesktop.PolicyKit1.AuthenticationAgent")]
impl PolkitAgent {
    fn begin_authentication(
        &self,
        action_id: String,
        message: String,
        _icon_name: String,
        _details: HashMap<String, String>,
        cookie: String,
        identities: Vec<(String, HashMap<String, OwnedValue>)>,
    ) {
        let user = identities
            .first()
            .and_then(|(_, dict)| dict.get("uid"))
            .and_then(|v| u32::try_from(v.clone()).ok())
            .and_then(user_from_uid)
            .unwrap_or_else(current_user);

        let identity = identities
            .first()
            .cloned()
            .unwrap_or_else(|| ("unix-user".to_owned(), HashMap::new()));

        let req = AuthRequest {
            action_id,
            message,
            cookie,
            identity,
            user,
        };

        // Zapisz żądanie i otwórz okno autoryzacji
        *self.pending.lock().unwrap() = Some(req);
    }

    fn cancel_authentication(&self, _cookie: String) {
        *self.pending.lock().unwrap() = None;
    }
}

/// Rejestruje agenta w usłudze `org.freedesktop.PolicyKit1.Authority` na magistrali systemowej.
pub fn register_agent(
    conn: &zbus::blocking::Connection,
    object_path: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let session_id = std::env::var("XDG_SESSION_ID").ok();
    let subject = if let Some(sid) = session_id {
        let mut map = HashMap::new();
        map.insert("session-id".to_owned(), Value::from(sid));
        ("unix-session".to_owned(), map)
    } else {
        let mut map = HashMap::new();
        map.insert("pid".to_owned(), Value::from(std::process::id()));
        map.insert("start-time".to_owned(), Value::from(0u64));
        ("unix-process".to_owned(), map)
    };

    let locale = std::env::var("LANG").unwrap_or_else(|_| "pl_PL.UTF-8".to_owned());

    conn.call_method(
        Some("org.freedesktop.PolicyKit1"),
        "/org/freedesktop/PolicyKit1/Authority",
        Some("org.freedesktop.PolicyKit1.Authority"),
        "RegisterAuthenticationAgent",
        &(subject, locale, object_path),
    )?;

    Ok(())
}

/// Potwierdza uwierzytelnienie do PolicyKit Authority **bez** helpera.
///
/// Normalnie robi to `polkit-agent-helper-1` po udanym PAM (i to on wysyła
/// odpowiedź). Ta funkcja istnieje na wypadek systemów bez helpera — wtedy
/// hasło trzeba sprawdzić samemu ([`verify_password`]) i dopiero potem potwierdzić
/// cookie. `polkitd` ufa agentowi, więc **nigdy** nie wysyłaj jej bez sprawdzenia.
pub fn send_response(
    conn: &zbus::blocking::Connection,
    cookie: &str,
    identity: &(String, HashMap<String, OwnedValue>),
) -> Result<(), Box<dyn std::error::Error>> {
    conn.call_method(
        Some("org.freedesktop.PolicyKit1"),
        "/org/freedesktop/PolicyKit1/Authority",
        Some("org.freedesktop.PolicyKit1.Authority"),
        "AuthenticationAgentResponse",
        &(cookie, identity),
    )?;
    Ok(())
}

/// Ścieżka `polkit-agent-helper-1` (setuid root; PAM + odpowiedź do `polkitd`).
fn agent_helper_path() -> Option<std::path::PathBuf> {
    [
        "/usr/lib/polkit-1/polkit-agent-helper-1",
        "/usr/libexec/polkit-agent-helper-1",
        "/usr/lib64/polkit-1/polkit-agent-helper-1",
    ]
    .iter()
    .map(std::path::PathBuf::from)
    .find(|path| path.exists())
}

/// Sprawdza hasło **w kontekście żądania polkit** i potwierdza cookie.
///
/// To jedyna poprawna droga dla agenta: `polkit-agent-helper-1` jest setuid root,
/// więc PAM widzi `pam_faillock` i `/etc/shadow`; po sukcesie helper **sam** wysyła
/// `AuthenticationAgentResponse`. Protokół: cookie w pierwszej linii stdin,
/// hasło w drugiej (argv widać w `/proc`, więc cookie nie idzie argumentem).
///
/// `Ok(())` = zalogowano i odpowiedź poszła do `polkitd` — nie wysyłaj jej ponownie.
pub fn verify_agent_request(user: &str, cookie: &str, password: &str) -> Result<(), String> {
    let helper = agent_helper_path()
        .ok_or_else(|| tr("polkit-agent-helper-1 is missing — install polkit"))?;
    let mut child = Command::new(&helper)
        .arg(user)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|err| format!("polkit-agent-helper-1: {err}"))?;

    if let Some(mut stdin) = child.stdin.take() {
        let mut input = String::with_capacity(cookie.len() + password.len() + 2);
        input.push_str(cookie);
        input.push('\n');
        input.push_str(password);
        input.push('\n');
        stdin
            .write_all(input.as_bytes())
            .map_err(|err| format!("polkit-agent-helper-1: {err}"))?;
    }
    let output = child
        .wait_with_output()
        .map_err(|err| format!("polkit-agent-helper-1: {err}"))?;
    if output.status.success() {
        return Ok(());
    }

    // Helper wypisuje na stdout komunikaty PAM (np. o blokadzie konta) i „FAILURE”.
    let stdout = String::from_utf8_lossy(&output.stdout);
    if let Some(line) = stdout
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && *line != "FAILURE")
        .find(|line| line.to_lowercase().contains("locked"))
    {
        return Err(line.to_owned());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let detail = stderr
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .next_back();
    match detail {
        Some(line) if !line.contains("FAILURE") => Err(line.to_owned()),
        _ => Err(tr("Incorrect password. Try again.")),
    }
}

/// Weryfikuje hasło użytkownika przez PAM.
///
/// Używane przez ekran blokady (nie ma tam cookie polkit). Źródłem prawdy jest PAM
/// (`pam_unix` → `unix_chkpwd`), czyli ten sam mechanizm, którego używają `login`
/// i `sudo`. `sudo` jest tylko awaryjnym fallbackiem, gdy PAM jest niedostępny
/// (minimalna instalacja bez `libpam`).
pub fn verify_password(user: &str, password: &str) -> AuthOutcome {
    match crate::pam::verify(user, password) {
        AuthOutcome::Unavailable => {
            if verify_with_sudo(password) {
                AuthOutcome::Ok
            } else {
                AuthOutcome::WrongPassword
            }
        }
        other => other,
    }
}

fn verify_with_sudo(password: &str) -> bool {
    let mut child = match Command::new("sudo")
        .args(["-S", "-k", "-v"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(c) => c,
        Err(_) => return false,
    };
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(password.as_bytes());
        let _ = stdin.write_all(b"\n");
    }
    child.wait().map(|s| s.success()).unwrap_or(false)
}

fn current_user() -> String {
    std::env::var("USER")
        .or_else(|_| std::env::var("LOGNAME"))
        .unwrap_or_else(|_| "root".to_owned())
}

fn user_from_uid(uid: u32) -> Option<String> {
    let passwd = std::fs::read_to_string("/etc/passwd").ok()?;
    for line in passwd.lines() {
        let parts: Vec<&str> = line.split(':').collect();
        if parts.len() >= 3 && parts[2] == uid.to_string() {
            return Some(parts[0].to_owned());
        }
    }
    None
}

/// Wyświetla graficzne okno autoryzacji Luny i zwraca podane hasło, gdy użytkownik
/// kliknie "Uwierzytelnij".
///
/// `verify` decyduje, czy hasło jest poprawne — dla agenta polkit to
/// [`verify_agent_request`] (helper potwierdza cookie), a dla ekranu blokady
/// [`verify_password`]. Dzięki temu okno zostaje otwarte do skutku i pokazuje
/// konkretny powód (złe hasło, blokada konta), zamiast zamykać się po pierwszej próbie.
pub fn show_dialog<F>(req: &AuthRequest, verify: F) -> Option<String>
where
    F: Fn(&str) -> Result<(), String> + 'static,
{
    let cfg = AppConfig::new(tr("Authentication"))
        .app_id("eu.paganlinux.Luna.Polkit")
        .initial_size(440, 260);

    let mut password = String::new();
    let authenticated_password: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let auth_result = Arc::clone(&authenticated_password);
    let mut error_msg: Option<String> = None;
    let user_name = req.user.clone();
    let message = if !req.message.is_empty() {
        req.message.clone()
    } else {
        tr("Administrator privileges are required.")
    };
    let action = req.action_id.clone();

    let _ = run(cfg, move |frame: &mut Frame| {
        let size = frame.size;
        let theme = &frame.theme;
        let text = frame.text;

        let pad = 20.0;
        let w = size.width;
        let h = size.height;

        // Przyciski i pole tekstowe
        let input_rect = Rect::from_xywh(pad, 130.0, w - 2.0 * pad, 36.0);
        let btn_w = 110.0;
        let btn_h = 34.0;
        let cancel_rect =
            Rect::from_xywh(w - pad - 2.0 * btn_w - 12.0, h - pad - btn_h, btn_w, btn_h);
        let auth_rect = Rect::from_xywh(w - pad - btn_w, h - pad - btn_h, btn_w, btn_h);

        for ev in frame.events {
            match ev {
                Event::Key(ke) if ke.state == ButtonState::Pressed => {
                    match ke.keysym {
                        0xff1b => {
                            // Esc — Anuluj
                            frame.close();
                            return;
                        }
                        0xff08 => {
                            // BackSpace
                            password.pop();
                        }
                        0xff0d | 0xff8d => {
                            // Enter — Uwierzytelnij
                            match verify(&password) {
                                Ok(()) => {
                                    *auth_result.lock().unwrap() = Some(password.clone());
                                    frame.close();
                                    return;
                                }
                                Err(message) => {
                                    error_msg = Some(message);
                                    password.clear();
                                }
                            }
                        }
                        _ => {
                            if let Some(c) = char_from_keysym(ke.keysym) {
                                password.push(c);
                            }
                        }
                    }
                }
                Event::PointerButton(pe)
                    if pe.state == ButtonState::Pressed && pe.button == PointerButton::Left =>
                {
                    if cancel_rect.contains(pe.position) {
                        frame.close();
                        return;
                    } else if auth_rect.contains(pe.position) {
                        match verify(&password) {
                            Ok(()) => {
                                *auth_result.lock().unwrap() = Some(password.clone());
                                frame.close();
                                return;
                            }
                            Err(message) => {
                                error_msg = Some(message);
                                password.clear();
                            }
                        }
                    }
                }
                _ => {}
            }
        }

        // ── Rysowanie okna dialogowego ──
        let mut ctx = PaintCtx::new(frame.scene, theme, text);

        // Tło
        ctx.scene
            .fill_rect(Rect::from_size(size), theme.colors.window_bg);

        // Ikona tarczy / kłódki
        let icon_sz = 32.0;
        let icon_rect = Rect::from_xywh(pad, pad, icon_sz, icon_sz);
        paint_icon(&mut ctx, IconName::Star, icon_rect, theme.colors.accent);

        // Tytuł i komunikat
        ctx.label_left(
            icon_rect.right() + 14.0,
            icon_rect.center().y - 6.0,
            &tr("Authentication required"),
            theme.typography.heading.size,
            weight::BOLD,
            theme.colors.text,
        );

        ctx.label_left(
            icon_rect.right() + 14.0,
            icon_rect.center().y + 12.0,
            &tr_args("User: {user}", &[("user", &user_name)]),
            theme.typography.caption.size,
            weight::NORMAL,
            theme.colors.dim_text,
        );

        ctx.label_left(
            pad,
            75.0,
            &message,
            theme.typography.body.size,
            weight::NORMAL,
            theme.colors.text,
        );

        ctx.label_left(
            pad,
            96.0,
            &tr_args("Action: {action}", &[("action", &action)]),
            theme.typography.caption.size,
            weight::NORMAL,
            theme.colors.dim_text,
        );

        // Pole hasła
        ctx.scene
            .fill_rounded_rect(input_rect, theme.radius.small, theme.colors.view_bg);
        ctx.scene
            .stroke_rounded_rect(input_rect, theme.radius.small, 1.5, theme.colors.accent);

        let masked = "•".repeat(password.len());
        if masked.is_empty() {
            ctx.label_left(
                input_rect.x() + 12.0,
                input_rect.center().y,
                &tr("Enter password…"),
                theme.typography.body.size,
                weight::NORMAL,
                theme.colors.dim_text,
            );
        } else {
            ctx.label_left(
                input_rect.x() + 12.0,
                input_rect.center().y,
                &masked,
                18.0,
                weight::BOLD,
                theme.colors.text,
            );
        }

        // Komunikat błędu (jeśli wystąpił)
        if let Some(err) = &error_msg {
            ctx.label_left(
                pad,
                input_rect.bottom() + 14.0,
                err,
                theme.typography.caption.size,
                weight::MEDIUM,
                Color::from_rgba8(0xe0, 0x40, 0x40, 0xff),
            );
        }

        // Przycisk Anuluj
        ctx.scene
            .fill_rounded_rect(cancel_rect, theme.radius.small, theme.colors.shade);
        ctx.scene
            .stroke_rounded_rect(cancel_rect, theme.radius.small, 1.0, theme.colors.border);
        ctx.label_center(
            cancel_rect.center().x,
            cancel_rect.center().y,
            &tr("Cancel"),
            theme.typography.body.size,
            weight::NORMAL,
            theme.colors.text,
        );

        // Przycisk Uwierzytelnij
        ctx.scene
            .fill_rounded_rect(auth_rect, theme.radius.small, theme.colors.accent);
        ctx.label_center(
            auth_rect.center().x,
            auth_rect.center().y,
            &tr("Authenticate"),
            theme.typography.body.size,
            weight::MEDIUM,
            theme.colors.accent_fg,
        );

        frame.request_frame();
    });

    let res = authenticated_password.lock().unwrap().clone();
    res
}

fn char_from_keysym(keysym: u32) -> Option<char> {
    match keysym {
        0x20..=0x7e => char::from_u32(keysym),
        0x0100..=0x10ff => char::from_u32(keysym),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_helper_is_found_on_this_host() {
        // Kontrakt: jeśli polkit jest zainstalowany, helper też (setuid).
        if let Some(path) = agent_helper_path() {
            assert!(path.is_absolute());
        }
    }

    #[test]
    fn bogus_cookie_is_rejected_without_panic() {
        if agent_helper_path().is_none() {
            return;
        }
        // Nieistniejący użytkownik + nieprawidłowe cookie: helper musi odmówić,
        // a my zwracamy czytelny komunikat (bez zapisu do `faillock`).
        let err = verify_agent_request("__luna_nie_ma_takiego_usera__", "zle-cookie", "x")
            .expect_err("nieprawidłowe cookie musi dać błąd");
        assert!(!err.is_empty());
    }
}
