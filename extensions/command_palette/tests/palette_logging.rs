use gpui::{Action, AppContext, InteractiveElement, IntoElement, Styled, TestAppContext};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};
use yard_shell::Extension;

/// The palette's key, per the contract: `cmd-shift-p` on macOS and
/// `ctrl-shift-p` elsewhere.
#[cfg(target_os = "macos")]
const TOGGLE_KEY: &str = "cmd-shift-p";
#[cfg(not(target_os = "macos"))]
const TOGGLE_KEY: &str = "ctrl-shift-p";

// The test card's own actions, so `command-run` has something to dispatch.
gpui::actions!(palette_test, [Banquet, Bequest, Quest, Quiet]);

/// The target of the card handlers' own lines, so they land inside the
/// `command-run` group, and of the bridged diagnostic log.
const PROBE: &str = "palette_logging_probe";

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

/// The shell's and the palette's diagnostic records, bridged into the trace
/// at ERROR: anything they log is a bug or a failure by definition.
struct Diagnostics;
impl log::Log for Diagnostics {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        metadata.target().starts_with("yard") || metadata.target().starts_with("command_palette")
    }
    fn log(&self, record: &log::Record<'_>) {
        if self.enabled(record.metadata()) {
            assert_eq!(record.level(), log::Level::Error);
            tracing::error!(target: PROBE, "{}", record.args());
        }
    }
    fn flush(&self) {}
}

struct Card {
    node: String,
}

