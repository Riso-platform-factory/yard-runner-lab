use gpui::{AppContext, IntoElement, Styled, TestAppContext};
use std::{
    collections::BTreeMap,
    fs,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};

struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |since| since.as_nanos());
        let path = std::env::temp_dir().join(format!(
            "yard-shell-title-bar-log-{}-{}-{nanos:x}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed),
        ));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[derive(Default)]
struct Captured {
    spans: BTreeMap<u64, String>,
    entered: Vec<u64>,
    lines: Vec<String>,
    span_stacks: Vec<Vec<String>>,
}

#[derive(Clone, Default)]
struct Trace(Arc<Mutex<Captured>>);

#[derive(Default)]
struct Fields(String);
impl tracing::field::Visit for Fields {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        use std::fmt::Write;
        let rendered = format!("{value:?}");
        let rendered = match field.name() {
            "path" | "file" => rendered.rsplit(['/', '\\']).next().unwrap().to_owned(),
            _ => rendered,
        };
        write!(self.0, " {}={rendered}", field.name()).unwrap();
    }
}

impl tracing::Subscriber for Trace {
    fn register_callsite(
        &self,
        _: &'static tracing::Metadata<'static>,
    ) -> tracing::subscriber::Interest {
        tracing::subscriber::Interest::sometimes()
    }
    fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
        true
    }
    fn new_span(&self, attributes: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        let mut captured = self.0.lock().unwrap();
        let id = captured.spans.len() as u64 + 1;
        let mut fields = Fields::default();
        attributes.record(&mut fields);
        let label = format!("{}{}", attributes.metadata().name(), fields.0);
        let mut stack: Vec<String> = captured
            .entered
            .iter()
            .map(|entered| captured.spans[entered].clone())
            .collect();
        stack.push(label.clone());
        captured.span_stacks.push(stack);
        captured.spans.insert(id, label);
        tracing::span::Id::from_u64(id)
    }
    fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
    fn event(&self, event: &tracing::Event<'_>) {
        let mut fields = Fields::default();
        event.record(&mut fields);
        let mut captured = self.0.lock().unwrap();
        let group = captured
            .entered
            .last()
            .map(|id| captured.spans[id].as_str())
            .unwrap_or("none");
        let line = format!("{group}: {}{}", event.metadata().level(), fields.0);
        captured.lines.push(line);
    }
    fn enter(&self, id: &tracing::span::Id) {
        self.0.lock().unwrap().entered.push(id.into_u64());
    }
    fn exit(&self, id: &tracing::span::Id) {
        assert_eq!(self.0.lock().unwrap().entered.pop(), Some(id.into_u64()));
    }
}

struct Diagnostics;
impl log::Log for Diagnostics {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        metadata.target().starts_with("yard")
    }
    fn log(&self, record: &log::Record<'_>) {
        if self.enabled(record.metadata()) {
            assert_eq!(record.level(), log::Level::Error);
            tracing::error!(target: "title_bar_diagnostic", "{}", record.args());
        }
    }
    fn flush(&self) {}
}

struct Card;
impl gpui::Render for Card {
    fn render(&mut self, _: &mut gpui::Window, _: &mut gpui::Context<Self>) -> impl IntoElement {
        gpui::div().size_full()
    }
}

fn draw(window: gpui::WindowHandle<yard_shell::Shell>, cx: &mut TestAppContext) {
    gpui::AnyWindowHandle::from(window)
        .update(cx, |_, window, cx| {
            window.refresh();
            let _ = window.draw(cx);
        })
        .unwrap();
}

/// The lines captured while `run` executes.
fn lines_of(trace: &Trace, run: impl FnOnce()) -> Vec<String> {
    let before = trace.0.lock().unwrap().lines.len();
    run();
    trace.0.lock().unwrap().lines[before..].to_vec()
}

/// A trace line about the title bar's draw: the bar's own line, or any other
/// crate's line about the bar.
fn is_bar_draw(line: &str) -> bool {
    if !line.contains(": TRACE") {
        return false;
    }
    line.contains("title bar")
        || ["left", "center", "right", "title"]
            .iter()
            .all(|field| line.contains(format!(" {field}=").as_str()))
}

/// The `(left, center, right)` shown counts and the title flag of the one
/// trace line a draw may log about the title bar, over every crate's lines.
fn drawn_bar(lines: &[String]) -> (usize, usize, usize, bool) {
    let about: Vec<&String> = lines
        .iter()
        .filter(|line| is_bar_draw(line.as_str()))
        .collect();
    assert_eq!(
        about.len(),
        1,
        "a draw must log exactly one trace line about the title bar, in {lines:#?}"
    );
    let line = about[0];
    assert!(
        ["left", "center", "right", "title"]
            .iter()
            .all(|field| line.contains(format!(" {field}=").as_str())),
        "the bar's draw line holds the shown counts and the title flag: {line}"
    );
    let value = |field: &str| {
        line.split(format!(" {field}=").as_str())
            .nth(1)
            .and_then(|rest| rest.split_whitespace().next())
            .unwrap()
    };
    (
        value("left").parse().unwrap(),
        value("center").parse().unwrap(),
        value("right").parse().unwrap(),
        value("title").parse().unwrap(),
    )
}

