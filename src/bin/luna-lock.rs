//! `luna-lock` — ekran blokady Luny.
//!
//! Powierzchnia **overlay** (`wlr-layer-shell`) z klawiaturą wyłączną: Selene nadaje
//! jej fokus od razu po zmapowaniu, a warstwa overlay jest nad panelami i oknami.
//! Na wielu monitorach ta sama powierzchnia jest rysowana na każdym wyjściu.
//!
//! Hasło weryfikujemy przez PAM (`luna::pam`), a `sudo` jest tylko fallbackiem,
//! gdy PAM jest niedostępny w systemie.
//! Po powodzeniu okno się zamyka; nie da się go zamknąć skrótem kompozytora.

use std::error::Error;
use std::os::fd::AsRawFd;
use std::path::Path;
use std::sync::mpsc::{self, TryRecvError};
use std::sync::Arc;
use std::time::Duration;

use libluna_core::style::weight;
use libluna_core::{
    ButtonState, Event, IconSettings, IconTheme, KeyEvent, LockSettings, LockWallpaper,
    PointerButton, Rect, Size, Theme, ThemePreference, ThemeSettings,
};
use libluna_i18n::{set_locale, tr, tr_args};
use libluna_platform::{run, AppConfig, Frame, LayerConfig};
use libluna_render::wallpaper::{configured_path, cover, decode_png, load_path};
use libluna_render::ImageData;
use libluna_widgets::{
    paint_icon_themed, Button, ButtonVariant, IconName, PaintCtx, PasswordEntry,
};

/// Minimalna długość hasła, którą w ogóle próbujemy weryfikować.
const MIN_PASSWORD: usize = 1;

/// Co ile odświeżamy UI, gdy trwa weryfikacja hasła (PAM może odpowiedzieć po ~2 s).
const VERIFY_POLL: Duration = Duration::from_millis(50);

struct Lock {
    entry: PasswordEntry,
    error: Option<String>,
    user: String,
    /// Język, w którym zbudowano placeholder i komunikat błędu.
    language: String,
    /// Wynik weryfikacji z wątku tła; `None`, gdy nic nie trwa.
    pending: Option<mpsc::Receiver<luna::polkit::AuthOutcome>>,
}

impl Lock {
    fn new() -> Self {
        let user = std::env::var("USER")
            .or_else(|_| std::env::var("LOGNAME"))
            .unwrap_or_else(|_| "?".to_owned());
        let mut entry = PasswordEntry::new(tr("Password"));
        entry.set_focused(true);
        Self {
            entry,
            error: None,
            user,
            language: libluna_i18n::locale().language().to_owned(),
            pending: None,
        }
    }

    /// Odświeża napisy po zmianie języka w Ustawieniach (ekran blokady działa
    /// dalej, więc nie może czekać na ponowne uruchomienie).
    fn refresh_language(&mut self) {
        let language = libluna_i18n::locale().language().to_owned();
        if language == self.language {
            return;
        }
        self.language = language;
        self.entry.set_placeholder(tr("Password"));
        self.error = None;
    }

    fn is_verifying(&self) -> bool {
        self.pending.is_some()
    }

    /// Odbiera wynik z wątku weryfikującego (bez blokowania UI).
    fn take_result(&mut self) -> Option<luna::polkit::AuthOutcome> {
        let rx = self.pending.as_ref()?;
        match rx.try_recv() {
            Ok(result) => {
                self.pending = None;
                Some(result)
            }
            Err(TryRecvError::Empty) => None,
            // Wątek padł bez odpowiedzi — traktujemy jak odrzucone hasło.
            Err(TryRecvError::Disconnected) => {
                self.pending = None;
                Some(luna::polkit::AuthOutcome::WrongPassword)
            }
        }
    }

