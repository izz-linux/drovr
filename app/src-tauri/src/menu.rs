//! The native menu bar and the saved theme, as in local-wiki.
//!
//! View > Theme lists the themes; choosing one emits `menu:menu-view-theme-<id>`,
//! which the UI applies and saves through `save_theme`. The theme lives in the
//! store plugin's `session.json`.

use tauri::image::Image;
use tauri::menu::{
    AboutMetadata, CheckMenuItem, MenuBuilder, MenuItem, PredefinedMenuItem, SubmenuBuilder,
};
use tauri::{AppHandle, Emitter, Manager, Wry};
use tauri_plugin_store::StoreExt;

// The themes in src/lib/themes.ts (a test below checks they match), for View > Theme.
pub const THEMES: [(&str, &str); 6] = [
    ("system", "Match System"),
    ("light", "Light"),
    ("dark", "Dark"),
    ("darcula", "Darcula"),
    ("solarized-dark", "Solarized Dark"),
    ("nord", "Nord"),
];
const DEFAULT_THEME: &str = "system";
const STORE: &str = "session.json";

/// Shows and hides the terminal under an agent's output, as in VS Code.
const TERMINAL_SHORTCUT: &str = "Ctrl+`";

/// Lists drovr's keyboard shortcuts.
const SHORTCUTS_SHORTCUT: &str = "CmdOrCtrl+/";

/// The View > Theme items by theme id, so the current one can be checked.
pub struct ThemeMenu(Vec<(String, CheckMenuItem<Wry>)>);

impl ThemeMenu {
    fn check(&self, theme: &str) {
        for (id, item) in &self.0 {
            let _ = item.set_checked(id == theme);
        }
    }
}

fn stored_theme(app: &AppHandle) -> Option<String> {
    app.store(STORE)
        .ok()?
        .get("theme")?
        .as_str()
        .map(String::from)
}

#[tauri::command]
pub async fn load_theme(app: AppHandle) -> Result<Option<String>, String> {
    Ok(stored_theme(&app))
}

#[tauri::command]
pub async fn save_theme(app: AppHandle, theme: String) -> Result<(), String> {
    // However the theme was chosen, the View > Theme menu shows it.
    if let Some(menu) = app.try_state::<ThemeMenu>() {
        menu.check(&theme);
    }
    let store = app.store(STORE).map_err(|e| e.to_string())?;
    store.set("theme", theme);
    store.save().map_err(|e| e.to_string())
}

/// drovr > About drovr: the name, version, icon and copyright, as a standard
/// Mac About panel shows them. The copyright (with the license) is
/// `bundle.copyright` in tauri.conf.json, which also puts it in Info.plist.
/// No credits: macOS shows them as plain, left-aligned body text.
fn about<'a>(copyright: Option<String>, icon: Option<Image<'a>>) -> AboutMetadata<'a> {
    AboutMetadata {
        copyright,
        icon,
        ..Default::default()
    }
}

