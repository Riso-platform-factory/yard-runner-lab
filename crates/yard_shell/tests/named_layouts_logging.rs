use gpui::{AppContext, InteractiveElement, IntoElement, Styled, TestAppContext, px};
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
use yard_shell as shell;

struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |since| since.as_nanos());
        let path = std::env::temp_dir().join(format!(
            "yard-shell-named-layouts-logging-{}-{}-{nanos:x}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed),
        ));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    fn path(&self) -> &std::path::Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

// The diagnostic log's records are re-emitted as tracing events under this
// target, so one subscriber sees both logs.
const PROBE: &str = "named_layouts_probe";

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
        if attributes.metadata().target().starts_with("yard_shell") {
            let mut stack: Vec<String> = captured
                .entered
                .iter()
                .map(|entered| captured.spans[entered].clone())
                .collect();
            stack.push(label.clone());
            captured.span_stacks.push(stack);
        }
        captured.spans.insert(id, label);
        tracing::span::Id::from_u64(id)
    }
    fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
    fn event(&self, event: &tracing::Event<'_>) {
        let target = event.metadata().target();
        if (!target.starts_with("yard_shell") && target != PROBE)
            || *event.metadata().level() == tracing::Level::TRACE
        {
            return;
        }
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

impl Trace {
    fn mark(&self) -> (usize, usize) {
        let captured = self.0.lock().unwrap();
        (captured.lines.len(), captured.span_stacks.len())
    }
}

// The one process-wide diagnostic logger this file installs once; alone here,
// it cannot collide with another test's logger.
struct Diagnostics;

impl log::Log for Diagnostics {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        metadata.target().starts_with("yard_shell")
    }
    fn log(&self, record: &log::Record<'_>) {
        if !self.enabled(record.metadata()) {
            return;
        }
        match record.level() {
            log::Level::Error => tracing::error!(target: PROBE, "{}", record.args()),
            log::Level::Warn => tracing::warn!(target: PROBE, "{}", record.args()),
            log::Level::Info => tracing::info!(target: PROBE, "{}", record.args()),
            log::Level::Debug => tracing::debug!(target: PROBE, "{}", record.args()),
            log::Level::Trace => tracing::trace!(target: PROBE, "{}", record.args()),
        }
    }
    fn flush(&self) {}
}

// A card fills the zone its container gives it and is found as `CARD-<node>`.
struct Card {
    node: String,
}

impl gpui::Render for Card {
    fn render(&mut self, _: &mut gpui::Window, _: &mut gpui::Context<Self>) -> impl IntoElement {
        gpui::div()
            .debug_selector(|| format!("CARD-{}", self.node))
            .size_full()
    }
}

fn extension(app: &mut TestAppContext) -> shell::Extension {
    app.update(|cx| {
        yard_ui::init(cx);
        let host = shell::Extension::new(cx, "tour").unwrap();
        host.register(cx, "tour.card", &[], |node, _, _, cx| {
            cx.new(|_| Card {
                node: node.to_string(),
            })
            .into()
        });
        host
    })
}

// Draw without forcing a refresh: operations must schedule it themselves.
fn draw(window: gpui::WindowHandle<shell::Shell>, app: &mut TestAppContext) {
    gpui::AnyWindowHandle::from(window)
        .update(app, |_, window, cx| {
            let _ = window.draw(cx);
        })
        .unwrap();
}

fn open_and_draw(app: &mut TestAppContext) -> gpui::WindowHandle<shell::Shell> {
    let window = app.update(|cx| {
        cx.open_window(gpui::WindowOptions::default(), shell::new_shell)
            .unwrap()
    });
    app.simulate_window_resize(window.into(), gpui::size(px(900.), px(600.)));
    window.update(app, |_, window, _| window.refresh()).unwrap();
    draw(window, app);
    window
}

fn show_layout(
    window: gpui::WindowHandle<shell::Shell>,
    app: &mut TestAppContext,
    host: &shell::Extension,
    name: &str,
) {
    gpui::AnyWindowHandle::from(window)
        .update(app, |_, window, cx| host.show_layout(window, cx, name))
        .unwrap();
}