    /// Rozpoczyna weryfikację hasła w tle (PAM nie blokuje schonka i zegara).
    fn attempt(&mut self) {
        if self.is_verifying() {
            return;
        }
        if self.entry.text().chars().count() < MIN_PASSWORD {
            self.error = Some(tr("Enter your password"));
            return;
        }
        let password = self.entry.text().to_owned();
        let user = self.user.clone();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let result = luna::polkit::verify_password(&user, &password);
            if std::env::var("LUNA_DEBUG").is_ok() {
                eprintln!(
                    "luna-lock: próba hasła len={} wynik={result:?}",
                    password.chars().count()
                );
            }
            let _ = tx.send(result);
        });
        self.pending = Some(rx);
        // Pole czyścimy od razu — hasło nie ma sensu na ekranie podczas sprawdzania.
        self.entry.clear();
        self.entry.set_focused(true);
        self.error = None;
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    // Przy `LUNA_PANELS=all` każdy panel to osobny proces — bez tej blokady kliknięcie
    // „Zablokuj ekran” uruchomiłoby po jednym lockerze na monitor. `flock` zwalnia się
    // sam przy zakończeniu procesu, więc plik może bezpiecznie zostać na dysku.
    let _instance = match try_lock_at(&instance_lock_path()) {
        LockAttempt::Acquired(lock) => Some(lock),
        LockAttempt::AlreadyRunning => return Ok(()),
        LockAttempt::Unavailable(err) => {
            eprintln!("luna-lock: nie można utworzyć blokady instancji: {err}");
            None
        }
    };

    // Ekran blokady ma obejmować **wszystkie** wyjścia, także gdy powłoka była
    // uruchomiona per-monitor (`LUNA_PANELS=all` ustawia `LUNA_OUTPUT`).
    std::env::remove_var("LUNA_OUTPUT");
    set_locale(libluna_i18n::preferred_locale());
    libluna_i18n::load_catalogs_from_dirs();

    let system_dark = std::env::var("LUNA_SYSTEM_DARK")
        .map(|v| !v.is_empty() && v != "0")
        .unwrap_or(false);
    let config = AppConfig::new("Luna Lock")
        .app_id("eu.luna.Lock")
        .layer(LayerConfig::lock_screen("luna-lock"))
        .settings(ThemeSettings::load())
        .system_dark(system_dark)
        // Escape czyści hasło; nie może odblokować ekranu przez zamknięcie procesu.
        .escape_to_exit(false);

    let mut lock = Lock::new();
    let lock_settings = LockSettings::load();
    let icon_theme = IconSettings::load().theme;
    let mut background: Option<(Size, Arc<ImageData>)> = None;
    let mut logged = false;
    run(config, move |frame: &mut Frame| {
        if !logged && std::env::var("LUNA_DEBUG").is_ok() {
            logged = true;
            eprintln!(
                "luna-lock: preference={:?} theme={:?}",
                frame.preference(),
                frame.theme.appearance
            );
        }
        // Motyw wybrany w Ustawieniach (ekran blokady to ustawienie użytkownika,
        // więc nie wymaga uprawnień roota). Domyślnie ciemny.
        let base = match lock_settings.theme {
            ThemePreference::Light => Theme::light(),
            ThemePreference::Dark => Theme::dark(),
            ThemePreference::System => {
                if frame.theme.appearance == libluna_core::style::Appearance::Dark {
                    Theme::dark()
                } else {
                    Theme::light()
                }
            }
        };
        let (accent, accent_fg) = match lock_settings.accent {
            Some(accent) => (accent.color(), accent.foreground()),
            None => (frame.theme.colors.accent, frame.theme.colors.accent_fg),
        };
        let theme = base.accented(accent, accent_fg);
        // Tapetę skalujemy raz na rozmiar powierzchni — w każdej klatce idzie tani blit.
        if background.as_ref().map(|(size, _)| *size) != Some(frame.size) {
            background = if lock_settings.wallpaper == LockWallpaper::Desktop {
                wallpaper_for(frame.size).map(|img| (frame.size, Arc::new(img)))
            } else {
                None
            };
        }
        let wallpaper = background.as_ref().map(|(_, img)| Arc::clone(img));
        lock.refresh_language();
        handle_events(&mut lock, frame, &theme);
        match lock.take_result() {
            Some(luna::polkit::AuthOutcome::Ok) => {
                lock.entry.clear();
                lock.error = None;
                frame.close();
            }
            Some(luna::polkit::AuthOutcome::WrongPassword) => {
                lock.entry.clear();
                lock.entry.set_focused(true);
                lock.error = Some(tr("Incorrect password"));
            }
            Some(luna::polkit::AuthOutcome::Locked(detail)) => {
                lock.entry.clear();
                lock.entry.set_focused(true);
                lock.error = Some(match detail {
                    Some(detail) => tr_args(
                        "Account locked after failed attempts: {detail}",
                        &[("detail", &detail)],
                    ),
                    None => {
                        tr("Account temporarily locked after failed attempts — wait a few minutes")
                    }
                });
            }
            Some(luna::polkit::AuthOutcome::Unavailable) => {
                lock.entry.clear();
                lock.entry.set_focused(true);
                lock.error = Some(tr("Authentication is unavailable — PAM is missing"));
            }
            Some(luna::polkit::AuthOutcome::Error(message)) => {
                lock.entry.clear();
                lock.entry.set_focused(true);
                lock.error = Some(message);
            }
            None => {}
        }
        paint(
            &lock,
            frame.scene,
            frame.size,
            &theme,
            frame.text,
            LockLook {
                background: wallpaper,
                show_clock: lock_settings.show_clock,
                icon_theme,
            },
        );
        // Zegar na ekranie blokady; podczas weryfikacji odświeżamy znacznie częściej,
        // żeby wynik z wątku PAM pojawił się bez odczuwalnego opóźnienia.
        let delay = if lock.is_verifying() {
            VERIFY_POLL
        } else {
            Duration::from_secs(1)
        };
        frame.request_frame_after(delay);
    })
}