impl gpui::Render for Card {
    fn render(&mut self, _: &mut gpui::Window, cx: &mut gpui::Context<Self>) -> impl IntoElement {
        let mut card = gpui::div()
            .debug_selector(|| format!("CARD-{}", self.node))
            .size_full();
        // The element handling the card's actions tracks the node's own
        // handle: they are available only while this card has the focus.
        if let Some(handle) = yard_shell::focus_handle(cx, &self.node) {
            card = card
                .track_focus(&handle)
                .key_context("palette_test")
                .on_action(|_: &Banquet, _, _| tracing::info!(target: PROBE, "banquet ran"))
                .on_action(|_: &Bequest, _, _| tracing::info!(target: PROBE, "bequest ran"))
                .on_action(|_: &Quest, _, _| tracing::info!(target: PROBE, "quest ran"))
                .on_action(|_: &Quiet, _, _| tracing::info!(target: PROBE, "quiet ran"));
        }
        card
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

/// Clicks the center of the element with `selector`.
fn click(visual: &mut gpui::VisualTestContext, selector: &'static str) {
    let bounds = visual
        .debug_bounds(selector)
        .unwrap_or_else(|| panic!("{selector} has bounds"));
    visual.simulate_click(bounds.center(), gpui::Modifiers::default());
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

/// The level and fields of `line`, without the enclosing groups.
fn event_of(line: &str) -> &str {
    line.split_once(": ")
        .map(|(_, event)| event)
        .unwrap_or(line)
}

/// The commands the palette must list for the current focus: every available
/// action once, without `NoAction`, `Unbind`, or the palette's `Toggle`.
fn expected_commands(visual: &mut gpui::VisualTestContext) -> Vec<String> {
    visual.update(|window, cx| {
        let mut names: Vec<String> = window
            .available_actions(cx)
            .iter()
            .map(|action| action.name().to_string())
            .collect();
        names.retain(|name| {
            let name = name.as_str();
            name != command_palette::Toggle::name_for_type()
                && !name.ends_with("::NoAction")
                && !name.ends_with("::Unbind")
        });
        names.sort();
        names.dedup();
        names
    })
}

// Alone in this binary: every captured callsite runs under one scoped subscriber.
#[gpui::test]
fn listing_matching_and_running_commands_log_what_the_contract_tables(cx: &mut TestAppContext) {
    // Until a `tracing` dispatcher has been set in the process, `tracing`'s
    // `log` feature (which GPUI's Linux platform turns on, through calloop)
    // sends every trace line to the `log` crate too; one set here keeps the
    // trace out of the diagnostic log on every system, as in the app.
    tracing::subscriber::with_default(tracing::subscriber::NoSubscriber::default(), || {});
    log::set_logger(&Diagnostics).unwrap();
    log::set_max_level(log::LevelFilter::Trace);
    cx.update(yard_ui::init);
    let trace = Trace::default();
    let expected = tracing::subscriber::with_default(trace.clone(), || {
        cx.update(|cx| {
            // The startup binds the default keys in `app-init`, before extensions run.
            let _group = tracing::info_span!("app-init").entered();
            yard_shell::bind_default_keys(cx);
            yard_ui::bind_text_input_keys(cx);
        });
        cx.update(|cx| {
            // The startup wraps each extension's setup in `extension-init`.
            let _group =
                tracing::info_span!("extension-init", extension = "command_palette").entered();
            let ext = Extension::new(cx, "command_palette").unwrap();
            command_palette::init(cx, &ext);
        });
        cx.update(|cx| {
            let test = Extension::new(cx, "palette_test").unwrap();
            test.register(cx, "palette_test.card", &[], |node, _, _, cx| {
                cx.new(|_| Card {
                    node: node.to_string(),
                })
                .into()
            });
            test.place_root(cx, "root", "yard.row");
            for node in ["a", "b"] {
                test.place(cx, node, "palette_test.card", "root", "children");
            }
        });
        let window = cx.update(|cx| {
            cx.open_window(gpui::WindowOptions::default(), yard_shell::new_shell)
                .unwrap()
        });
        cx.simulate_window_resize(window.into(), gpui::size(gpui::px(800.), gpui::px(600.)));
        draw(window, cx);
        let mut visual = gpui::VisualTestContext::from_window(window.into(), cx);
        click(&mut visual, "CARD-a");
        draw(window, cx);
        visual.simulate_keystrokes(TOGGLE_KEY);
        draw(window, cx);
        // `commands matched` follows the keystrokes: `q`, `e`, `q`, `backspace`.
        visual.simulate_input("q");
        visual.simulate_input("e");
        visual.simulate_input("q");
        visual.simulate_keystrokes("backspace");
        // The query is back to `qe`: `enter` runs the first match, `quest`.
        visual.simulate_keystrokes("enter");
        draw(window, cx);
        visual.run_until_parked();
        // Back on the card, these are the commands the palette listed.
        expected_commands(&mut visual)
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
    for name in [
        "palette_test::Banquet",
        "palette_test::Bequest",
        "palette_test::Quest",
        "palette_test::Quiet",
        "yard::FocusDown",
        "yard::FocusLeft",
        "yard::FocusRight",
        "yard::FocusUp",
        "yard::Hide",
        "yard::SelectNextTab",
        "yard::SelectPreviousTab",
        "yard::ToggleZoom",
    ] {
        assert!(
            expected.iter().any(|candidate| candidate == name),
            "{name} is available where the focus is"
        );
    }
    // The key is bound: inside the extension's `keys-bind` group, inside its
    // `extension-init`, one DEBUG line names the key, the action, and `Shell`.
    let keys_bind = "INFO extension-init extension=command_palette > INFO keys-bind";
    assert!(
        lines.iter().any(|line| {
            stack_of(line).starts_with(keys_bind)
                && event_of(line).starts_with("DEBUG")
                && event_of(line).contains(&format!("keys={TOGGLE_KEY}"))
                && event_of(line).contains("action=command_palette::Toggle")
                && event_of(line).contains("context=Shell")
        }),
        "one `bound` line for `{TOGGLE_KEY}`/`command_palette::Toggle`/`Shell` inside the extension's `keys-bind` in {lines:#?}"
    );
    // `commands listed` at DEBUG with `count`, inside the modal layer's
    // `modal-show` group.
    let modal_show = spans
        .iter()
        .find(|span| span.starts_with("INFO modal-show"))
        .expect("the palette opens inside a `modal-show` group");
    assert_eq!(
        lines
            .iter()
            .filter(|line| {
                group_of(line) == modal_show.as_str()
                    && event_of(line).starts_with("DEBUG")
                    && event_of(line).contains("commands listed")
                    && event_of(line).contains(&format!("count={}", expected.len()))
            })
            .count(),
        1,
        "one `commands listed` at DEBUG with `count={}` inside `{modal_show}` in {lines:#?}",
        expected.len()
    );
    // `commands matched` at TRACE with `query_length` and `count`, one per
    // keystroke: `q` matches the four `qe` commands, `e` keeps them, a third
    // `q` empties the list, and `backspace` brings them back.
    let matched: Vec<&String> = lines
        .iter()
        .filter(|line| {
            event_of(line).starts_with("TRACE") && event_of(line).contains("commands matched")
        })
        .collect();
    assert_eq!(
        matched.len(),
        4,
        "one `commands matched` line per keystroke in {lines:#?}"
    );
    for (line, (query_length, count)) in matched.iter().zip([(1, 4), (2, 4), (3, 0), (2, 4)]) {
        assert!(
            event_of(line).contains(&format!("query_length={query_length}"))
                && event_of(line).contains(&format!("count={count}")),
            "`commands matched` with query_length={query_length} and count={count}: {line}"
        );
    }
    // A `command-run{action}` group wraps the dispatch: the action's own line
    // is inside it.
    let command_run = {
        let groups: Vec<&String> = spans
            .iter()
            .filter(|span| {
                span.starts_with("INFO command-run") && span.contains("action=palette_test::Quest")
            })
            .collect();
        assert_eq!(
            groups.len(),
            1,
            "one `command-run` group for `palette_test::Quest` in {spans:#?}"
        );
        groups[0]
    };
    assert!(
        lines.iter().any(|line| {
            group_of(line) == command_run.as_str() && event_of(line) == "INFO message=quest ran"
        }),
        "the action's own `quest ran` line is inside `{command_run}` in {lines:#?}"
    );
    assert_eq!(
        lines
            .iter()
            .filter(|line| event_of(line).starts_with("ERROR"))
            .count(),
        0,
        "nothing at ERROR in {lines:#?}"
    );
}
