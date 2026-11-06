//! Resolved appearance for native Markdown rendering.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use franken_markdown::{FontAssetSlot, FontAssets};

/// User-configurable Markdown appearance. Call `resolve` before rendering.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RenderedTheme {
    pub dark: bool,
    pub background: String,
    pub text: String,
    pub heading: String,
    pub link: String,
    pub code_background: String,
    pub code_text: String,
    pub quote: String,
    pub quote_bar: String,
    pub table_border: String,
    pub table_stripe: String,
    pub body_font: Option<PathBuf>,
    pub heading_font: Option<PathBuf>,
    pub code_font: Option<PathBuf>,
    pub body_size_px: u16,
    pub code_size_px: u16,
    pub heading_scale_percent: u16,
    pub line_height_percent: u16,
    pub page_padding_px: u16,
    pub(crate) fonts: Arc<FontAssets>,
    pub(crate) heading_bytes: Option<Arc<[u8]>>,
}

impl RenderedTheme {
    pub fn defaults(dark: bool) -> Self {
        let colors = if dark {
            franken_markdown::theme::ThemeColors::dark()
        } else {
            franken_markdown::theme::ThemeColors::light()
        };
        Self {
            dark,
            background: colors.bg,
            heading: colors.fg.clone(),
            code_text: colors.fg.clone(),
            text: colors.fg,
            link: colors.accent,
            code_background: colors.code_bg,
            quote: colors.quote_fg,
            quote_bar: colors.quote_bar,
            table_border: colors.border,
            table_stripe: colors.stripe,
            body_font: None,
            heading_font: None,
            code_font: None,
            body_size_px: 16,
            code_size_px: 16,
            heading_scale_percent: 100,
            line_height_percent: 140,
            page_padding_px: 12,
            fonts: Arc::new(FontAssets::default()),
            heading_bytes: None,
        }
    }

    /// Validate settings and load local font files once, relative to the config directory.
    pub fn resolve(mut self, config_dir: &Path) -> Result<Arc<Self>, String> {
        for (name, value) in [
            ("background", &self.background),
            ("text", &self.text),
            ("heading", &self.heading),
            ("link", &self.link),
            ("code_background", &self.code_background),
            ("code_text", &self.code_text),
            ("quote", &self.quote),
            ("quote_bar", &self.quote_bar),
            ("table_border", &self.table_border),
            ("table_stripe", &self.table_stripe),
        ] {
            if value.len() != 7
                || !value.starts_with('#')
                || !value.as_bytes()[1..].iter().all(u8::is_ascii_hexdigit)
            {
                return Err(format!("rendered.{name} must be a #RRGGBB color"));
            }
        }
        for (name, value, min, max) in [
            ("body_size_px", self.body_size_px, 9, 32),
            ("code_size_px", self.code_size_px, 9, 32),
            ("heading_scale_percent", self.heading_scale_percent, 50, 200),
            ("line_height_percent", self.line_height_percent, 100, 400),
            ("page_padding_px", self.page_padding_px, 0, 128),
        ] {
            if !(min..=max).contains(&value) {
                return Err(format!("rendered.{name} must be between {min} and {max}"));
            }
        }
        self.heading_bytes = None;
        let mut fonts = FontAssets::default();
        for (name, path, slot) in [
            ("body_font", &self.body_font, FontAssetSlot::BodyRegular),
            (
                "heading_font",
                &self.heading_font,
                FontAssetSlot::BodyRegular,
            ),
            ("code_font", &self.code_font, FontAssetSlot::MonoRegular),
        ] {
            if let Some(path) = path {
                let full = config_dir.join(path);
                let bytes = std::fs::read(&full)
                    .map_err(|e| format!("Cannot read rendered.{name} {}: {e}", full.display()))?;
                if name == "heading_font" {
                    // Validate with the same deterministic parser as the body face.
                    let mut validation = FontAssets::default();
                    validation
                        .set_slot(slot, bytes.clone())
                        .map_err(|e| format!("Invalid rendered.{name} {}: {e}", full.display()))?;
                    self.heading_bytes = Some(Arc::from(bytes));
                } else {
                    fonts
                        .set_slot(slot, bytes)
                        .map_err(|e| format!("Invalid rendered.{name} {}: {e}", full.display()))?;
                }
            }
        }
        self.fonts = Arc::new(fonts);
        Ok(Arc::new(self))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_invalid_colors_sizes_and_fonts() {
        let mut theme = RenderedTheme::defaults(false);
        theme.background = "red".into();
        assert!(theme.resolve(Path::new(".")).is_err());
        let mut theme = RenderedTheme::defaults(false);
        theme.code_size_px = 0;
        assert!(theme.resolve(Path::new(".")).is_err());
        let mut theme = RenderedTheme::defaults(false);
        theme.body_font = Some("missing-font.ttf".into());
        assert!(
            theme
                .resolve(Path::new("."))
                .unwrap_err()
                .contains("body_font")
        );
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("bad.ttf"), b"not a font").unwrap();
        let mut theme = RenderedTheme::defaults(false);
        theme.heading_font = Some("bad.ttf".into());
        assert!(
            theme
                .resolve(dir.path())
                .unwrap_err()
                .contains("heading_font")
        );
    }
}