// True when `stack` is `<parent>` inside of which `label` opened directly:
// `parent` is the last-but-one label.
fn child(stack: &[String], parent: &str) -> bool {
    stack.len() >= 2 && stack[stack.len() - 2].starts_with(parent)
}

// True when `line` holds `node` as a `node` field.
fn node_field(line: &str, node: &str) -> bool {
    line.contains(&format!("node={node} ")) || line.ends_with(&format!("node={node}"))
}

// The named-layout log: `named-layout-register` for a registration or a
// refusal, `layout-switch{from, to}` for a switch, holding a `layout-restore`
// for the user layer, a skipped `INFO` per refused operation, `view dropped`
// and `component-create` for the views that leave and come, and a `switched`
// with `kept`, `dropped`, `created`; `already shown` on a no-op switch and
// `no named layout` for an unknown name. Alone in this binary: every call
// reaching these log lines shares this one subscriber and logger.
#[gpui::test]
fn named_layouts_log_their_groups(cx: &mut TestAppContext) {
    log::set_logger(&Diagnostics).unwrap();
    log::set_max_level(log::LevelFilter::Trace);
    let trace = Trace::default();
    let (to_swap, to_diet, to_mixed, reshow, ghost, to_default) =
        tracing::subscriber::with_default(trace.clone(), || {
            let temp = TempDir::new();
            let layouts = temp.path().join("data").join("layouts");
            let mut app = cx.new_app();
            let files = layouts.clone();
            app.update(|cx| {
                shell::set_layout_file(cx, temp.path().join("data").join("layout.json"));
                shell::set_named_layout_files(cx, move |name| files.join(format!("{name}.json")));
            });
            let host = extension(&mut app);
            app.update(|cx| {
                host.place_root(cx, "root", "yard.row");
                for node in ["a", "b", "c"] {
                    host.place(cx, node, "tour.card", "root", "children");
                }
                host.named_layout(cx, "tour.swap", |layout| {
                    layout.move_node("c", "root", "children", 0);
                });
                host.named_layout(cx, "tour.diet", |layout| {
                    layout.remove("a");
                    layout.add("d", "tour.card", "root", "children");
                });
                host.named_layout(cx, "tour.mixed", |layout| {
                    // A second `a` and a remove of a node that is nowhere are
                    // refused; `b` is hidden and `e` is added.
                    layout.add("a", "tour.card", "root", "children");
                    layout.remove("ghost");
                    layout.set_hidden("b", true);
                    layout.add("e", "tour.card", "root", "children");
                });
                host.named_layout(cx, "other.x", |_| {});
            });
            let window = open_and_draw(&mut app);
            // Swap reorders only: nothing drops, nothing is created.
            let to_swap = trace.mark();
            show_layout(window, &mut app, &host, "tour.swap");
            draw(window, &mut app);
            // Diet removes `a` and adds `d`: a `view dropped` and a
            // `component-create`.
            let to_diet = trace.mark();
            show_layout(window, &mut app, &host, "tour.diet");
            draw(window, &mut app);
            // Mixed has two refused operations, drops `d`, and brings `a`
            // back plus `e`.
            let to_mixed = trace.mark();
            show_layout(window, &mut app, &host, "tour.mixed");
            draw(window, &mut app);
            // Showing the shown layout, then a name nobody registered.
            let reshow = trace.mark();
            show_layout(window, &mut app, &host, "tour.mixed");
            let ghost = trace.mark();
            show_layout(window, &mut app, &host, "tour.ghost");
            // Back to `yard.default`, whose user layer is layout.json.
            let to_default = trace.mark();
            show_layout(window, &mut app, &host, "yard.default");
            draw(window, &mut app);
            (to_swap, to_diet, to_mixed, reshow, ghost, to_default)
        });
    let captured = trace.0.lock().unwrap();
    assert!(captured.entered.is_empty());
    let lines: Vec<String> = captured
        .lines
        .iter()
        .map(|line| line.replace('"', ""))
        .collect();
    let stacks: Vec<Vec<String>> = captured
        .span_stacks
        .iter()
        .map(|stack| stack.iter().map(|label| label.replace('"', "")).collect())
        .collect();
    let between = |mark: (usize, usize), next: (usize, usize)| {
        (&lines[mark.0..next.0], &stacks[mark.1..next.1])
    };

    // Every registered layout opens `named-layout-register{name, extension,
    // operations}` and logs `registered` inside.
    for (name, operations) in [
        ("tour.swap", "operations=1"),
        ("tour.diet", "operations=2"),
        ("tour.mixed", "operations=4"),
    ] {
        assert!(
            stacks.iter().any(|stack| {
                stack.last().is_some_and(|label| {
                    label.starts_with("named-layout-register")
                        && label.contains(&format!("name={name}"))
                        && label.contains("extension=tour")
                        && label.contains(operations)
                })
            }),
            "missing named-layout-register{{{name}}}: {stacks:#?}"
        );
        assert!(
            lines.iter().any(|line| {
                line.starts_with("named-layout-register")
                    && line.contains(&format!("name={name}"))
                    && line.contains(": DEBUG")
                    && line.contains("registered")
            }),
            "missing `registered` inside {name}'s group: {lines:#?}"
        );
    }
    // A refused registration logs its ERROR inside its own group.
    assert!(
        lines.iter().any(|line| {
            line.starts_with("named-layout-register")
                && line.contains("name=other.x")
                && line.contains(": ERROR")
        }),
        "missing the refused registration's ERROR inside its group: {lines:#?}"
    );

    // The switch to `tour.swap` opens `layout-switch{from, to}`.
    let (switch_lines, switch_stacks) = between(to_swap, to_diet);
    assert!(
        switch_stacks.iter().any(|stack| {
            stack.last().is_some_and(|label| {
                label.starts_with("layout-switch")
                    && label.contains("from=yard.default")
                    && label.contains("to=tour.swap")
            })
        }),
        "missing layout-switch{{from=yard.default, to=tour.swap}}: {switch_stacks:#?}"
    );
    // It holds a `layout-restore{file}` for the swap's user layer file.
    assert!(
        switch_stacks.iter().any(|stack| {
            stack.last().is_some_and(|label| {
                label.starts_with("layout-restore") && label.contains("tour.swap.json")
            }) && child(stack, "layout-switch")
        }),
        "missing layout-restore{{file=tour.swap.json}} inside layout-switch: {switch_stacks:#?}"
    );
    // And ends with `switched {kept, dropped, created}`: a reorder kept every
    // view, so nothing dropped and nothing was created.
    assert!(
        switch_lines.iter().any(|line| {
            line.starts_with("layout-switch")
                && line.contains(": DEBUG")
                && line.contains("switched")
                && line.contains("kept=")
                && line.contains("dropped=0")
                && line.contains("created=0")
        }),
        "missing `switched` inside the swap's layout-switch: {switch_lines:#?}"
    );

    // The switch to `tour.diet`: `a` leaves and `d` comes.
    let (switch_lines, switch_stacks) = between(to_diet, to_mixed);
    assert!(
        switch_stacks.iter().any(|stack| {
            stack.last().is_some_and(|label| {
                label.starts_with("layout-switch")
                    && label.contains("from=tour.swap")
                    && label.contains("to=tour.diet")
            })
        }),
        "missing layout-switch{{from=tour.swap, to=tour.diet}}: {switch_stacks:#?}"
    );
    assert!(
        switch_stacks.iter().any(|stack| {
            stack.last().is_some_and(|label| {
                label.starts_with("layout-restore") && label.contains("tour.diet.json")
            }) && child(stack, "layout-switch")
        }),
        "missing layout-restore{{file=tour.diet.json}} inside layout-switch: {switch_stacks:#?}"
    );
    assert!(
        switch_lines.iter().any(|line| {
            line.starts_with("layout-switch")
                && line.contains("view dropped")
                && node_field(line, "a")
        }),
        "missing `view dropped` for a inside layout-switch: {switch_lines:#?}"
    );
    assert!(
        switch_stacks.iter().any(|stack| {
            stack.last().is_some_and(|label| {
                label.starts_with("component-create") && node_field(label, "d")
            }) && child(stack, "layout-switch")
        }),
        "missing d's component-create inside layout-switch: {switch_stacks:#?}"
    );
    assert!(
        switch_lines.iter().any(|line| {
            line.starts_with("layout-switch")
                && line.contains(": DEBUG")
                && line.contains("switched")
                && line.contains("kept=")
                && line.contains("dropped=1")
                && line.contains("created=1")
        }),
        "missing `switched` inside the diet's layout-switch: {switch_lines:#?}"
    );

    // The switch to `tour.mixed`: a skipped INFO per refused operation, `d`
    // drops, `a` and `e` are created.
    let (switch_lines, switch_stacks) = between(to_mixed, reshow);
    for node in ["a", "ghost"] {
        assert!(
            switch_lines.iter().any(|line| {
                line.starts_with("layout-switch")
                    && line.contains(": INFO")
                    && line.contains("skipped")
                    && node_field(line, node)
            }),
            "missing the skipped INFO for {node} inside layout-switch: {switch_lines:#?}"
        );
    }
    assert!(
        switch_lines.iter().any(|line| {
            line.starts_with("layout-switch")
                && line.contains("view dropped")
                && node_field(line, "d")
        }),
        "missing `view dropped` for d inside layout-switch: {switch_lines:#?}"
    );
    for node in ["a", "e"] {
        assert!(
            switch_stacks.iter().any(|stack| {
                stack.last().is_some_and(|label| {
                    label.starts_with("component-create") && node_field(label, node)
                }) && child(stack, "layout-switch")
            }),
            "missing {node}'s component-create inside layout-switch: {switch_stacks:#?}"
        );
    }
    assert!(
        switch_lines.iter().any(|line| {
            line.starts_with("layout-switch")
                && line.contains(": DEBUG")
                && line.contains("switched")
                && line.contains("kept=")
                && line.contains("dropped=1")
                && line.contains("created=2")
        }),
        "missing `switched` inside the mixed's layout-switch: {switch_lines:#?}"
    );

    // Showing `tour.mixed` while it is shown: `already shown`, and nothing
    // else — no restore is read again and nothing is counted.
    let (switch_lines, switch_stacks) = between(reshow, ghost);
    assert!(
        switch_lines.iter().any(|line| {
            line.starts_with("layout-switch")
                && line.contains(": DEBUG")
                && line.contains("already shown")
        }),
        "missing `already shown` inside layout-switch: {switch_lines:#?}"
    );
    assert!(
        !switch_lines.iter().any(|line| line.contains("switched"))
            && !switch_stacks
                .iter()
                .flatten()
                .any(|label| label.starts_with("layout-restore")),
        "the no-op switch did work: {switch_lines:#?}"
    );

    // `tour.ghost` was never registered: `no named layout {name}` in the
    // group.
    let (switch_lines, _) = between(ghost, to_default);
    assert!(
        switch_lines.iter().any(|line| {
            line.starts_with("layout-switch")
                && line.contains(": INFO")
                && line.contains("no named layout")
                && line.contains("tour.ghost")
        }),
        "missing `no named layout` inside layout-switch: {switch_lines:#?}"
    );

    // Back to `yard.default`: its user layer is read from layout.json.
    let (_, switch_stacks) = between(to_default, (lines.len(), stacks.len()));
    assert!(
        switch_stacks.iter().any(|stack| {
            stack.last().is_some_and(|label| {
                label.starts_with("layout-switch")
                    && label.contains("from=tour.mixed")
                    && label.contains("to=yard.default")
            })
        }),
        "missing layout-switch{{from=tour.mixed, to=yard.default}}: {switch_stacks:#?}"
    );
    assert!(
        switch_stacks.iter().any(|stack| {
            stack.last().is_some_and(|label| {
                label.starts_with("layout-restore") && label.contains(" file=layout.json")
            }) && child(stack, "layout-switch")
        }),
        "missing layout-restore{{file=layout.json}} inside layout-switch: {switch_stacks:#?}"
    );
}