fn handle_events(lock: &mut Lock, frame: &mut Frame, theme: &Theme) {
    // Podczas weryfikacji ignorujemy wejście — pole jest puste, a wynik i tak
    // nadpisze stan. Dzięki temu wpisane w międzyczasie znaki nie znikają bez śladu.
    if lock.is_verifying() {
        return;
    }
    let lay = layout(frame.size, theme);
    let events = frame.events.to_vec();
    for event in &events {
        match event {
            Event::Key(key) if key.state == ButtonState::Pressed => {
                handle_key(lock, frame, key);
            }
            Event::Paste(text) => {
                // Ekran blokady przyjmuje wklejony tekst (np. z menedżera haseł),
                // ale sam nigdy nie eksportuje hasła do schowka.
                lock.entry.paste(text);
                lock.error = None;
            }
            Event::PointerMotion { position, .. } => {
                lock.entry
                    .set_peek_hover(lock.entry.peek_at(lay.entry, theme, *position));
            }
            Event::PointerButton(button)
                if button.state == ButtonState::Pressed && button.button == PointerButton::Left =>
            {
                if lock.entry.peek_at(lay.entry, theme, button.position) {
                    lock.entry.toggle_peek();
                } else if lay.unlock.contains(button.position) {
                    lock.attempt();
                } else if lock.entry.contains(lay.entry, button.position) {
                    lock.entry.set_focused(true);
                }
            }
            _ => {}
        }
    }
}

fn handle_key(lock: &mut Lock, frame: &mut Frame, key: &KeyEvent) {
    if std::env::var("LUNA_DEBUG").is_ok() {
        eprintln!("luna-lock: key keysym={:x} text={:?}", key.keysym, key.text);
    }
    let ctrl = key.modifiers.ctrl;
    if ctrl && key.keysym == 0x61 {
        lock.entry.select_all();
        return;
    }
    // Wklejanie hasła (Ctrl+V). Kopiowanie/wycinanie jest świadomie zablokowane —
    // ekran blokady nie może wypuścić hasła do systemowego schowka.
    if ctrl && matches!(key.keysym, 0x56 | 0x76) {
        frame.paste_from_clipboard();
        return;
    }
    if !ctrl {
        if let Some(text) = &key.text {
            let plain: String = text.chars().filter(|c| !c.is_control()).collect();
            if !plain.is_empty() {
                lock.entry.insert_str(&plain);
                lock.error = None;
                if std::env::var("LUNA_DEBUG").is_ok() {
                    eprintln!("luna-lock: wpisano {plain:?}");
                }
                return;
            }
        }
    }
    match key.keysym {
        0xff0d | 0xff8d => lock.attempt(),
        0xff1b => {
            lock.entry.clear();
            lock.error = None;
        }
        0xff08 => {
            lock.entry.backspace();
            lock.error = None;
        }
        0xffff => lock.entry.delete_forward(),
        0xff51 => lock.entry.move_left(),
        0xff53 => lock.entry.move_right(),
        0xff50 => lock.entry.home(),
        0xff57 => lock.entry.end(),
        _ => {}
    }
}