pub fn build(app: &tauri::App) -> tauri::Result<()> {
    let handle = app.handle();
    let current = stored_theme(handle).unwrap_or_else(|| DEFAULT_THEME.to_string());

    // Name the items ourselves: left to macOS, they use the process name,
    // which is "drovr-app" when it isn't running from the bundle.
    let name = handle
        .config()
        .product_name
        .clone()
        .unwrap_or_else(|| "drovr".into());
    let about = about(
        handle.config().bundle.copyright.clone(),
        handle.default_window_icon().cloned(),
    );
    let app_menu = SubmenuBuilder::new(handle, &name)
        .item(&PredefinedMenuItem::about(
            handle,
            Some(&format!("About {name}")),
            Some(about),
        )?)
        .separator()
        .item(&PredefinedMenuItem::hide(
            handle,
            Some(&format!("Hide {name}")),
        )?)
        .item(&PredefinedMenuItem::hide_others(handle, None)?)
        .item(&PredefinedMenuItem::show_all(handle, None)?)
        .separator()
        .item(&PredefinedMenuItem::quit(
            handle,
            Some(&format!("Quit {name}")),
        )?)
        .build()?;

    // Without an Edit menu, ⌘C / ⌘V / ⌘A don't work in macOS webviews.
    let edit_menu = SubmenuBuilder::new(handle, "Edit")
        .item(&PredefinedMenuItem::undo(handle, None)?)
        .item(&PredefinedMenuItem::redo(handle, None)?)
        .separator()
        .item(&PredefinedMenuItem::cut(handle, None)?)
        .item(&PredefinedMenuItem::copy(handle, None)?)
        .item(&PredefinedMenuItem::paste(handle, None)?)
        .item(&PredefinedMenuItem::select_all(handle, None)?)
        .separator()
        .item(&MenuItem::with_id(
            handle,
            "menu-edit-find",
            "Find…",
            true,
            Some("CmdOrCtrl+F"),
        )?)
        .build()?;

    let mut items = Vec::new();
    let mut theme_menu = SubmenuBuilder::new(handle, "Theme");
    for (i, (id, name)) in THEMES.iter().enumerate() {
        let item = CheckMenuItem::with_id(
            handle,
            format!("menu-view-theme-{id}"),
            *name,
            true,
            *id == current,
            None::<&str>,
        )?;
        theme_menu = theme_menu.item(&item);
        // "Match System", then the themes themselves.
        if i == 0 {
            theme_menu = theme_menu.separator();
        }
        items.push((id.to_string(), item));
    }
    let theme_menu = theme_menu.build()?;
    app.manage(ThemeMenu(items));
    let view_menu = SubmenuBuilder::new(handle, "View")
        .item(&MenuItem::with_id(
            handle,
            "menu-view-terminal",
            "Toggle Terminal",
            true,
            Some(TERMINAL_SHORTCUT),
        )?)
        .item(&MenuItem::with_id(
            handle,
            "menu-view-shortcuts",
            "Keyboard Shortcuts",
            true,
            Some(SHORTCUTS_SHORTCUT),
        )?)
        .separator()
        .item(&theme_menu)
        .build()?;

    let window_menu = SubmenuBuilder::new(handle, "Window")
        .item(&PredefinedMenuItem::minimize(handle, None)?)
        .item(&PredefinedMenuItem::maximize(handle, None)?)
        .separator()
        .item(&PredefinedMenuItem::close_window(handle, None)?)
        .build()?;

    let menu = MenuBuilder::new(handle)
        .item(&app_menu)
        .item(&edit_menu)
        .item(&view_menu)
        .item(&window_menu)
        .build()?;
    app.set_menu(menu)?;
    app.on_menu_event(|app, event| {
        let id = event.id().0.as_str();
        if id.starts_with("menu-") {
            let _ = app.emit(&format!("menu:{id}"), ());
        }
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::THEMES;

    #[test]
    fn about_shows_the_copyright_and_license() {
        let conf: serde_json::Value =
            serde_json::from_str(include_str!("../tauri.conf.json")).unwrap();
        let copyright = conf["bundle"]["copyright"].as_str().unwrap();
        let about = super::about(Some(copyright.to_string()), None);
        assert_eq!(about.copyright.as_deref(), Some(copyright));
        assert!(about.credits.is_none());

        // "Copyright © <year> <holder>. MIT License.", matching LICENSE.
        let (holder, license_name) = copyright
            .strip_prefix("Copyright © ")
            .and_then(|s| s.split_once(". "))
            .expect("Copyright © <year> <holder>. <license>.");
        let license = include_str!("../../../LICENSE");
        assert!(
            license.starts_with(&format!(
                "{} License",
                license_name.trim_end_matches(" License.")
            )),
            "tauri.conf.json names {license_name} but LICENSE is something else"
        );
        assert!(
            license.contains(&format!("Copyright (c) {holder}")),
            "tauri.conf.json's copyright ({copyright}) doesn't match LICENSE"
        );
    }

    #[test]
    fn shortcuts_parse() {
        // Tauri drops an accelerator it can't parse without a word.
        for s in [
            super::TERMINAL_SHORTCUT,
            super::SHORTCUTS_SHORTCUT,
            "CmdOrCtrl+F",
        ] {
            assert!(
                s.replace("CmdOrCtrl", "Ctrl")
                    .parse::<muda::accelerator::Accelerator>()
                    .is_ok(),
                "{s}"
            );
        }
    }

    #[test]
    fn themes_match_the_ui() {
        let ts = include_str!("../../src/lib/themes.ts");
        let ui: Vec<&str> = ts
            .lines()
            .filter_map(|l| l.trim().strip_prefix("{ id: \""))
            .filter_map(|l| l.split('"').next())
            .collect();
        let menu: Vec<&str> = THEMES.iter().map(|(id, _)| *id).collect();
        // themes.ts lists "system" in THEME_SETTINGS after the themes.
        let mut ui_sorted = ui.clone();
        ui_sorted.sort();
        let mut menu_sorted = menu.clone();
        menu_sorted.sort();
        assert_eq!(
            ui_sorted, menu_sorted,
            "src/lib/themes.ts and the View > Theme menu disagree"
        );
    }
}
