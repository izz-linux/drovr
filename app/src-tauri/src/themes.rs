//! The themes, from `src/lib/themes.json`: the one list the UI and the
//! View > Theme menu are both built from.

use serde::Deserialize;
use std::collections::BTreeMap;

// The menu reads only id and name; the rest is for tests, which check the file
// the UI is built from.
#[allow(dead_code)]
pub const COLOR_KEYS: [&str; 17] = [
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
    "chart-1",
    "chart-2",
    "chart-3",
    "chart-4",
];

#[derive(Deserialize)]
#[allow(dead_code)]
pub struct Lettering {
    pub color: String,
    pub foil: String,
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
fn linear(hex: &str) -> [f64; 3] {
    [1, 3, 5].map(|i| {
        let c = f64::from(u8::from_str_radix(&hex[i..i + 2], 16).unwrap()) / 255.0;
        if c <= 0.03928 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    })
}

#[cfg(test)]
fn contrast(a: &str, b: &str) -> f64 {
    fn luminance(hex: &str) -> f64 {
        let [r, g, b] = linear(hex);
        0.2126 * r + 0.7152 * g + 0.0722 * b
    }
    let (la, lb) = (luminance(a), luminance(b));
    let (hi, lo) = if la >= lb { (la, lb) } else { (lb, la) };
    (hi + 0.05) / (lo + 0.05)
}

/// How far apart two colors look: distance in OKLab, times 100. `sight` is
/// how the eye mixes linear RGB; see `SIGHTS`.
#[cfg(test)]
fn difference(a: &str, b: &str, sight: &[[f64; 3]; 3]) -> f64 {
    let mix = |m: &[[f64; 3]; 3], v: [f64; 3]| m.map(|r| r[0] * v[0] + r[1] * v[1] + r[2] * v[2]);
    let lab = |hex: &str| {
        let seen = mix(sight, linear(hex)).map(|c| c.clamp(0.0, 1.0));
        let lms = [
            [0.4122214708, 0.5363325363, 0.0514459929],
            [0.2119034982, 0.6806995451, 0.1073969566],
            [0.0883024619, 0.2817188376, 0.6299787005],
        ];
        let oklab = [
            [0.2104542553, 0.7936177850, -0.0040720468],
            [1.9779984951, -2.4285922050, 0.4505937099],
            [0.0259040371, 0.7827717662, -0.8086757660],
        ];
        mix(&oklab, mix(&lms, seen).map(f64::cbrt))
    };
    let (a, b) = (lab(a), lab(b));
    100.0 * (0..3).map(|i| (a[i] - b[i]).powi(2)).sum::<f64>().sqrt()
}

/// Full color vision, then protanopia and deuteranopia (Machado et al. 2009),
/// each with the least difference two chart colors side by side may have.
#[cfg(test)]
const SIGHTS: [(&str, f64, [[f64; 3]; 3]); 3] = [
    (
        "full color",
        15.0,
        [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
    ),
    (
        "protanopia",
        8.0,
        [
            [0.152286, 1.052583, -0.204868],
            [0.114503, 0.786281, 0.099216],
            [-0.003882, -0.048116, 1.051998],
        ],
    ),
    (
        "deuteranopia",
        8.0,
        [
            [0.367322, 0.860646, -0.227968],
            [0.280085, 0.672501, 0.047413],
            [-0.011820, 0.042940, 0.968881],
        ],
    ),
];

/// What's wrong with a theme's chart colors.
#[cfg(test)]
fn chart_problems(t: &Theme) -> Vec<String> {
    let mut out = vec![];
    let chart: Vec<&str> = (1..=4)
        .filter_map(|n| t.colors.get(&format!("chart-{n}")))
        .map(String::as_str)
        .filter(|c| c.len() == 7 && c.starts_with('#'))
        .collect();
    if chart.len() != 4 {
        return vec![format!("{}: needs chart-1 to chart-4, as #rrggbb", t.id)];
    }
    for pair in chart.windows(2) {
        for (sight, least, matrix) in &SIGHTS {
            let d = difference(pair[0], pair[1], matrix);
            if d < *least {
                out.push(format!("{}: {pair:?} differ by {d:.1} in {sight}", t.id));
            }
        }
    }
    for c in chart {
        for on in ["bg", "panel"] {
            let ratio = contrast(c, &t.colors[on]);
            if ratio < 2.0 {
                out.push(format!("{}: {c} on {on} is {ratio:.2}", t.id));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chart_colors_can_be_told_apart() {
        let problems: Vec<String> = builtin().iter().flat_map(chart_problems).collect();
        assert!(problems.is_empty(), "{problems:#?}");
    }

    #[test]
    fn rejects_bad_chart_colors() {
        let good = ["#3987e5", "#d95926", "#199e70", "#c98500"];
        for (case, n, color) in [
            ("missing", 2, None),
            ("not hex", 2, Some("rgba(1, 2, 3, 0.5)")),
            ("like its neighbor", 2, Some("#3a88e6")),
            ("like its neighbor to a deuteranope", 3, Some("#b3691f")),
            ("invisible on the panel", 4, Some("#1a1a1a")),
        ] {
            let mut t = builtin().into_iter().find(|t| t.id == "dark").unwrap();
            for (i, c) in good.iter().enumerate() {
                t.colors.insert(format!("chart-{}", i + 1), c.to_string());
            }
            assert!(chart_problems(&t).is_empty(), "{:?}", chart_problems(&t));
            let key = format!("chart-{n}");
            match color {
                Some(c) => t.colors.insert(key, c.into()),
                None => t.colors.remove(&key),
            };
            assert!(!chart_problems(&t).is_empty(), "{case}");
        }
    }

    #[test]
    fn difference_matches_known_values() {
        let [(_, _, full), _, _] = SIGHTS;
        assert!((difference("#000000", "#ffffff", &full) - 100.0).abs() < 0.1);
        assert!(difference("#3987e5", "#3987e5", &full) < 1e-9);
    }

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
        let builtin_ids: Vec<String> = builtin().iter().map(|t| t.id.clone()).collect();
        assert_eq!(ids[1..], builtin_ids[..]);
    }

    #[test]
    fn existing_themes_have_no_lettering() {
        for t in builtin() {
            let gold = ["gilded", "deco", "oxblood", "ink"].contains(&t.id.as_str());
            assert_eq!(t.lettering.is_some(), gold, "{}", t.id);
        }
    }
}