/// Uchwyt blokady jednej instancji — żyje przez cały czas działania lockera.
struct InstanceLock(#[allow(dead_code)] std::fs::File);

/// Wynik próby przejęcia blokady.
enum LockAttempt {
    Acquired(InstanceLock),
    /// Inny locker już działa (np. na innym monitorze) — nie ma czego pokazywać.
    AlreadyRunning,
    /// Nie da się użyć blokady (uprawnienia, system plików); lepiej zablokować niż nie.
    Unavailable(std::io::Error),
}

/// Ścieżka pliku blokady w katalogu runtime użytkownika.
fn instance_lock_path() -> std::path::PathBuf {
    let dir = std::env::var_os("XDG_RUNTIME_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("luna");
    dir.join("lock.flock")
}

fn try_lock_at(path: &Path) -> LockAttempt {
    if let Some(parent) = path.parent() {
        if let Err(err) = std::fs::create_dir_all(parent) {
            return LockAttempt::Unavailable(err);
        }
    }
    let file = match std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)
    {
        Ok(f) => f,
        Err(err) => return LockAttempt::Unavailable(err),
    };
    // SAFETY: deskryptor jest ważny przez cały czas życia `file`.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
        return LockAttempt::Acquired(InstanceLock(file));
    }
    let err = std::io::Error::last_os_error();
    if err.raw_os_error() == Some(libc::EWOULDBLOCK) {
        LockAttempt::AlreadyRunning
    } else {
        LockAttempt::Unavailable(err)
    }
}

struct Layout {
    card: Rect,
    avatar: Rect,
    entry: Rect,
    unlock: Rect,
}

/// Wygląd ekranu blokady zależny od konfiguracji (tło, zegar, styl ikon).
struct LockLook {
    background: Option<Arc<ImageData>>,
    show_clock: bool,
    icon_theme: IconTheme,
}

fn layout(size: Size, _theme: &Theme) -> Layout {
    let w = 420.0_f64.min((size.width - 48.0).max(300.0));
    let h = 360.0_f64.min((size.height - 48.0).max(280.0));
    let card = Rect::from_xywh((size.width - w) / 2.0, (size.height - h) / 2.0, w, h);
    let avatar = Rect::from_xywh(card.center().x - 30.0, card.y() + 40.0, 60.0, 60.0);
    let entry = Rect::from_xywh(card.x() + 40.0, card.y() + 194.0, w - 80.0, 42.0);
    let unlock = Rect::from_xywh(card.x() + 40.0, entry.bottom() + 14.0, w - 80.0, 38.0);
    Layout {
        card,
        avatar,
        entry,
        unlock,
    }
}

