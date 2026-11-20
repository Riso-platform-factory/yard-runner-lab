use gpui::TestAppContext;
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

#[derive(Default)]
struct Captured {
    spans: BTreeMap<u64, String>,
    entered: Vec<u64>,
    lines: Vec<String>,
}

#[derive(Clone, Default)]
struct Trace(Arc<Mutex<Captured>>);

#[derive(Default)]
struct Fields(String);
impl tracing::field::Visit for Fields {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        use std::fmt::Write;
        write!(self.0, " {}={value:?}", field.name()).unwrap();
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
        captured.spans.insert(
            id,
            format!(
                "{} {}{}",
                attributes.metadata().level(),
                attributes.metadata().name(),
                fields.0
            ),
        );
        tracing::span::Id::from_u64(id)
    }
    fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
    fn event(&self, event: &tracing::Event<'_>) {
        if !event.metadata().target().starts_with("yard_shell")
            && event.metadata().target() != "app_menu_diagnostic"
            || *event.metadata().level() == tracing::Level::TRACE
        {
            return;
        }
        let mut fields = Fields::default();
        event.record(&mut fields);
        let mut captured = self.0.lock().unwrap();
        // Every enclosing group, outermost first.
        let group = if captured.entered.is_empty() {
            String::from("none")
        } else {
            captured
                .entered
                .iter()
                .map(|id| captured.spans[id].as_str())
                .collect::<Vec<_>>()
                .join(" > ")
        };
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
            tracing::error!(target: "app_menu_diagnostic", "{}", record.args());
        }
    }
    fn flush(&self) {}
}

/// The innermost group enclosing `line`, or "" when no group is open.
fn group_of(line: &str) -> &str {
    line.split_once(": ")
        .and_then(|(stack, _)| stack.rsplit(" > ").next())
        .unwrap_or("")
}

/// Every group enclosing `line`, outermost first, joined by " > ".
fn stack_of(line: &str) -> &str {
    line.split_once(": ").map(|(stack, _)| stack).unwrap_or("")
}

/// The spans ever created whose name starts with `prefix`, quotes stripped.
fn spans_named(trace: &Trace, prefix: &str) -> Vec<String> {
    trace
        .0
        .lock()
        .unwrap()
        .spans
        .values()
        .map(|span| span.replace('"', ""))
        .filter(|span| span.starts_with(prefix))
        .collect()
}

/// The menus the application's composition root gives the startup: the
/// application's own menu and `View` with the shell's actions.
fn menus() -> Vec<gpui::Menu> {
    vec![
        gpui::Menu::new("yard").items(vec![gpui::MenuItem::action("Quit yard", yard_shell::Quit)]),
        gpui::Menu::new("View").items(vec![
            gpui::MenuItem::action("Zoom", yard_shell::ToggleZoom),
            gpui::MenuItem::action("Hide Node", yard_shell::Hide),
            gpui::MenuItem::separator(),
            gpui::MenuItem::action("Select Next Tab", yard_shell::SelectNextTab),
            gpui::MenuItem::action("Select Previous Tab", yard_shell::SelectPreviousTab),
            gpui::MenuItem::separator(),
            gpui::MenuItem::action("Focus Left", yard_shell::FocusLeft),
            gpui::MenuItem::action("Focus Right", yard_shell::FocusRight),
            gpui::MenuItem::action("Focus Up", yard_shell::FocusUp),
            gpui::MenuItem::action("Focus Down", yard_shell::FocusDown),
        ]),
    ]
}

// `keys-bind` logs one more key than before app-menu: eleven on macOS, nine
// elsewhere.
#[cfg(target_os = "macos")]
const BOUND_KEYS: usize = 12;
#[cfg(not(target_os = "macos"))]
const BOUND_KEYS: usize = 10;

// `yard::Quit`'s key: `cmd-q` on macOS, `ctrl-q` elsewhere.
#[cfg(target_os = "macos")]
const QUIT_KEY: &str = "cmd-q";
#[cfg(not(target_os = "macos"))]
const QUIT_KEY: &str = "ctrl-q";

