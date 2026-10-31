//! OSD — ekranowe wskaźniki (głośność, jasność, …) pojawiające się na chwilę.
//!
//! Model jest czysty (bez okna), więc łatwo go testować i podglądać headless.

use libluna_core::style::weight;
use libluna_core::Rect;
use libluna_widgets::{paint_icon, IconName, PaintCtx};

/// Czas wchodzenia (s).
const FADE_IN: f64 = 0.18;
/// Czas wychodzenia (s).
const FADE_OUT: f64 = 0.45;

/// Wskaźnik OSD.
#[derive(Debug, Clone)]
pub struct Osd {
    /// Ikona z pakietu Luny.
    pub icon: IconName,
    /// Etykieta (np. „Głośność”).
    pub label: String,
    /// Wartość 0..1 (poziom).
    pub value: f64,
    age: f64,
    lifetime: f64,
}

impl Osd {
    /// Tworzy wskaźnik **ukryty** — pokaże się dopiero po wywołaniu [`Osd::show`].
    ///
    /// Wcześniej OSD było widoczne od razu po starcie, więc powłoka witała użytkownika
    /// dwusekundowym toastem „Głośność 0%” (i przez ten czas nie mogła odpocząć,
    /// bo animacja bez przerwy prosiła o kolejne klatki).
    pub fn new(icon: IconName, label: impl Into<String>, value: f64) -> Self {
        Self {
            icon,
            label: label.into(),
            value: value.clamp(0.0, 1.0),
            age: 2.0,
            lifetime: 2.0,
        }
    }

    /// Pokazuje wskaźnik ponownie z nową wartością (reset licznika).
    pub fn show(&mut self, label: impl Into<String>, value: f64) {
        self.label = label.into();
        self.value = value.clamp(0.0, 1.0);
        self.age = 0.0;
    }

    /// Przesuwa czas animacji.
    pub fn advance(&mut self, dt: f64) {
        self.age += dt;
    }

    /// Czy wskaźnik jest jeszcze widoczny.
    pub fn visible(&self) -> bool {
        self.age < self.lifetime
    }

    /// Przezroczystość (0..1) wynikająca z animacji wejścia/wyjścia.
    pub fn opacity(&self) -> f64 {
        let fade_in = (self.age / FADE_IN).clamp(0.0, 1.0);
        let fade_out = ((self.lifetime - self.age) / FADE_OUT).clamp(0.0, 1.0);
        fade_in.min(fade_out)
    }

    /// Rysuje wskaźnik w zadanym prostokącie.
    pub fn paint(&self, ctx: &mut PaintCtx, rect: Rect) {
        let a = self.opacity() as f32;
        if a <= 0.01 {
            return;
        }
        let t = ctx.theme;
        let cy = rect.center().y;
        let pad = t.spacing.lg;

        // Tło „pigułki” + obramowanie.
        ctx.scene
            .fill_rounded_rect(rect, t.radius.medium, t.colors.card_bg.scale_alpha(a));
        ctx.scene
            .stroke_rounded_rect(rect, t.radius.medium, 1.0, t.colors.border.scale_alpha(a));

        // Ikona w kwadracie akcentu.
        let icon = Rect::from_xywh(rect.x() + pad, cy - 20.0, 40.0, 40.0);
        ctx.scene
            .fill_rounded_rect(icon, t.radius.small, t.colors.accent.scale_alpha(0.18 * a));
        let icon_inner = Rect::from_xywh(icon.x() + 10.0, icon.y() + 10.0, 20.0, 20.0);
        paint_icon(ctx, self.icon, icon_inner, t.colors.accent.scale_alpha(a));

        // Etykieta i wartość procentowa.
        let text_x = icon.right() + t.spacing.md;
        ctx.label_left(
            text_x,
            rect.y() + 24.0,
            &self.label,
            t.typography.body.size,
            weight::MEDIUM,
            t.colors.text.scale_alpha(a),
        );
        ctx.label_right(
            rect.right() - pad,
            rect.y() + 24.0,
            &format!("{}%", (self.value * 100.0).round() as i32),
            t.typography.body.size,
            weight::NORMAL,
            t.colors.dim_text.scale_alpha(a),
        );

        // Pasek poziomu.
        let bar = Rect::from_xywh(
            text_x,
            rect.bottom() - 22.0,
            (rect.right() - pad) - text_x,
            6.0,
        );
        ctx.scene
            .fill_rounded_rect(bar, 3.0, t.colors.shade.scale_alpha(a));
        if self.value > 0.0 {
            let fill = Rect::from_xywh(bar.x(), bar.y(), bar.width() * self.value, bar.height());
            ctx.scene
                .fill_rounded_rect(fill, 3.0, t.colors.accent.scale_alpha(a));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use libluna_core::Theme;
    use libluna_render::{Scene, TextEngine};

    #[test]
    fn fades_in_and_out() {
        let mut osd = Osd::new(IconName::Volume, "Głośność", 0.5);
        // Nowy wskaźnik jest ukryty — dopiero `show` go wyświetli.
        assert!(!osd.visible());
        osd.show("Głośność", 0.5);
        assert!(osd.visible());
        assert!(osd.opacity() < 0.05); // dopiero start
        osd.advance(0.3);
        assert!(osd.opacity() > 0.9);
        osd.advance(2.0);
        assert!(!osd.visible());
        assert!(osd.opacity() < 0.05);
    }

    #[test]
    fn paints_without_window() {
        let theme = Theme::dark();
        let text = TextEngine::none();
        let mut scene = Scene::new();
        let mut ctx = PaintCtx::new(&mut scene, &theme, &text);
        let mut osd = Osd::new(IconName::Volume, "Głośność", 0.65);
        osd.show("Głośność", 0.65);
        osd.advance(0.3);
        osd.paint(&mut ctx, Rect::from_xywh(0.0, 0.0, 320.0, 72.0));
        assert!(scene.len() > 5);
    }
}