fn paint(
    lock: &Lock,
    scene: &mut libluna_render::Scene,
    size: Size,
    theme: &Theme,
    text: &libluna_render::TextEngine,
    look: LockLook,
) {
    let lay = layout(size, theme);
    let mut ctx = PaintCtx::new(scene, theme, text);

    let dark = theme.appearance == libluna_core::style::Appearance::Dark;
    match look.background {
        Some(image) => {
            ctx.scene.background(image);
            // Tapeta musi zostać czytelna, ale nie może konkurować z kartą logowania.
            // W jasnym motywie rozjaśniamy ją zamiast zaciemniać.
            let overlay = if dark {
                libluna_core::Color::from_rgba8(0x0c, 0x0e, 0x14, 0x9c)
            } else {
                libluna_core::Color::from_rgba8(0xf4, 0xf6, 0xfa, 0xb0)
            };
            ctx.scene.fill_rect(Rect::from_size(size), overlay);
        }
        None => {
            // Brak tapety: jednolite tło dobrane do motywu (karta musi się odcinać).
            let base = if dark {
                libluna_core::Color::from_rgba8(0x0c, 0x0e, 0x14, 0xff)
            } else {
                libluna_core::Color::from_rgba8(0xdd, 0xe2, 0xea, 0xff)
            };
            ctx.scene.fill_rect(Rect::from_size(size), base);
        }
    }
    ctx.scene
        .fill_rounded_rect(lay.card, theme.radius.large, theme.colors.card_bg);
    ctx.scene
        .stroke_rounded_rect(lay.card, theme.radius.large, 1.0, theme.colors.border);

    // Awatar / ikona blokady.
    let (avatar_bg, avatar_fg) = if look.icon_theme.is_monochrome() {
        (theme.colors.shade, theme.colors.text)
    } else {
        (theme.colors.accent, theme.colors.accent_fg)
    };
    ctx.scene
        .fill_rounded_rect(lay.avatar, lay.avatar.width() / 2.0, avatar_bg);
    paint_icon_themed(
        &mut ctx,
        IconName::User,
        Rect::from_xywh(
            lay.avatar.center().x - 15.0,
            lay.avatar.center().y - 15.0,
            30.0,
            30.0,
        ),
        avatar_fg,
        look.icon_theme,
    );

    ctx.label_center(
        lay.card.center().x,
        lay.card.y() + 122.0,
        &lock.user,
        theme.typography.heading.size,
        weight::BOLD,
        theme.colors.text,
    );
    if look.show_clock {
        ctx.label_center(
            lay.card.center().x,
            lay.card.y() + 148.0,
            &clock(),
            theme.typography.body.size,
            weight::NORMAL,
            theme.colors.dim_text,
        );
    }
    ctx.label_center(
        lay.card.center().x,
        lay.card.y() + 172.0,
        &tr("Luna is locked"),
        theme.typography.caption.size,
        weight::NORMAL,
        theme.colors.dim_text,
    );

    lock.entry.paint(&mut ctx, lay.entry);
    Button::new(tr("Unlock"))
        .variant(ButtonVariant::Suggested)
        .paint(&mut ctx, lay.unlock);
    if let Some(error) = &lock.error {
        ctx.label_center(
            lay.card.center().x,
            lay.unlock.bottom() + 20.0,
            error,
            theme.typography.caption.size,
            weight::MEDIUM,
            theme.colors.error,
        );
    } else if lock.is_verifying() {
        ctx.label_center(
            lay.card.center().x,
            lay.unlock.bottom() + 20.0,
            &tr("Checking password…"),
            theme.typography.caption.size,
            weight::NORMAL,
            theme.colors.dim_text,
        );
    } else {
        ctx.label_center(
            lay.card.center().x,
            lay.unlock.bottom() + 20.0,
            &tr("Press Enter to unlock"),
            theme.typography.caption.size,
            weight::NORMAL,
            theme.colors.dim_text,
        );
    }
}

/// Tapeta taka sama jak w Selene: ustawiona przez użytkownika albo wbudowana grafika.
fn wallpaper_source() -> Option<&'static ImageData> {
    static SRC: std::sync::OnceLock<Option<ImageData>> = std::sync::OnceLock::new();
    SRC.get_or_init(|| {
        if std::env::var_os("SELENE_NO_WALLPAPER").is_some() {
            return None;
        }
        configured_path()
            .and_then(|path| load_path(&path))
            .or_else(|| decode_png(include_bytes!("../../../assets/branding/paganlinux.png")))
    })
    .as_ref()
}

/// Tapeta przeskalowana „cover” do rozmiaru powierzchni blokady.
fn wallpaper_for(size: Size) -> Option<ImageData> {
    let src = wallpaper_source()?;
    Some(cover(
        src,
        size.width.max(1.0) as u32,
        size.height.max(1.0) as u32,
    ))
}

