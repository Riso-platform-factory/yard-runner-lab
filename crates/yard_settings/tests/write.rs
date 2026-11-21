use gpui::TestAppContext;
use serde_json::json;
use std::{
    cell::Cell,
    fs,
    path::{Path, PathBuf},
    rc::Rc,
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use yard_settings::{Settings, get, init, load, observe, register, watch, write};

struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |since| since.as_nanos());
        let path = std::env::temp_dir().join(format!(
            "yard-settings-write-{}-{}-{nanos:x}",
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

#[derive(serde::Deserialize, serde::Serialize, PartialEq, Debug)]
#[serde(default)]
struct First {
    greeting: String,
    size: u32,
    enabled: bool,
}

impl Default for First {
    fn default() -> Self {
        Self {
            greeting: "hello".into(),
            size: 14,
            enabled: true,
        }
    }
}

impl Settings for First {
    const KEY: &'static str = "first";
}

#[derive(serde::Deserialize, serde::Serialize, PartialEq, Debug)]
#[serde(default)]
struct Second {
    label: String,
    limit: usize,
}

impl Default for Second {
    fn default() -> Self {
        Self {
            label: "panel".into(),
            limit: 3,
        }
    }
}

impl Settings for Second {
    const KEY: &'static str = "second";
}

#[gpui::test]
fn write_changes_the_file_and_tells_the_sections_users_at_once(cx: &mut TestAppContext) {
    let temp = TempDir::new();
    let path = temp.path().join("settings.json");
    fs::write(
        &path,
        r#"{"b": 1, "first": {"size": 3, "greeting": "hi"}, "second": {"label": "custom"}, "a": 2}"#,
    )
    .unwrap();

    let first_calls = Rc::new(Cell::new(0usize));
    let second_calls = Rc::new(Cell::new(0usize));
    let _subscriptions = cx.update(|cx| {
        init(cx);
        load(cx, path.clone());
        register::<First>(cx);
        register::<Second>(cx);
        let first = first_calls.clone();
        let second = second_calls.clone();
        let subscriptions = (
            observe::<First>(cx, move |_| first.set(first.get() + 1)),
            observe::<Second>(cx, move |_| second.set(second.get() + 1)),
        );
        watch(cx);
        subscriptions
    });
    cx.read(|cx| {
        assert_eq!(
            get::<First>(cx),
            &First {
                greeting: "hi".into(),
                size: 3,
                enabled: true,
            }
        );
        assert_eq!(
            get::<Second>(cx),
            &Second {
                label: "custom".into(),
                limit: 3,
            }
        );
    });
    assert_eq!(first_calls.get(), 0);
    assert_eq!(second_calls.get(), 0);

    cx.update(|cx| write::<First>(cx, "size", json!(4)));
    cx.run_until_parked();

    // The new value is published at once, before any check of the file: the
    // watch is running but no second has passed.
    cx.read(|cx| {
        assert_eq!(get::<First>(cx).size, 4);
        assert_eq!(get::<First>(cx).greeting, "hi");
        assert_eq!(get::<Second>(cx).label, "custom");
    });
    assert_eq!(first_calls.get(), 1);
    assert_eq!(second_calls.get(), 0);

    // The whole file keeps every key in its place, with two-space indentation
    // and a final newline.
    assert_eq!(
        fs::read_to_string(&path).unwrap(),
        r#"{
  "b": 1,
  "first": {
    "size": 4,
    "greeting": "hi"
  },
  "second": {
    "label": "custom"
  },
  "a": 2
}
"#
    );

    // The next check of the file finds nothing new: nobody is told again.
    cx.executor().advance_clock(Duration::from_secs(1));
    cx.run_until_parked();
    assert_eq!(first_calls.get(), 1);
    assert_eq!(second_calls.get(), 0);
    cx.read(|cx| assert_eq!(get::<First>(cx).size, 4));
}

#[gpui::test]
fn a_missing_file_is_created_with_only_that_value(cx: &mut TestAppContext) {
    let temp = TempDir::new();
    let path = temp.path().join("settings.json");
    assert!(!path.exists());

    cx.update(|cx| {
        init(cx);
        load(cx, path.clone());
        register::<First>(cx);
        write::<First>(cx, "size", json!(15));
    });

    assert_eq!(
        fs::read_to_string(&path).unwrap(),
        r#"{
  "first": {
    "size": 15
  }
}
"#
    );
    cx.read(|cx| {
        assert_eq!(
            get::<First>(cx),
            &First {
                greeting: "hello".into(),
                size: 15,
                enabled: true,
            }
        );
    });
}

