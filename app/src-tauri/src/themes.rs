//! The themes, from `src/lib/themes.json`: the one list the UI and the
//! View > Theme menu are both built from.

use serde::Deserialize;
use std::collections::BTreeMap;

// The menu reads only id and name; the rest is for tests, which check the file
// the UI is built from.
#[allow(dead_code)]
pub const COLOR_KEYS: [&str; 13] = [
    "bg",
    "panel",
    "panel-2",
    "border",
    "text",
    "muted",
    "accent",
    "on-accent",
    "green",
    "yellow",
    "red",
    "purple",
    "cyan",
];

#[derive(Deserialize)]
#[allow(dead_code)]
pub struct Lettering {
    pub color: String,
    pub foil: String,
    pub font: String,
}

#[derive(Deserialize)]
#[allow(dead_code)]
pub struct Theme {
    pub id: String,
    pub name: String,
    pub scheme: String,
    pub colors: BTreeMap<String, String>,
    pub lettering: Option<Lettering>,
}

pub fn builtin() -> Vec<Theme> {
    serde_json::from_str(include_str!("../../src/lib/themes.json"))
        .expect("src/lib/themes.json is valid")
}

/// View > Theme's items as `(id, name)`: "Match System", then the themes.
pub fn menu_entries() -> Vec<(String, String)> {
    std::iter::once(("system".to_string(), "Match System".to_string()))
        .chain(builtin().into_iter().map(|t| (t.id, t.name)))
        .collect()
}

#[cfg(test)]
fn contrast(a: &str, b: &str) -> f64 {
    fn luminance(hex: &str) -> f64 {
        let channel = |i: usize| {
            let c = f64::from(u8::from_str_radix(&hex[i..i + 2], 16).unwrap()) / 255.0;
            if c <= 0.03928 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * channel(1) + 0.7152 * channel(3) + 0.0722 * channel(5)
    }
    let (la, lb) = (luminance(a), luminance(b));
    let (hi, lo) = if la >= lb { (la, lb) } else { (lb, la) };
    (hi + 0.05) / (lo + 0.05)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_unique_slugs() {
        let themes = builtin();
        let mut seen = std::collections::HashSet::new();
        for t in &themes {
            assert!(
                !t.id.is_empty()
                    && t.id
                        .chars()
                        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'),
                "{} isn't a slug",
                t.id
            );
            assert_ne!(t.id, "system");
            assert!(seen.insert(t.id.clone()), "{} is listed twice", t.id);
        }
    }

    #[test]
    fn every_theme_has_every_color() {
        for t in builtin() {
            let keys: Vec<&str> = t.colors.keys().map(String::as_str).collect();
            let mut want = COLOR_KEYS.to_vec();
            want.sort_unstable();
            assert_eq!(keys, want, "{}", t.id);
            for (k, v) in &t.colors {
                assert!(
                    (v.starts_with('#') && v.len() == 7) || v.starts_with("rgba("),
                    "{}: {k} is {v}",
                    t.id
                );
            }
        }
    }

    #[test]
    fn scheme_is_light_or_dark() {
        for t in builtin() {
            assert!(
                t.scheme == "light" || t.scheme == "dark",
                "{}: {}",
                t.id,
                t.scheme
            );
        }
    }

    #[test]
    fn text_is_readable() {
        for t in builtin() {
            let ratio = contrast(&t.colors["text"], &t.colors["bg"]);
            assert!(ratio >= 4.5, "{}: text on bg is {ratio:.2}", t.id);
        }
    }

    #[test]
    fn contrast_matches_known_values() {
        assert!((contrast("#000000", "#ffffff") - 21.0).abs() < 0.01);
        assert!((contrast("#777777", "#ffffff") - 4.48).abs() < 0.05);
    }

    #[test]
    fn existing_themes_keep_their_order() {
        let ids: Vec<String> = menu_entries().into_iter().map(|(id, _)| id).collect();
        assert_eq!(
            ids[..6],
            [
                "system",
                "light",
                "dark",
                "darcula",
                "solarized-dark",
                "nord"
            ]
        );
        assert_eq!(ids[ids.len() - 4..], ["gilded", "deco", "oxblood", "ink"]);
        assert_eq!(ids.len(), 10);
    }

    #[test]
    fn lettering_fonts_are_bundled() {
        let css = include_str!("../../src/app.css");
        let fonts: Vec<String> =
            std::fs::read_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/../static/fonts"))
                .expect("app/static/fonts exists")
                .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .collect();
        for t in builtin() {
            let Some(l) = t.lettering else { continue };
            let face = format!("font-family: \"{}\"", l.font);
            assert!(
                css.contains(&face),
                "{}: no @font-face for {}",
                t.id,
                l.font
            );
            let stem = l.font.to_lowercase().replace(' ', "-");
            assert!(
                fonts
                    .iter()
                    .any(|f| f.starts_with(&stem) && f.ends_with(".woff2")),
                "{}: no .woff2 for {}",
                t.id,
                l.font
            );
        }
    }

    #[test]
    fn existing_themes_have_no_lettering() {
        for t in builtin() {
            let gold = ["gilded", "deco", "oxblood", "ink"].contains(&t.id.as_str());
            assert_eq!(t.lettering.is_some(), gold, "{}", t.id);
        }
    }
}