// Alone in this binary: every captured callsite runs under one scoped subscriber.
#[gpui::test]
fn the_title_bar_logs_its_frame_title_refusals_and_draws(cx: &mut TestAppContext) {
    log::set_logger(&Diagnostics).unwrap();
    log::set_max_level(log::LevelFilter::Trace);
    let temp = TempDir::new();
    let layout_path = temp.0.join("layout.json");
    fs::write(
        &layout_path,
        r#"{
  "version": 1,
  "operations": [
    {"op": "move", "node": "a", "parent": "body", "slot": "children", "index": 0},
    {"op": "hide", "node": "title_bar"},
    {"op": "remove", "node": "window"}
  ]
}"#,
    )
    .unwrap();
    let trace = Trace::default();
    tracing::subscriber::with_default(trace.clone(), || {
        let mut app = cx.new_app();
        app.update(yard_ui::init);
        let ext = app.update(|cx| {
            yard_shell::set_layout_file(cx, layout_path);
            let ext = yard_shell::Extension::new(cx, "tour").unwrap();
            ext.register(cx, "tour.card", &[], |_, _, _, cx| cx.new(|_| Card).into());
            yard_shell::set_window_title(cx, "Test Title");
            ext.place_root(cx, "root", "yard.row");
            ext.place(cx, "a", "tour.card", "root", "children");
            ext.place(cx, "item", "tour.card", "title_bar", "left");
            ext.place(cx, "p_body_slot", "tour.card", "body", "children");
            ext.place(cx, "p_window_slot", "tour.card", "window", "title_bar");
            ext.place(cx, "p_frame_comp", "yard.window", "root", "children");
            ext
        });
        let window = app.update(|cx| {
            cx.open_window(gpui::WindowOptions::default(), yard_shell::new_shell)
                .unwrap()
        });
        app.simulate_window_resize(window.into(), gpui::size(gpui::px(800.), gpui::px(600.)));
        let drawn = lines_of(&trace, || draw(window, &mut app));
        assert_eq!(
            drawn_bar(&drawn),
            (1, 0, 0, false),
            "an item in left replaces the title"
        );
        app.update(|cx| {
            for node in ["window", "title_bar", "body"] {
                ext.remove(cx, node);
                ext.move_node(cx, node, "root", "children", 0);
                ext.set_hidden(cx, node, true);
                ext.set_hidden(cx, node, false);
                ext.set_zoomed(cx, node);
            }
            ext.set_zoomed(cx, "item");
            ext.set_zoomed(cx, "root");
            ext.move_node(cx, "a", "body", "children", 0);
            ext.move_node(cx, "a", "window", "title_bar", 0);
            ext.move_node(cx, "a", "window", "body", 0);
        });
        gpui::AnyWindowHandle::from(window)
            .update(&mut app, |_, window, cx| {
                ext.add(window, cx, "add_body", "tour.card", "body", "children");
                ext.add(window, cx, "add_slot", "tour.card", "window", "title_bar");
                ext.add(window, cx, "add_frame", "yard.window", "root", "children");
            })
            .unwrap();
        let drawn = lines_of(&trace, || draw(window, &mut app));
        assert_eq!(drawn_bar(&drawn), (1, 0, 0, false));
        app.update(|cx| ext.set_hidden(cx, "item", true));
        let drawn = lines_of(&trace, || draw(window, &mut app));
        assert_eq!(
            drawn_bar(&drawn),
            (0, 0, 0, true),
            "a hidden item shows the title again"
        );
        app.update(|cx| ext.set_hidden(cx, "item", false));
        let drawn = lines_of(&trace, || draw(window, &mut app));
        assert_eq!(
            drawn_bar(&drawn),
            (1, 0, 0, false),
            "a shown item hides the title"
        );
    });
    let captured = trace.0.lock().unwrap();
    assert!(captured.entered.is_empty());
    // The frame's three nodes are placed inside `window-frame`, as any placement.
    for expected in [
        r#"root-place node="window" component="yard.window""#,
        r#"node-place node="title_bar" component="yard.title_bar" parent="window" slot="title_bar""#,
        r#"node-place node="body" component="yard.body" parent="window" slot="body""#,
        r#"node-place node="status_bar" component="yard.status_bar" parent="window" slot="status_bar""#,
    ] {
        assert!(
            captured.span_stacks.iter().any(|stack| {
                stack.windows(2).any(|pair| {
                    pair[0].split_whitespace().next() == Some("window-frame")
                        && pair[1].as_str() == expected
                })
            }),
            "missing {expected:?} inside window-frame in {:#?}",
            captured.span_stacks
        );
    }
    for line in [
        r#"root-place node="window" component="yard.window": DEBUG message=root placed node=window component=yard.window extension=yard"#,
        r#"node-place node="title_bar" component="yard.title_bar" parent="window" slot="title_bar": DEBUG message=node placed node=title_bar component=yard.title_bar extension=yard parent=window slot=title_bar"#,
        r#"node-place node="body" component="yard.body" parent="window" slot="body": DEBUG message=node placed node=body component=yard.body extension=yard parent=window slot=body"#,
        r#"node-place node="status_bar" component="yard.status_bar" parent="window" slot="status_bar": DEBUG message=node placed node=status_bar component=yard.status_bar extension=yard parent=window slot=status_bar"#,
    ] {
        assert!(
            captured.lines.iter().any(|actual| actual.as_str() == line),
            "missing {line:?} in {:#?}",
            captured.lines
        );
    }
    assert!(
        captured
            .lines
            .iter()
            .any(|line| line.as_str() == "none: DEBUG message=layout file set path=layout.json"),
        "missing the layout-file line in {:#?}",
        captured.lines
    );
    assert!(
        captured.lines.iter().any(|line| {
            line.contains(": DEBUG") && line.contains("window title") && line.contains("Test Title")
        }),
        "missing the window-title line in {:#?}",
        captured.lines
    );
    // Startup placements into the frame's slots, and of a frame component, are
    // refused inside their `node-place` group.
    for (node, component, parent, slot) in [
        ("p_body_slot", "tour.card", "body", "children"),
        ("p_window_slot", "tour.card", "window", "title_bar"),
        ("p_frame_comp", "yard.window", "root", "children"),
    ] {
        let prefix = format!(
            r#"node-place node="{node}" component="{component}" parent="{parent}" slot="{slot}": INFO"#
        );
        assert!(
            captured.lines.iter().any(|line| {
                line.starts_with(&prefix)
                    && line.contains("the window's frame belongs to the shell")
            }),
            "missing {prefix:?} frame refusal in {:#?}",
            captured.lines
        );
    }
    // Every refused change of a frame node is an INFO with the frame's reason.
    for node in ["window", "title_bar", "body"] {
        for op in ["remove", "move", "hide", "show"] {
            let prefix = format!(r#"layout-op op="{op}" node="{node}" extension="tour": INFO"#);
            assert!(
                captured.lines.iter().any(|line| {
                    line.starts_with(&prefix)
                        && line.contains("the window's frame belongs to the shell")
                }),
                "missing {prefix:?} frame refusal in {:#?}",
                captured.lines
            );
        }
        let prefix = format!(r#"zoom node="{node}" extension="tour": INFO"#);
        assert!(
            captured.lines.iter().any(|line| {
                line.starts_with(&prefix)
                    && line.contains("the window's frame belongs to the shell")
            }),
            "missing {prefix:?} frame refusal in {:#?}",
            captured.lines
        );
    }
    assert!(
        captured.lines.iter().any(|line| {
            line.starts_with(r#"zoom node="item" extension="tour": INFO"#)
                && line.contains("only a node in the body can be zoomed")
        }),
        "missing the bar-item zoom refusal in {:#?}",
        captured.lines
    );
    assert!(
        captured.lines.iter().any(|line| {
            line.starts_with(r#"zoom node="root" extension="tour": INFO"#)
                && line.contains("the root already fills the body")
        }),
        "missing the zoomed-root refusal in {:#?}",
        captured.lines
    );
    // Each move into a frame slot logs the frame's reason inside its layout-op.
    assert_eq!(
        captured
            .lines
            .iter()
            .filter(|line| {
                line.starts_with(r#"layout-op op="move" node="a" extension="tour": INFO"#)
                    && line.contains("the window's frame belongs to the shell")
            })
            .count(),
        3,
        "each move into a frame slot logs the frame's reason in {:#?}",
        captured.lines
    );
    // Adds into the frame's slots, and of a frame component, are refused.
    for node in ["add_body", "add_slot", "add_frame"] {
        let prefix = format!(r#"layout-op op="add" node="{node}" extension="tour": INFO"#);
        assert!(
            captured.lines.iter().any(|line| {
                line.starts_with(&prefix)
                    && line.contains("the window's frame belongs to the shell")
            }),
            "missing {prefix:?} frame refusal in {:#?}",
            captured.lines
        );
    }
    // Frame changes from the saved layout are skipped inside `layout-restore`.
    for (op, node) in [("move", "a"), ("hide", "title_bar"), ("remove", "window")] {
        let expected = format!(
            r#"layout-restore file=layout.json: INFO message=skipped op="{op}" node="{node}" reason="the window's frame belongs to the shell""#
        );
        assert!(
            captured.lines.iter().any(|line| line.as_str() == expected),
            "missing {expected} in {:#?}",
            captured.lines
        );
    }
    assert!(
        captured.lines.iter().any(|line| {
            line.as_str()
                == "layout-restore file=layout.json: DEBUG message=restored applied=0 skipped=3"
        }),
        "missing the restore summary in {:#?}",
        captured.lines
    );
    assert_eq!(
        captured
            .lines
            .iter()
            .filter(|line| line.contains(": ERROR"))
            .count(),
        0,
        "nothing at ERROR in {:#?}",
        captured.lines
    );
}