#[gpui::test]
fn a_broken_file_or_a_non_object_section_is_left_byte_for_byte(cx: &mut TestAppContext) {
    let temp = TempDir::new();
    let path = temp.path().join("settings.json");
    fs::write(&path, r#"{"first": {"greeting": "kept", "size": 5}}"#).unwrap();
    cx.update(|cx| {
        init(cx);
        load(cx, path.clone());
        register::<First>(cx);
    });
    cx.read(|cx| assert_eq!(get::<First>(cx).size, 5));

    // The file is broken after it was loaded: the write leaves it as it is.
    fs::write(&path, r#"{"first": "#).unwrap();
    cx.update(|cx| write::<First>(cx, "size", json!(9)));
    assert_eq!(fs::read(&path).unwrap(), br#"{"first": "#);
    cx.read(|cx| assert_eq!(get::<First>(cx).size, 5));

    // A section that is not a JSON object refuses the write the same way.
    let app = cx.new_app();
    let other_path = temp.path().join("other.json");
    fs::write(&other_path, r#"{"first": 42, "b": 1}"#).unwrap();
    app.update(|cx| {
        init(cx);
        load(cx, other_path.clone());
        register::<First>(cx);
        write::<First>(cx, "size", json!(9));
    });
    assert_eq!(fs::read(&other_path).unwrap(), br#"{"first": 42, "b": 1}"#);
    app.read(|cx| assert_eq!(get::<First>(cx), &First::default()));
}

#[gpui::test]
fn a_wrong_type_or_an_unknown_field_writes_nothing(cx: &mut TestAppContext) {
    let temp = TempDir::new();
    let path = temp.path().join("settings.json");
    fs::write(&path, r#"{"first": {"greeting": "hi", "size": 5}}"#).unwrap();
    let before = fs::read(&path).unwrap();
    cx.update(|cx| {
        init(cx);
        load(cx, path.clone());
        register::<First>(cx);
    });

    cx.update(|cx| write::<First>(cx, "size", json!("big")));
    assert_eq!(fs::read(&path).unwrap(), before);
    cx.read(|cx| assert_eq!(get::<First>(cx).size, 5));

    cx.update(|cx| write::<First>(cx, "nope", json!(1)));
    assert_eq!(fs::read(&path).unwrap(), before);
    cx.read(|cx| assert_eq!(get::<First>(cx).size, 5));
}

#[gpui::test]
fn the_same_value_writes_nothing(cx: &mut TestAppContext) {
    let temp = TempDir::new();
    let path = temp.path().join("settings.json");
    fs::write(&path, r#"{"first": {"greeting": "hi", "size": 3}}"#).unwrap();
    // A fixed modification time makes any rewrite visible on every filesystem.
    // Windows sets a time only through a handle open for writing.
    fs::OpenOptions::new()
        .write(true)
        .open(&path)
        .unwrap()
        .set_modified(UNIX_EPOCH + Duration::from_secs(1_700_000_000))
        .unwrap();
    let before_bytes = fs::read(&path).unwrap();
    let before_mtime = fs::metadata(&path).unwrap().modified().unwrap();

    cx.update(|cx| {
        init(cx);
        load(cx, path.clone());
        register::<First>(cx);
        write::<First>(cx, "size", json!(3));
    });

    assert_eq!(fs::read(&path).unwrap(), before_bytes);
    assert_eq!(
        fs::metadata(&path).unwrap().modified().unwrap(),
        before_mtime
    );
    cx.read(|cx| assert_eq!(get::<First>(cx).size, 3));
}

#[gpui::test]
fn an_edit_just_before_the_write_is_kept(cx: &mut TestAppContext) {
    let temp = TempDir::new();
    let path = temp.path().join("settings.json");
    fs::write(
        &path,
        r#"{"first": {"greeting": "original", "size": 1}, "tail": 9}"#,
    )
    .unwrap();
    cx.update(|cx| {
        init(cx);
        load(cx, path.clone());
        register::<First>(cx);
    });
    cx.read(|cx| assert_eq!(get::<First>(cx).greeting, "original"));

    // The file changes on disk after `load`, with no check of the file in between.
    fs::write(
        &path,
        r#"{"first": {"greeting": "edited", "size": 1}, "tail": 9, "extra": true}"#,
    )
    .unwrap();
    cx.update(|cx| write::<First>(cx, "size", json!(42)));

    // The written file holds the edit and the new value.
    assert_eq!(
        fs::read_to_string(&path).unwrap(),
        r#"{
  "first": {
    "greeting": "edited",
    "size": 42
  },
  "tail": 9,
  "extra": true
}
"#
    );
    cx.read(|cx| {
        assert_eq!(
            get::<First>(cx),
            &First {
                greeting: "edited".into(),
                size: 42,
                enabled: true,
            }
        );
    });
}
