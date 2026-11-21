use gpui::TestAppContext;
use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use yard_ui::{
    ThemeAppearance, active_theme, apply_theme_setting, choose_theme, load_user_themes, theme_list,
};

struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |since| since.as_nanos());
        let path = std::env::temp_dir().join(format!(
            "yard-ui-theme-list-{}-{}-{nanos:x}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed),
        ));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

// A family of a dark and a light theme whose names sort between the bundled
// One Dark and One Light, as an extension ships them.
const BETWEEN: &str = r##"{
    "name": "Between",
    "author": "yard's tests",
    "themes": [
        {"name": "One Evening", "appearance": "dark", "style": {"background": "#223344ff"}},
        {"name": "One Late", "appearance": "light", "style": {"background": "#eeddccff"}}
    ]
}"##;

fn theme_dir(temp: &TempDir, files: &[(&str, &str)]) -> PathBuf {
    let dir = temp.path().join("themes");
    fs::create_dir_all(&dir).unwrap();
    for (name, content) in files {
        fs::write(dir.join(name), content).unwrap();
    }
    dir
}

// The store, the file, the UI, then the theme setting, as the application's
// startup does it.
fn startup(cx: &mut gpui::App, settings: &Path, content: &str, appearance: theme::Appearance) {
    fs::write(settings, content).unwrap();
    yard_settings::init(cx);
    yard_settings::load(cx, settings.to_path_buf());
    yard_ui::init(cx);
    theme::SystemAppearance::global_mut(cx).0 = appearance;
    apply_theme_setting(cx);
}

// The list as (name, appearance) pairs, in the order it is read.
fn listed(cx: &gpui::App) -> Vec<(String, &'static str)> {
    theme_list(cx)
        .into_iter()
        .map(|entry| {
            let appearance = match entry.appearance {
                ThemeAppearance::Light => "light",
                ThemeAppearance::Dark => "dark",
            };
            (entry.name.to_string(), appearance)
        })
        .collect()
}

#[gpui::test]
fn the_list_is_dark_themes_first_then_light_ones_each_group_by_name(cx: &mut TestAppContext) {
    let temp = TempDir::new();
    let settings = temp.path().join("settings.json");
    cx.update(|cx| {
        startup(
            cx,
            &settings,
            r#"{"theme": {"mode": "dark"}}"#,
            theme::Appearance::Light,
        )
    });

    // The bundled themes alone: One Dark, then One Light.
    cx.read(|cx| {
        assert_eq!(
            listed(cx),
            [
                ("One Dark".to_owned(), "dark"),
                ("One Light".to_owned(), "light"),
            ]
        )
    });

    // With the user's themes loaded, each one is in its place by name.
    let dir = theme_dir(&temp, &[("between.json", BETWEEN)]);
    cx.update(|cx| assert_eq!(load_user_themes(cx, &dir), 2));
    cx.read(|cx| {
        assert_eq!(
            listed(cx),
            [
                ("One Dark".to_owned(), "dark"),
                ("One Evening".to_owned(), "dark"),
                ("One Late".to_owned(), "light"),
                ("One Light".to_owned(), "light"),
            ]
        )
    });
}

#[gpui::test]
fn choosing_a_light_theme_with_mode_dark_writes_light_and_mode(cx: &mut TestAppContext) {
    let temp = TempDir::new();
    let settings = temp.path().join("settings.json");
    cx.update(|cx| {
        startup(
            cx,
            &settings,
            r#"{"b": 1, "theme": {"mode": "dark", "note": "kept"}, "a": 2}"#,
            theme::Appearance::Light,
        )
    });
    cx.read(|cx| assert_eq!(active_theme(cx), "One Dark"));

    cx.update(|cx| choose_theme(cx, "One Light"));

    // The light theme's name goes into `light`, `mode` becomes `light`, and
    // every other key is kept. One Light is active at once.
    cx.read(|cx| assert_eq!(active_theme(cx), "One Light"));
    assert_eq!(
        fs::read_to_string(&settings).unwrap(),
        r#"{
  "b": 1,
  "theme": {
    "mode": "light",
    "note": "kept",
    "light": "One Light"
  },
  "a": 2
}
"#
    );
}

#[gpui::test]
fn choosing_a_theme_of_the_systems_appearance_with_mode_system_keeps_system(
    cx: &mut TestAppContext,
) {
    let temp = TempDir::new();
    let settings = temp.path().join("settings.json");
    let dir = theme_dir(&temp, &[("between.json", BETWEEN)]);
    cx.update(|cx| {
        startup(
            cx,
            &settings,
            r#"{"theme": {"mode": "system"}}"#,
            theme::Appearance::Dark,
        );
        assert_eq!(load_user_themes(cx, &dir), 2);
    });
    cx.read(|cx| assert_eq!(active_theme(cx), "One Dark"));

    // The dark theme goes into `dark`; `mode` stays `system` and the system
    // keeps choosing, so the new theme shows because it is the one for the
    // system's appearance.
    cx.update(|cx| choose_theme(cx, "One Evening"));
    cx.read(|cx| assert_eq!(active_theme(cx), "One Evening"));
    assert_eq!(
        fs::read_to_string(&settings).unwrap(),
        r#"{
  "theme": {
    "mode": "system",
    "dark": "One Evening"
  }
}
"#
    );

    // The light theme goes into `light`, and `mode` becomes `light`.
    cx.update(|cx| choose_theme(cx, "One Late"));
    cx.read(|cx| assert_eq!(active_theme(cx), "One Late"));
    assert_eq!(
        fs::read_to_string(&settings).unwrap(),
        r#"{
  "theme": {
    "mode": "light",
    "dark": "One Evening",
    "light": "One Late"
  }
}
"#
    );
}

#[gpui::test]
fn choosing_a_name_that_is_not_registered_writes_nothing(cx: &mut TestAppContext) {
    let temp = TempDir::new();
    let settings = temp.path().join("settings.json");
    cx.update(|cx| {
        startup(
            cx,
            &settings,
            r#"{"theme": {"mode": "dark"}}"#,
            theme::Appearance::Light,
        )
    });
    cx.read(|cx| assert_eq!(active_theme(cx), "One Dark"));

    // A fixed modification time makes any rewrite of the file visible.
    // Windows sets a time only through a handle open for writing.
    fs::OpenOptions::new()
        .write(true)
        .open(&settings)
        .unwrap()
        .set_modified(UNIX_EPOCH + Duration::from_secs(1_700_000_000))
        .unwrap();
    let before = fs::read(&settings).unwrap();
    let before_mtime = fs::metadata(&settings).unwrap().modified().unwrap();

    cx.update(|cx| choose_theme(cx, "Solarized"));

    assert_eq!(fs::read(&settings).unwrap(), before);
    assert_eq!(
        fs::metadata(&settings).unwrap().modified().unwrap(),
        before_mtime
    );
    cx.read(|cx| assert_eq!(active_theme(cx), "One Dark"));
}