// Alone in this binary: all calls reaching the captured lines use one subscriber.
#[gpui::test]
fn the_shell_logs_menus_set_keys_bind_and_app_quit(cx: &mut TestAppContext) {
    // Until a `tracing` dispatcher has been set in the process, `tracing`'s
    // `log` feature (which GPUI's Linux platform turns on, through calloop)
    // sends every trace line to the `log` crate too; one set here keeps the
    // trace out of the diagnostic log on every system, as in the app.
    tracing::subscriber::with_default(tracing::subscriber::NoSubscriber::default(), || {});
    log::set_logger(&Diagnostics).unwrap();
    log::set_max_level(log::LevelFilter::Trace);
    cx.update(yard_ui::init);
    let trace = Trace::default();
    tracing::subscriber::with_default(trace.clone(), || {
        {
            // The startup binds the default keys and handles `Quit` in
            // `app-init`, and sets the menus after the extensions run, inside
            // `app-startup`.
            let _startup = tracing::info_span!("app-startup").entered();
            cx.update(|cx| {
                {
                    let _init = tracing::info_span!("app-init").entered();
                    yard_shell::bind_default_keys(cx);
                    yard_shell::handle_quit(cx);
                }
                {
                    let _load = tracing::info_span!("extensions-load").entered();
                }
                yard_shell::set_menus(cx, menus());
            });
        }
        cx.update(|cx| cx.dispatch_action(&yard_shell::Quit));
        assert_eq!(
            spans_named(&trace, "INFO app-quit").len(),
            1,
            "`Quit` with no window opens `app-quit` once"
        );
        let window = cx.update(|cx| {
            cx.open_window(gpui::WindowOptions::default(), yard_shell::new_shell)
                .unwrap()
        });
        window
            .update(cx, |_, window, _| window.activate_window())
            .unwrap();
        cx.update(|cx| {
            assert_eq!(
                cx.active_window(),
                Some(window.into()),
                "the shell window is the active window"
            )
        });
        window
            .update(cx, |_, window, cx| {
                window.dispatch_action(Box::new(yard_shell::Quit), cx)
            })
            .unwrap();
        assert_eq!(
            spans_named(&trace, "INFO app-quit").len(),
            2,
            "`Quit` with a window open opens `app-quit` once more"
        );
    });
    let captured = trace.0.lock().unwrap();
    assert!(captured.entered.is_empty());
    let spans: Vec<String> = captured
        .spans
        .values()
        .map(|span| span.replace('"', ""))
        .collect();
    let lines: Vec<String> = captured
        .lines
        .iter()
        .map(|line| line.replace('"', ""))
        .collect();
    // `menus-set{count}` opens once inside `app-startup`, after
    // `extensions-load`, with one `menu` line per menu: `name` and `items`,
    // the number of its items, separators included.
    let menus_group = "INFO menus-set count=2";
    assert_eq!(
        spans.iter().filter(|span| **span == menus_group).count(),
        1,
        "one `{menus_group}` group in {spans:#?}"
    );
    let loaded = spans
        .iter()
        .position(|span| span.starts_with("INFO extensions-load"))
        .expect("the `extensions-load` group");
    let set = spans
        .iter()
        .position(|span| span.as_str() == menus_group)
        .expect("the `menus-set` group");
    assert!(
        loaded < set,
        "`menus-set` comes after `extensions-load` in {spans:#?}"
    );
    let menus_stack = format!("INFO app-startup > {menus_group}");
    let inside: Vec<&str> = lines
        .iter()
        .map(String::as_str)
        .filter(|line| group_of(line) == menus_group)
        .collect();
    assert_eq!(inside.len(), 2, "lines inside `{menus_group}`: {lines:#?}");
    for (name, items) in [("yard", "items=1"), ("View", "items=10")] {
        let name = format!("name={name}");
        assert_eq!(
            inside
                .iter()
                .filter(|line| {
                    stack_of(line) == menus_stack
                        && line.contains(": DEBUG")
                        && line.contains(name.as_str())
                        && line.contains(items)
                        && line.contains("message=menu")
                })
                .count(),
            1,
            "`{menus_group}` must log `menu` for {name} with {items} once in {inside:#?}"
        );
    }
    // `keys-bind` counts one more key, and its `bound` line for `yard::Quit`
    // names no key context.
    let keys_group = format!("INFO keys-bind count={BOUND_KEYS}");
    assert_eq!(
        spans.iter().filter(|span| **span == keys_group).count(),
        1,
        "one `{keys_group}` group in {spans:#?}"
    );
    let keys_stack = format!("INFO app-startup > INFO app-init > {keys_group}");
    let inside: Vec<&str> = lines
        .iter()
        .map(String::as_str)
        .filter(|line| group_of(line) == keys_group.as_str())
        .collect();
    assert_eq!(
        inside.len(),
        BOUND_KEYS,
        "lines inside `{keys_group}`: {lines:#?}"
    );
    let quit: Vec<&str> = inside
        .iter()
        .filter(|line| {
            stack_of(line) == keys_stack
                && line.contains(": DEBUG")
                && line.contains(QUIT_KEY)
                && line.contains("Quit")
                && line.contains("message=bound")
        })
        .copied()
        .collect();
    assert_eq!(quit.len(), 1, "one `bound` line for `Quit` in {inside:#?}");
    assert!(
        !quit[0].contains("Shell") && !quit[0].contains("ModalLayer"),
        "the `Quit` binding names no key context: {}",
        quit[0]
    );
    assert!(
        !lines
            .iter()
            .any(|line| line.contains(": ERROR") || line.contains(": WARN")),
        "nothing at ERROR or WARN in {lines:#?}"
    );
}