/// Godzina w czasie lokalnym (`HH:MM`).
fn clock() -> String {
    // Bez dodatkowej zależności: `localtime_r` z libc (już linkowany przez Lunę).
    unsafe {
        let now = libc::time(std::ptr::null_mut());
        let mut tm: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&now, &mut tm).is_null() {
            return String::new();
        }
        format!("{:02}:{:02}", tm.tm_hour, tm.tm_min)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use libluna_render::{Scene, SoftwareRenderer, TextEngine};

    #[test]
    fn second_instance_is_rejected_while_first_is_alive() {
        let dir = std::env::temp_dir().join(format!("luna-lock-test-{}", std::process::id()));
        let path = dir.join("lock.flock");
        let first = try_lock_at(&path);
        assert!(matches!(&first, LockAttempt::Acquired(_)));
        assert!(matches!(try_lock_at(&path), LockAttempt::AlreadyRunning));
        drop(first);
        assert!(matches!(try_lock_at(&path), LockAttempt::Acquired(_)));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn empty_password_does_not_start_verification() {
        let mut lock = Lock::new();
        lock.attempt();
        assert!(!lock.is_verifying());
        assert!(lock.error.is_some());
    }

    #[test]
    fn pending_result_is_consumed_once() {
        let mut lock = Lock::new();
        let (tx, rx) = mpsc::channel();
        lock.pending = Some(rx);
        assert!(lock.is_verifying());
        assert_eq!(lock.take_result(), None);

        tx.send(luna::polkit::AuthOutcome::Ok).unwrap();
        assert_eq!(lock.take_result(), Some(luna::polkit::AuthOutcome::Ok));
        assert!(!lock.is_verifying());
        assert_eq!(lock.take_result(), None);
    }

    #[test]
    fn dropped_verifier_counts_as_failure() {
        let mut lock = Lock::new();
        let (tx, rx) = mpsc::channel::<luna::polkit::AuthOutcome>();
        lock.pending = Some(rx);
        drop(tx);
        assert_eq!(
            lock.take_result(),
            Some(luna::polkit::AuthOutcome::WrongPassword)
        );
        assert!(!lock.is_verifying());
    }

    #[test]
    fn card_and_controls_fit_on_screen() {
        let theme = Theme::dark();
        let lay = layout(Size::new(1920.0, 1080.0), &theme);
        assert!(lay.card.x() >= 0.0 && lay.card.right() <= 1920.0);
        assert!(lay.entry.x() >= lay.card.x() && lay.entry.right() <= lay.card.right());
        assert!(lay.unlock.bottom() <= lay.card.bottom());
    }

    #[test]
    fn small_screen_still_has_a_card() {
        let theme = Theme::dark();
        let lay = layout(Size::new(800.0, 600.0), &theme);
        assert!(lay.card.width() >= 300.0);
        assert!(lay.card.height() >= 280.0);
    }

    #[test]
    fn paints_without_window() {
        let theme = Theme::dark();
        let text = TextEngine::none();
        let lock = Lock::new();
        let mut scene = Scene::new();
        paint(
            &lock,
            &mut scene,
            Size::new(1920.0, 1080.0),
            &theme,
            &text,
            LockLook {
                background: None,
                show_clock: true,
                icon_theme: IconTheme::default(),
            },
        );
        assert!(scene.len() > 10);
    }

    #[test]
    #[ignore = "generuje podgląd do docs/images"]
    fn render_lock_preview_light() {
        let theme = Theme::light();
        let text = TextEngine::system();
        let lock = Lock::new();
        let mut scene = Scene::new();
        paint(
            &lock,
            &mut scene,
            Size::new(1920.0, 1080.0),
            &theme,
            &text,
            LockLook {
                background: None,
                show_clock: false,
                icon_theme: IconTheme::Monochrome,
            },
        );
        let mut renderer = SoftwareRenderer::new(1920, 1080);
        renderer.draw(&scene, 1.0, libluna_core::Color::BLACK);
        let png = renderer.encode_png().expect("PNG");
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../docs/images/luna-lock-light.png");
        std::fs::write(path, png).expect("zapis podglądu");
    }

    #[test]
    #[ignore = "generuje podgląd do docs/images"]
    fn render_lock_preview() {
        let theme = Theme::dark();
        let text = TextEngine::system();
        let lock = Lock::new();
        let mut scene = Scene::new();
        // Podgląd pokazuje wbudowaną tapetę (nie zależy od konfiguracji użytkownika).
        let background = decode_png(include_bytes!("../../../assets/branding/paganlinux.png"))
            .map(|src| Arc::new(cover(&src, 1920, 1080)));
        paint(
            &lock,
            &mut scene,
            Size::new(1920.0, 1080.0),
            &theme,
            &text,
            LockLook {
                background,
                show_clock: true,
                icon_theme: IconTheme::default(),
            },
        );
        let mut renderer = SoftwareRenderer::new(1920, 1080);
        renderer.draw(&scene, 1.0, libluna_core::Color::BLACK);
        let png = renderer.encode_png().expect("PNG");
        let path =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../docs/images/luna-lock.png");
        std::fs::write(path, png).expect("zapis podglądu");
    }
}
