//! Component factories and the root view kept by the main window.

use super::session::SessionWindow;
gpui::actions!(
    yard,
    [
        ToggleZoom,
        Hide,
        SelectNextTab,
        SelectPreviousTab,
        FocusLeft,
        FocusRight,
        FocusUp,
        FocusDown,
        NewWindow
    ]
);

gpui::actions!(yard, [Dismiss]);
gpui::actions!(yard, [Quit]);
use gpui::{
    Action, Along, AnyEntity, AnyView, App, AppContext, Axis, Bounds, Context, DefiniteLength, Div,
    Entity, FocusHandle, Global, InteractiveElement, IntoElement, KeyBinding,
    KeyBindingContextPredicate, ParentElement, Pixels, Point, Rems, Render, ScrollEvent,
    ScrollHandle, SharedString, Size, Styled, Subscription, WeakEntity, Window, canvas, div,
};
use std::{
    any::{TypeId, type_name},
    cell::{Cell, RefCell},
    collections::{BTreeMap, BTreeSet, VecDeque, btree_map::Entry},
    path::{Path, PathBuf},
    rc::{Rc, Weak},
    sync::{Arc, Mutex, PoisonError},
    time::{Duration, Instant},
};
use tracing::Instrument as _;
use yard_layout::Layers;
use yard_layout::{Applied, ComponentId, Node, NodeId, Operation, Position, Props, SlotName, Tree};
use yard_layout::{decode, encode};
use yard_paths::write_atomic;
use yard_state::{self as state_format, SavedNode, WindowStates};
use yard_state::{StoreValues, decode_store, encode_store};

/// A read-only snapshot of one node in the extension's layout tree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NodeInfo {
    pub id: SharedString,
    pub depth: usize,
    pub slot: Option<SharedString>,
    pub component: SharedString,
    pub extension: SharedString,
    pub props: serde_json::Value,
    pub hidden: bool,
    pub origin: Origin,
}

/// The layer that introduced a node, or the item key of a runtime copy.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Origin {
    Default,
    NamedLayout(SharedString),
    UserLayer,
    Copy(SharedString),
}

type WindowOpener = Rc<dyn Fn(&mut App)>;
type UrlHandler = Rc<dyn Fn(&str, &mut App)>;

type LayoutObserver = Rc<dyn Fn(&mut App)>;
type LayoutObservers = Rc<RefCell<BTreeMap<u64, LayoutObserver>>>;

/// Returns the body layout, including hidden nodes and item-slot copies.
pub fn inspect(cx: &App) -> Vec<NodeInfo> {
    if no_window(cx) {
        tracing::info!("no window");
        return Vec::new();
    }
    fn visit(
        layout_state: &WindowState,
        tree: &Tree,
        node: &Node,
        depth: usize,
        slot: Option<&SlotName>,
        out: &mut Vec<NodeInfo>,
    ) {
        let origin = if node.id.as_str().contains('@') {
            Origin::Copy(node.id.as_str().split_once('@').unwrap().1.to_owned().into())
        } else if layout_state.tree.borrow().user_layer().iter().any(|operation| {
            matches!(operation, Operation::Add { node: added, .. } if added.id == node.id)
        }) {
            Origin::UserLayer
        } else if layout_state.named_nodes.contains(&node.id) {
            Origin::NamedLayout(layout_state.shown_layout.clone())
        } else {
            Origin::Default
        };
        out.push(NodeInfo {
            id: node.id.to_string().into(),
            depth,
            slot: slot.map(|slot| slot.to_string().into()),
            component: node.component.to_string().into(),
            extension: node.extension.clone().into(),
            props: if node.props.is_empty() {
                serde_json::Value::Null
            } else {
                serde_json::Value::Object(node.props.clone())
            },
            hidden: hidden(tree, &node.id),
            origin,
        });
        if node.id.as_str().contains('@') {
            return;
        }
        for (slot, children) in tree.slots(&node.id) {
            for child in children {
                visit(layout_state, tree, child, depth + 1, Some(slot), out);
            }
            if let Some(copies) = layout_state
                .item_children
                .borrow()
                .get(&(node.id.clone(), slot.to_string()))
            {
                for id in copies {
                    if let Some(copy) = layout_state.copies.get(id) {
                        visit(layout_state, tree, copy, depth + 1, Some(slot), out);
                    }
                }
            }
        }
    }
    let mut nodes = Vec::new();
    if cx.try_global::<Registry>().is_some() {
        let state_handle = window_state(cx);
        let layout_state = state_handle.borrow();
        let tree_handle = layout_state.tree.clone();
        let layers = tree_handle.borrow();
        let tree = layers.current();
        if let Some(root) = layout_state
            .extension_root
            .as_ref()
            .and_then(|id| node_of(tree, id))
        {
            visit(&layout_state, tree, root, 0, None, &mut nodes);
        }
    }
    tracing::trace!(nodes = nodes.len(), "layout inspected");
    nodes
}

/// Observes completed layout changes until the subscription is dropped.
pub fn observe_layout(cx: &mut App, f: impl Fn(&mut App) + 'static) -> Subscription {
    let registry = cx.default_global::<Registry>();
    let id = registry.next_layout_observer;
    registry.next_layout_observer += 1;
    registry
        .layout_observers
        .borrow_mut()
        .insert(id, Rc::new(f));
    let observers = Rc::downgrade(&registry.layout_observers);
    Subscription::new(move || {
        if let Some(observers) = observers.upgrade() {
            observers.borrow_mut().remove(&id);
        }
    })
}

fn notify_layout(cx: &mut App) {
    let observers = cx.global::<Registry>().layout_observers.clone();
    let ids = observers.borrow().keys().copied().collect::<Vec<_>>();
    if ids.is_empty() {
        return;
    }
    tracing::debug!(count = ids.len(), "layout observers");
    // Observer work may target another window; finish this operation's groups first.
    cx.defer(move |cx| {
        for id in ids {
            let callback = observers.borrow().get(&id).cloned();
            if let Some(callback) = callback {
                let registry = cx.global::<Registry>();
                let owner = registry.owner.clone();
                let previous = owner.replace(None);
                let logged = registry.logged_window.clone();
                let previous_logged = logged.replace(None);
                let _restore_owner = WindowScope {
                    owner,
                    previous,
                    _span: None,
                    logged: Some((logged, previous_logged)),
                };
                callback(cx);
            }
        }
    });
}

type NamedLayoutFiles = Rc<dyn Fn(&str) -> PathBuf>;

/// Operations recorded by an extension for a named layout.
#[derive(Clone)]
pub struct NamedLayout {
    extension: String,
    operations: Vec<Result<Operation, String>>,
}

impl NamedLayout {
    pub fn add(&mut self, node: &str, component: &str, parent: &str, slot: &str) {
        self.record_add(node, component, parent, slot, Ok(Props::new()));
    }

    pub fn add_with_props(
        &mut self,
        node: &str,
        component: &str,
        parent: &str,
        slot: &str,
        props: impl serde::Serialize,
    ) {
        self.record_add(node, component, parent, slot, encode_props(props));
    }

    fn record_add(
        &mut self,
        node: &str,
        component: &str,
        parent: &str,
        slot: &str,
        props: Result<Props, String>,
    ) {
        self.operations.push((|| {
            Ok(Operation::Add {
                node: Node {
                    id: NodeId::parse(node).map_err(|error| error.to_string())?,
                    component: ComponentId::parse(component).map_err(|error| error.to_string())?,
                    extension: self.extension.clone(),
                    props: props?,
                },
                parent: operation_id(parent).map_err(|error| error.to_string())?,
                slot: SlotName::parse(slot).map_err(|error| error.to_string())?,
                at: Position::Last,
            })
        })());
    }

    pub fn remove(&mut self, node: &str) {
        self.operations.push(
            operation_id(node)
                .map(|node| Operation::Remove { node })
                .map_err(|error| error.to_string()),
        );
    }

    pub fn move_node(&mut self, node: &str, parent: &str, slot: &str, index: usize) {
        self.operations.push((|| {
            Ok(Operation::Move {
                node: operation_id(node).map_err(|error| error.to_string())?,
                parent: operation_id(parent).map_err(|error| error.to_string())?,
                slot: SlotName::parse(slot).map_err(|error| error.to_string())?,
                at: Position::Index(index),
            })
        })());
    }

    pub fn set_hidden(&mut self, node: &str, hidden: bool) {
        self.operations.push(
            operation_id(node)
                .map(|node| Operation::SetHidden { node, hidden })
                .map_err(|error| error.to_string()),
        );
    }

    pub fn set_props(&mut self, node: &str, props: impl serde::Serialize) {
        self.operations.push((|| {
            Ok(Operation::SetProps {
                node: operation_id(node).map_err(|error| error.to_string())?,
                props: encode_props(props)?,
            })
        })());
    }
}

/// Names the user-layer file for each registered named layout.
pub fn set_named_layout_files(cx: &mut App, file: impl Fn(&str) -> PathBuf + 'static) {
    cx.default_global::<Registry>().named_layout_files = Some(Rc::new(file));
    tracing::debug!("named layout files set");
}

/// Returns the layout shown by the window.
pub fn shown_layout(cx: &App, window: &Window) -> SharedString {
    let _window_scope = window_context(window, cx);
    if no_window(cx) {
        tracing::info!("no window");
        return "yard.default".into();
    }
    try_window_state(cx).map_or_else(
        || "yard.default".into(),
        |state_handle| {
            let layout_state = state_handle.borrow();
            layout_state.shown_layout.clone()
        },
    )
}

fn layout_path(registry: &Registry, layout_state: &WindowState) -> Option<PathBuf> {
    if !layout_state.main {
        return None;
    }
    if layout_state.shown_layout.as_str() == "yard.default" {
        registry.layout_file.clone()
    } else {
        registry
            .named_layout_files
            .as_ref()
            .map(|file| file(&layout_state.shown_layout))
    }
}

fn replay_operation(cx: &mut App, extension: &str, operation: Operation) -> bool {
    let node = match &operation {
        Operation::Add { node, .. } => node.id.clone(),
        Operation::Remove { node }
        | Operation::Move { node, .. }
        | Operation::SetHidden { node, .. }
        | Operation::SetProps { node, .. } => node.clone(),
    };
    if let Err(reason) = shell_rule_error(extension, cx, &operation) {
        tracing::info!(node = %node, reason, "skipped");
        return false;
    }
    if let Err(error) = window_state(cx).borrow().tree.borrow_mut().apply(operation) {
        tracing::info!(node = %node, reason = %error, "skipped");
        return false;
    }
    true
}

/// A placement that needs both props and a position. `props` is `Null` for
/// no props, or an object.
pub struct Placement {
    pub props: serde_json::Value,
    pub at: Position,
}

/// The sizes a component declares, in rems: the smallest it may be made, and
/// optionally the size it starts at along a split.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SizeHints {
    pub min: Size<Rems>,
    pub preferred: Size<Option<Rems>>,
}

/// A component view that can be told updated props.
pub trait PropsView<P>: Render {
    fn set_props(&mut self, props: P, cx: &mut Context<Self>);
}

/// A component view that supplies its tab title and optional icon.
pub trait Titled: Render {
    fn title(&self, cx: &App) -> SharedString;
    fn icon(&self, _cx: &App) -> Option<yard_ui::IconName> {
        None
    }
}

/// The state a node gives to the shell when quitting and receives on creation.
pub trait NodeState: Render {
    fn save_state(&self, cx: &App) -> serde_json::Value;
    fn restore_state(
        &mut self,
        state: serde_json::Value,
        window: &mut Window,
        cx: &mut Context<Self>,
    );
}

#[derive(Clone, Copy)]
struct StateRegistration {
    view_type: TypeId,
    save: fn(&AnyView, &App) -> serde_json::Value,
    restore: fn(&AnyView, serde_json::Value, &mut Window, &mut App),
}

type TitleReader = Rc<dyn Fn(&AnyView, &App) -> Option<(SharedString, Option<yard_ui::IconName>)>>;

/// A component view that can say it has nothing to show now.
pub trait Emptiable: Render {
    fn is_empty(&self, cx: &App) -> bool;
}

type EmptyReader = Rc<dyn Fn(&AnyView, &App) -> Option<bool>>;

type ActionHandler = Rc<dyn Fn(Div) -> Div>;
type Factory = Rc<dyn Fn(&Node, Slots, &mut Window, &mut App) -> AnyView>;
type Teller = Rc<dyn Fn(&Props, &mut App)>;
type ContextFactory = Rc<dyn Fn(&NodeId, &mut App) -> AnyEntity>;

#[derive(Clone)]
struct Provider {
    model: TypeId,
    name: &'static str,
    create: ContextFactory,
}

/// A model whose stable slug keys define the contents of an item slot.
pub trait Items: 'static {
    fn items(&self) -> Vec<SharedString>;
}

/// An item model whose tabs can be closed and added. Both only ask: what
/// the model does shows when it notifies.
pub trait ItemTabs: Items {
    fn close_item(&mut self, key: &SharedString, window: &mut Window, cx: &mut Context<Self>)
    where
        Self: Sized;
    fn new_item(&mut self, window: &mut Window, cx: &mut Context<Self>)
    where
        Self: Sized;
}

type ItemReader = Rc<dyn Fn(&AnyEntity, &App) -> Vec<SharedString>>;
type ItemCloser = Rc<dyn Fn(&AnyEntity, &SharedString, &mut Window, &mut App)>;
type ItemMaker = Rc<dyn Fn(&AnyEntity, &mut Window, &mut App)>;
/// What a tabs container with item tabs asks its model.
#[derive(Clone)]
struct ItemTabsHooks {
    model: TypeId,
    name: &'static str,
    close: ItemCloser,
    new: ItemMaker,
}
/// The group of an item tab's button while the click is handled, including
/// the effects GPUI runs right after it.
struct ItemGroup {
    serial: u64,
    span: tracing::Span,
    model: gpui::EntityId,
}
type ItemObserver = Rc<dyn Fn(&AnyEntity, &mut Window, &mut App) -> Subscription>;
#[derive(Clone)]
struct ItemDeclaration {
    model: TypeId,
    name: &'static str,
    read: ItemReader,
    observe: ItemObserver,
}
#[derive(Default)]
struct ItemState {
    keys: Vec<SharedString>,
    warned: BTreeSet<String>,
}
type ItemSubscriptions = BTreeMap<(NodeId, String), (gpui::EntityId, Subscription)>;
type ItemWindow = (gpui::AnyWindowHandle, Weak<RefCell<ItemSubscriptions>>);

type ItemChildren = BTreeMap<(NodeId, String), Vec<NodeId>>;

type ViewMap = BTreeMap<NodeId, AnyView>;

const FRAME_NODES: &[&str] = &["window", "title_bar", "body", "status_bar"];
const FRAME_COMPONENTS: &[&str] = &[
    "yard.window",
    "yard.title_bar",
    "yard.body",
    "yard.status_bar",
];

fn is_frame_node(id: &NodeId) -> bool {
    FRAME_NODES.contains(&id.as_str())
}

fn is_frame_component(id: &ComponentId) -> bool {
    FRAME_COMPONENTS.contains(&id.as_str())
}

fn is_frame_slot(parent: &NodeId, slot: &SlotName) -> bool {
    matches!(
        (parent.as_str(), slot.as_str()),
        ("window", "title_bar" | "body" | "status_bar") | ("body", "children")
    )
}

fn is_bar_item(tree: &Tree, id: &NodeId) -> bool {
    current_location(tree, id)
        .is_some_and(|(parent, _, _, _)| matches!(parent.as_str(), "title_bar" | "status_bar"))
}

fn is_in_body(tree: &Tree, id: &NodeId) -> bool {
    let mut current = id.clone();
    while let Some((parent, _, _, _)) = current_location(tree, &current) {
        if parent.as_str() == "body" {
            return true;
        }
        current = parent;
    }
    false
}

struct WindowFrame {
    slots: Slots,
}

impl Render for WindowFrame {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        yard_ui::v_flex()
            .size_full()
            .children(self.slots.views("title_bar"))
            .children(self.slots.views("body").into_iter().map(|view| {
                div()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .grid()
                    .grid_cols(1)
                    .grid_rows(1)
                    .child(view)
            }))
            .children(self.slots.views("status_bar"))
    }
}

struct TitleBarFrame {
    node: NodeId,
    slots: Slots,
}

impl Render for TitleBarFrame {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let _window_scope = window_context(window, cx);
        let left = self.slots.views("left");
        let center = self.slots.views("center");
        let right = self.slots.views("right");
        let title_shown = left.is_empty();
        tracing::trace!(
            left = left.len(),
            center = center.len(),
            right = right.len(),
            title = title_shown,
            "rendering title bar",
        );
        let window_title = cx
            .try_global::<Registry>()
            .map(|registry| registry.window_title.clone())
            .unwrap_or_default();
        let mut bar = yard_ui::TitleBar::new(SharedString::from(self.node.to_string()));
        if title_shown {
            let selector_title = window_title.clone();
            bar = bar.left(
                div()
                    .debug_selector(move || format!("WINDOW-TITLE-{selector_title}"))
                    .child(
                        yard_ui::Label::new(window_title)
                            .size(yard_ui::LabelSize::Small)
                            .color(yard_ui::TextColor::Muted),
                    ),
            );
        } else {
            for view in left {
                bar = bar.left(view);
            }
        }
        for view in center {
            bar = bar.center(view);
        }
        for view in right {
            bar = bar.right(view);
        }
        bar
    }
}

struct StatusBarFrame {
    node: NodeId,
    slots: Slots,
}

impl Render for StatusBarFrame {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let left = self.slots.views("left");
        let right = self.slots.views("right");
        if left.is_empty() && right.is_empty() {
            return div().into_any_element();
        }
        tracing::trace!(
            left = left.len(),
            right = right.len(),
            "rendering status bar"
        );
        let mut bar = yard_ui::StatusBar::new(SharedString::from(self.node.to_string()));
        for view in left {
            bar = bar.left(view);
        }
        for view in right {
            bar = bar.right(view);
        }
        bar.into_any_element()
    }
}

struct BodyFrame {
    slots: Slots,
}

impl Render for BodyFrame {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let _window_scope = window_context(window, cx);
        let zoomed_view = try_window_state(cx).and_then(|state_handle| {
            let layout_state = state_handle.borrow();
            layout_state
                .zoomed
                .as_ref()
                .and_then(|node| layout_state.views.borrow().get(node).cloned())
        });
        let views = match zoomed_view {
            Some(view) => vec![view],
            None => self.slots.views("children"),
        };
        div()
            .size_full()
            .grid()
            .grid_cols(1)
            .grid_rows(1)
            .children(views)
    }
}

/// The node a toggle shows and hides, read from its `node` prop.
#[derive(Clone, Debug, PartialEq)]
struct ToggleNode(NodeId);

impl serde::Serialize for ToggleNode {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.0.as_str())
    }
}

impl<'de> serde::Deserialize<'de> for ToggleNode {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let name = <String as serde::Deserialize>::deserialize(deserializer)?;
        NodeId::parse(&name)
            .map(ToggleNode)
            .map_err(|error| serde::de::Error::custom(format!("invalid node ID {name:?}: {error}")))
    }
}

/// An icon of the UI kit, named in snake case, as `info` or `x_circle`.
#[derive(Clone, Copy, Debug, PartialEq)]
struct ToggleIcon(yard_ui::IconName);

impl ToggleIcon {
    fn name(self) -> &'static str {
        self.0
            .path()
            .trim_start_matches("icons/")
            .trim_end_matches(".svg")
    }
}

impl serde::Serialize for ToggleIcon {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.name())
    }
}

impl<'de> serde::Deserialize<'de> for ToggleIcon {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let name = <String as serde::Deserialize>::deserialize(deserializer)?;
        yard_ui::IconName::ALL
            .iter()
            .map(|icon| ToggleIcon(*icon))
            .find(|icon| icon.name() == name)
            .ok_or_else(|| serde::de::Error::custom(format!("unknown icon {name:?}")))
    }
}

/// The props of `yard.toggle`.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
struct ToggleProps {
    node: Option<ToggleNode>,
    icon: Option<ToggleIcon>,
    tooltip: Option<String>,
    alone: bool,
}

/// Why a toggle cannot show or hide its node.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ToggleProblem {
    Missing,
    Itself,
    Ancestor,
}

/// Whether a toggle's node is hidden, or why the toggle cannot act on it.
fn toggle_target(tree: &Tree, toggle: &NodeId, target: &NodeId) -> Result<bool, ToggleProblem> {
    if toggle == target {
        return Err(ToggleProblem::Itself);
    }
    if node_of(tree, target).is_none() {
        return Err(ToggleProblem::Missing);
    }
    let mut current = toggle.clone();
    while let Some((parent, _, _, _)) = current_location(tree, &current) {
        if &parent == target {
            return Err(ToggleProblem::Ancestor);
        }
        current = parent;
    }
    Ok(tree.is_hidden(target))
}

/// The component `yard.toggle`: a button that shows or hides the node its
/// props name, as Zed's `PanelButtons` show and hide its docks' panels.
struct Toggle {
    node: NodeId,
    props: ToggleProps,
    /// The problem last logged and the node it names, so that a problem is
    /// logged again only when it changes.
    warned: Option<(ToggleProblem, NodeId)>,
}

impl Render for Toggle {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let _window_scope = window_context(window, cx);
        let (Some(ToggleNode(target)), Some(ToggleIcon(icon))) =
            (self.props.node.clone(), self.props.icon)
        else {
            return div().into_any_element();
        };
        let state = try_window_state(cx).map_or(Err(ToggleProblem::Missing), |state_handle| {
            let layout_state = state_handle.borrow();
            toggle_target(layout_state.tree.borrow().current(), &self.node, &target)
        });
        let toggled = match state {
            Ok(hidden) => !hidden && !empty_now(cx, &target, Emptiness::Ask),
            Err(problem) => {
                let message = match problem {
                    ToggleProblem::Missing => format!(
                        "toggle {} names node {target}, which is not in the tree; it is disabled",
                        self.node
                    ),
                    ToggleProblem::Itself => {
                        format!("toggle {} names itself; it is disabled", self.node)
                    }
                    ToggleProblem::Ancestor => format!(
                        "toggle {} names node {target}, which is above it; it is disabled",
                        self.node
                    ),
                };
                let warned = Some((problem, target.clone()));
                if self.warned != warned {
                    self.warned = warned;
                    log::warn!("{message}");
                }
                tracing::trace!("{message}");
                false
            }
        };
        let tooltip = self
            .props
            .tooltip
            .clone()
            .map(SharedString::from)
            .unwrap_or_else(|| tab_title(&target, cx).0);
        tracing::trace!(
            toggle = %self.node,
            node = %target,
            toggled,
            disabled = state.is_err(),
            "rendering toggle"
        );
        let toggle = self.node.clone();
        yard_ui::IconButton::new(SharedString::from(format!("toggle:{}", self.node)), icon)
            .icon_size(yard_ui::IconSize::Small)
            .toggle_state(toggled)
            .disabled(state.is_err())
            .tooltip(yard_ui::Tooltip::text(tooltip))
            .on_click(move |window, cx| {
                let _window = window_scope(window, cx);
                click_toggle(cx, &toggle)
            })
            .into_any_element()
    }
}

/// Shows a toggle's node when it is hidden, first hiding the other shown
/// nodes of its slot with `alone`, and hides it when it is shown.
fn click_toggle(cx: &mut App, toggle: &NodeId) {
    let _window_scope = selected_scope(cx);
    let Some((target, alone, hidden, others)) = (|| {
        let state_handle = window_state(cx);
        let layout_state = state_handle.borrow();
        let props = layout_state
            .views
            .borrow()
            .get(toggle)?
            .clone()
            .downcast::<NodeView>()
            .ok()?
            .read(cx)
            .view
            .clone()
            .downcast::<Toggle>()
            .ok()?
            .read(cx)
            .props
            .clone();
        let target = props.node?.0;
        let tree_handle = layout_state.tree.clone();
        let layers = tree_handle.borrow();
        let tree = layers.current();
        let hidden = toggle_target(tree, toggle, &target).ok()?;
        let others: Vec<NodeId> = if props.alone && hidden {
            current_location(tree, &target)
                .and_then(|(parent, slot, _, _)| {
                    tree.slots(&parent)
                        .find(|(name, _)| **name == slot)
                        .map(|(_, children)| {
                            children
                                .iter()
                                .filter(|child| child.id != target && !tree.is_hidden(&child.id))
                                .map(|child| child.id.clone())
                                .collect()
                        })
                })
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        Some((target, props.alone, hidden, others))
    })() else {
        return;
    };
    let _group = tracing::info_span!(
        "toggle",
        toggle = toggle.as_str(),
        node = target.as_str(),
        shown = hidden,
    )
    .entered();
    tracing::debug!(alone, others = others.len(), "toggle clicked");
    for other in others {
        run_op("yard", None, cx, "hide", other.as_str(), |_, node| {
            Ok(Operation::SetHidden { node, hidden: true })
        });
    }
    let op = if hidden { "show" } else { "hide" };
    run_op("yard", None, cx, op, target.as_str(), |_, node| {
        Ok(Operation::SetHidden {
            node,
            hidden: !hidden,
        })
    });
}

struct NodeView {
    node: NodeId,
    handle: Option<FocusHandle>,
    view: AnyView,
    zone: Option<DropZone>,
}

impl Render for NodeView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let _window_scope = window_context(window, cx);
        let node = self.node.clone();
        let in_bar = try_window_state(cx).is_some_and(|state_handle| {
            let layout_state = state_handle.borrow();
            let tree_handle = layout_state.tree.clone();
            let layers = tree_handle.borrow();
            is_bar_item(layers.current(), &self.node)
        });
        let container = if matches!(self.node.as_str(), "title_bar" | "status_bar") {
            div().relative().w_full().flex_none()
        } else if in_bar {
            yard_ui::h_flex().relative().h_full().flex_shrink_0()
        } else {
            div()
                .relative()
                .size_full()
                .grid()
                .grid_cols(1)
                .grid_rows(1)
        };
        let container = if let Some(handle) = &self.handle {
            container
                .track_focus(handle)
                .on_action(cx.listener(|view, _: &ToggleZoom, window, cx| {
                    let _window_scope = window_scope(window, cx);
                    layout_action(LayoutAction::ToggleZoom, Some(&view.node), window, cx);
                }))
                .on_action(cx.listener(|view, _: &Hide, window, cx| {
                    let _window_scope = window_scope(window, cx);
                    layout_action(LayoutAction::Hide, Some(&view.node), window, cx);
                }))
                .on_action(cx.listener(|view, _: &SelectNextTab, window, cx| {
                    let _window_scope = window_scope(window, cx);
                    layout_action(LayoutAction::SelectNextTab, Some(&view.node), window, cx);
                }))
                .on_action(cx.listener(|view, _: &FocusLeft, window, cx| {
                    let _window_scope = window_scope(window, cx);
                    layout_action(LayoutAction::FocusLeft, Some(&view.node), window, cx);
                }))
                .on_action(cx.listener(|view, _: &FocusRight, window, cx| {
                    let _window_scope = window_scope(window, cx);
                    layout_action(LayoutAction::FocusRight, Some(&view.node), window, cx);
                }))
                .on_action(cx.listener(|view, _: &FocusUp, window, cx| {
                    let _window_scope = window_scope(window, cx);
                    layout_action(LayoutAction::FocusUp, Some(&view.node), window, cx);
                }))
                .on_action(cx.listener(|view, _: &FocusDown, window, cx| {
                    let _window_scope = window_scope(window, cx);
                    layout_action(LayoutAction::FocusDown, Some(&view.node), window, cx);
                }))
                .on_action(cx.listener(|view, _: &SelectPreviousTab, window, cx| {
                    let _window_scope = window_scope(window, cx);
                    layout_action(
                        LayoutAction::SelectPreviousTab,
                        Some(&view.node),
                        window,
                        cx,
                    );
                }))
        } else {
            container
        };
        if !cx.has_active_drag() {
            self.zone = None;
        }
        let mut container = container
            .id(SharedString::from(format!("drop:{}", self.node)))
            .on_drag_move::<DraggedNode>(cx.listener(|view, event: &gpui::DragMoveEvent<DraggedNode>, window, cx| {
                let _window_scope = window_context(window, cx);
                let next = drop_target_at(cx, event.event.position)
                    .filter(|target| target == &view.node)
                    .and_then(|_| zone_at(event.bounds, event.event.position));
                if next != view.zone {
                    view.zone = next;
                    let target = drop_target_name(cx, &view.node);
                    tracing::trace!(target = %target, zone = next.map_or("none", DropZone::name), "drop zone");
                    cx.notify();
                }
            }));
        if let Some(zone) = self.zone {
            container = container.on_drop::<DraggedNode>(cx.listener(
                move |view, dragged: &DraggedNode, window, cx| {
                    let _window_scope = window_scope(window, cx);
                    view.zone = None;
                    cx.notify();
                    drop_on_zone(cx, window, &dragged.node, &view.node, zone);
                },
            ));
        }
        container
            .child(self.view.clone())
            .child(
                canvas(
                    move |bounds, window, cx| {
                        let _window_scope = window_context(window, cx);
                        window_state(cx)
                            .borrow_mut()
                            .bounds
                            .insert(node.clone(), bounds);
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .top_0()
                .left_0()
                .size_full(),
            )
            .children(
                self.zone
                    .map(|zone| zone_overlay(zone, yard_ui::palette(cx).drop_target)),
            )
    }
}

#[derive(Clone)]
struct DraggedNode {
    node: NodeId,
    label: SharedString,
    icon: Option<yard_ui::IconName>,
}

impl Render for DraggedNode {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let _window_scope = window_context(window, cx);
        let palette = yard_ui::palette(cx);
        div()
            .px_3()
            .py_1()
            .bg(palette.background)
            .border_1()
            .border_color(palette.border)
            .text_color(palette.text)
            .font(yard_ui::ui_font(cx))
            .child(
                yard_ui::h_flex()
                    .gap_1()
                    .children(self.icon.map(|icon| {
                        yard_ui::Icon::new(icon)
                            .size(yard_ui::IconSize::Small)
                            .color(yard_ui::TextColor::Muted)
                    }))
                    .child(yard_ui::Label::new(self.label.clone())),
            )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Edge {
    Before,
    After,
}

#[derive(Clone)]
struct Component {
    slots: Vec<SlotName>,
    create: Factory,
}

/// A node's shown children, read from the tree and view map at call time.
#[derive(Clone)]
pub struct Slots {
    node: NodeId,
    tree: Rc<RefCell<Layers>>,
    views: Rc<RefCell<ViewMap>>,
    items: Rc<RefCell<ItemChildren>>,
}

impl Slots {
    pub fn nodes(&self, slot: &str) -> Vec<NodeId> {
        let layers = self.tree.borrow();
        let tree = layers.current();
        let views = self.views.borrow();
        slot_nodes(tree, &self.items.borrow(), &self.node, slot)
            .into_iter()
            .filter(|node| !hidden(tree, node) && views.contains_key(node))
            .collect()
    }

    pub fn views(&self, slot: &str) -> Vec<AnyView> {
        self.nodes(slot)
            .iter()
            .filter_map(|node| self.views.borrow().get(node).cloned())
            .collect()
    }
}

fn template_id(node: &NodeId) -> NodeId {
    node.as_str().split_once('@').map_or_else(
        || node.clone(),
        |(template, _)| NodeId::parse(template).expect("copy template ID"),
    )
}

fn hidden(tree: &Tree, node: &NodeId) -> bool {
    tree.is_hidden(&template_id(node))
}

fn slot_nodes(tree: &Tree, items: &ItemChildren, parent: &NodeId, slot: &str) -> Vec<NodeId> {
    if let Some(copies) = items.get(&(parent.clone(), slot.into())) {
        return copies.clone();
    }
    // A copy is the template alone; descendants are not copied, so a container copy shows its slots empty.
    tree.slots(parent)
        .find(|(name, _)| name.as_str() == slot)
        .map(|(_, children)| children.iter().map(|child| child.id.clone()).collect())
        .unwrap_or_default()
}

type WindowOwner = Rc<Cell<Option<gpui::WindowId>>>;

struct Registry {
    layout_observers: LayoutObservers,
    next_layout_observer: u64,
    extensions: BTreeSet<String>,
    urls: UrlRouting,
    components: BTreeMap<ComponentId, Component>,
    action_handlers: Vec<ActionHandler>,
    states: BTreeMap<ComponentId, StateRegistration>,
    titles: BTreeMap<ComponentId, TitleReader>,
    required: BTreeSet<ComponentId>,
    slots_for_one: Vec<(ComponentId, SlotName)>,
    item_slots: BTreeMap<(NodeId, String), ItemDeclaration>,
    item_tabs: BTreeMap<NodeId, ItemTabsHooks>,
    providers: BTreeMap<ComponentId, Vec<Provider>>,
    window_contexts: BTreeMap<TypeId, AnyEntity>,
    requirements: BTreeMap<ComponentId, Vec<(TypeId, &'static str)>>,
    empties: BTreeMap<ComponentId, EmptyReader>,
    window_title: SharedString,
    layout_file: Option<PathBuf>,
    named_layout_files: Option<NamedLayoutFiles>,
    named_layouts: BTreeMap<String, NamedLayout>,
    size_hints: BTreeMap<ComponentId, SizeHints>,
    default: Rc<RefCell<WindowState>>,
    windows: BTreeMap<gpui::WindowId, Weak<RefCell<WindowState>>>,
    owner: WindowOwner,
    logged_window: WindowOwner,
    active_history: RefCell<Vec<gpui::WindowId>>,
    ever_opened: bool,
    window_opener: Option<WindowOpener>,
    warned_window_opener: bool,
}

struct WindowState {
    pending_states: WindowStates,
    state_errors: RefCell<BTreeSet<NodeId>>,
    main: bool,
    named_nodes: BTreeSet<NodeId>,
    item_states: BTreeMap<(NodeId, String), ItemState>,
    item_children: Rc<RefCell<ItemChildren>>,
    item_window: Option<ItemWindow>,
    item_group: Option<ItemGroup>,
    item_group_serial: u64,
    item_new_pending: BTreeSet<NodeId>,
    failed_views: BTreeSet<NodeId>,
    copies: BTreeMap<NodeId, Node>,
    contexts: BTreeMap<NodeId, BTreeMap<TypeId, AnyEntity>>,
    title_errors: RefCell<BTreeSet<NodeId>>,
    empty_errors: RefCell<BTreeSet<NodeId>>,
    emptiness: RefCell<BTreeMap<NodeId, bool>>,
    laid_out: RefCell<BTreeMap<NodeId, (bool, bool)>>,
    tree: Rc<RefCell<Layers>>,
    views: Rc<RefCell<ViewMap>>,
    tellers: BTreeMap<NodeId, Teller>,
    focus_handles: BTreeMap<NodeId, FocusHandle>,
    bounds: BTreeMap<NodeId, Bounds<Pixels>>,
    root_focus_handle: Option<FocusHandle>,
    splits: Vec<WeakEntity<Split>>,
    tabs: Vec<WeakEntity<Tabs>>,
    extension_root: Option<NodeId>,
    zoomed: Option<NodeId>,
    shown_layout: SharedString,
    saved_layers: BTreeMap<String, Vec<Operation>>,
    /// The session file of a window opened from the session, and the named
    /// layouts whose user layers the session gave it: a switch to one of
    /// them uses that layer, not the layout's file.
    session_layers: Option<(PathBuf, BTreeSet<String>)>,
    startup_tree: Tree,
    warned_named_files: bool,
    views_created: bool,
    default_fixed: bool,
    splitting: bool,
    cycle_drop: Option<NodeId>,
    /// The zoom saved at the last run, given once the window's views exist:
    /// `Some(None)` is a saved `null`.
    given_zoom: Option<Option<String>>,
}

impl Default for Registry {
    fn default() -> Self {
        let mut components: BTreeMap<_, _> = [
            ("yard.row", Axis::Horizontal),
            ("yard.column", Axis::Vertical),
        ]
        .into_iter()
        .map(|(name, axis)| {
            (
                ComponentId::parse(name).unwrap(),
                Component {
                    slots: vec![SlotName::parse("children").unwrap()],
                    create: Rc::new(move |node, slots, _, cx| {
                        let (shares, sizes) = read_sizes(&node.id, &node.component, &node.props);
                        let rem_size = yard_ui::ui_font_size(cx);
                        let split = cx.new(|_| Split {
                            node: node.id.clone(),
                            axis,
                            slots,
                            shares,
                            sizes,
                            shown_nodes: Vec::new(),
                            shown_shares: Rc::new(RefCell::new(Vec::new())),
                            shown_given: Vec::new(),
                            rem_size,
                        });

                        let state_handle = window_state(cx);
                        let mut layout_state = state_handle.borrow_mut();
                        layout_state.splits.push(split.downgrade());
                        let weak = split.downgrade();
                        let tell_node = node.id.clone();
                        let tell_component = node.component.clone();
                        let teller: Teller = Rc::new(move |props, cx| {
                            let Some(split) = weak.upgrade() else {
                                return;
                            };
                            let (shares, sizes) = read_sizes(&tell_node, &tell_component, props);
                            split.update(cx, |split, cx| {
                                split.set_sizes(shares, sizes, cx);
                                cx.notify();
                            });
                            tracing::debug!(node = %tell_node, "view told");
                        });
                        layout_state.tellers.insert(node.id.clone(), teller);
                        split.into()
                    }),
                },
            )
        })
        .collect();
        components.insert(
            ComponentId::parse("yard.tabs").unwrap(),
            Component {
                slots: vec![SlotName::parse("children").unwrap()],
                create: Rc::new(|node, slots, _, cx| {
                    let tabs = cx.new(|_| Tabs {
                        node: node.id.clone(),
                        slots,
                        selected: None,
                        reselect: false,
                        bar_bounds: None,
                        scroll: ScrollHandle::new(),
                        scrolled_to: None,
                        seen_offset: Point::default(),
                        pending: None,
                    });
                    window_state(cx).borrow_mut().tabs.push(tabs.downgrade());
                    tabs.into()
                }),
            },
        );
        components.insert(
            ComponentId::parse("yard.window").unwrap(),
            Component {
                slots: vec![
                    SlotName::parse("title_bar").unwrap(),
                    SlotName::parse("body").unwrap(),
                    SlotName::parse("status_bar").unwrap(),
                ],
                create: Rc::new(|_, slots, _, cx| cx.new(|_| WindowFrame { slots }).into()),
            },
        );
        components.insert(
            ComponentId::parse("yard.title_bar").unwrap(),
            Component {
                slots: vec![
                    SlotName::parse("left").unwrap(),
                    SlotName::parse("center").unwrap(),
                    SlotName::parse("right").unwrap(),
                ],
                create: Rc::new(|node, slots, _, cx| {
                    cx.new(|_| TitleBarFrame {
                        node: node.id.clone(),
                        slots,
                    })
                    .into()
                }),
            },
        );
        components.insert(
            ComponentId::parse("yard.body").unwrap(),
            Component {
                slots: vec![SlotName::parse("children").unwrap()],
                create: Rc::new(|_, slots, _, cx| cx.new(|_| BodyFrame { slots }).into()),
            },
        );
        components.insert(
            ComponentId::parse("yard.status_bar").unwrap(),
            Component {
                slots: vec![
                    SlotName::parse("left").unwrap(),
                    SlotName::parse("right").unwrap(),
                ],
                create: Rc::new(|node, slots, _, cx| {
                    cx.new(|_| StatusBarFrame {
                        node: node.id.clone(),
                        slots,
                    })
                    .into()
                }),
            },
        );
        components.insert(
            ComponentId::parse("yard.toggle").unwrap(),
            Component {
                slots: Vec::new(),
                create: Rc::new(|node, _, _, cx| {
                    let props = read_props::<ToggleProps>(&node.id, &node.component, &node.props);
                    let toggle = cx.new(|_| Toggle {
                        node: node.id.clone(),
                        props,
                        warned: None,
                    });
                    let weak = toggle.downgrade();
                    let tell_node = node.id.clone();
                    let tell_component = node.component.clone();
                    let teller: Teller = Rc::new(move |props, cx| {
                        let Some(toggle) = weak.upgrade() else {
                            return;
                        };
                        let props = read_props::<ToggleProps>(&tell_node, &tell_component, props);
                        toggle.update(cx, |toggle, cx| {
                            toggle.props = props;
                            cx.notify();
                        });
                        tracing::debug!(node = %tell_node, "view told");
                    });
                    window_state(cx)
                        .borrow_mut()
                        .tellers
                        .insert(node.id.clone(), teller);
                    toggle.into()
                }),
            },
        );
        let mut tree = Tree::default();
        {
            let _frame = tracing::info_span!("window-frame").entered();
            let window_id = NodeId::parse("window").unwrap();
            {
                let _place =
                    tracing::info_span!("root-place", node = "window", component = "yard.window",)
                        .entered();
                let _ = tree.place_root(Node {
                    id: window_id.clone(),
                    component: ComponentId::parse("yard.window").unwrap(),
                    extension: "yard".into(),
                    props: Props::new(),
                });
            }
            {
                let _place = tracing::info_span!(
                    "node-place",
                    node = "title_bar",
                    component = "yard.title_bar",
                    parent = "window",
                    slot = "title_bar",
                )
                .entered();
                let _ = tree.place(
                    Node {
                        id: NodeId::parse("title_bar").unwrap(),
                        component: ComponentId::parse("yard.title_bar").unwrap(),
                        extension: "yard".into(),
                        props: Props::new(),
                    },
                    &window_id,
                    SlotName::parse("title_bar").unwrap(),
                );
            }
            {
                let _place = tracing::info_span!(
                    "node-place",
                    node = "body",
                    component = "yard.body",
                    parent = "window",
                    slot = "body",
                )
                .entered();
                let _ = tree.place(
                    Node {
                        id: NodeId::parse("body").unwrap(),
                        component: ComponentId::parse("yard.body").unwrap(),
                        extension: "yard".into(),
                        props: Props::new(),
                    },
                    &window_id,
                    SlotName::parse("body").unwrap(),
                );
            }
            {
                let _place = tracing::info_span!(
                    "node-place",
                    node = "status_bar",
                    component = "yard.status_bar",
                    parent = "window",
                    slot = "status_bar"
                )
                .entered();
                let _ = tree.place(
                    Node {
                        id: NodeId::parse("status_bar").unwrap(),
                        component: ComponentId::parse("yard.status_bar").unwrap(),
                        extension: "yard".into(),
                        props: Props::new(),
                    },
                    &window_id,
                    SlotName::parse("status_bar").unwrap(),
                );
            }
        }
        Self {
            layout_observers: Rc::default(),
            next_layout_observer: 0,
            extensions: BTreeSet::new(),
            urls: UrlRouting::default(),
            components,
            action_handlers: Vec::new(),
            states: BTreeMap::new(),
            titles: BTreeMap::new(),
            required: BTreeSet::new(),
            slots_for_one: Vec::new(),
            item_slots: BTreeMap::new(),
            item_tabs: BTreeMap::new(),
            providers: BTreeMap::new(),
            window_contexts: BTreeMap::new(),
            requirements: BTreeMap::new(),
            empties: BTreeMap::new(),
            window_title: SharedString::default(),
            layout_file: None,
            named_layout_files: None,
            named_layouts: BTreeMap::new(),
            size_hints: BTreeMap::new(),
            default: Rc::new(RefCell::new(WindowState::new(tree))),
            windows: BTreeMap::new(),
            owner: Rc::new(Cell::new(None)),
            logged_window: Rc::new(Cell::new(None)),
            active_history: RefCell::new(Vec::new()),
            ever_opened: false,
            window_opener: None,
            warned_window_opener: false,
        }
    }
}
impl Global for Registry {}

impl WindowState {
    fn new(tree: Tree) -> Self {
        Self {
            pending_states: WindowStates::new(),
            state_errors: RefCell::new(BTreeSet::new()),
            main: false,
            named_nodes: BTreeSet::new(),
            item_states: BTreeMap::new(),
            item_children: Rc::default(),
            item_window: None,
            item_group: None,
            item_group_serial: 0,
            item_new_pending: BTreeSet::new(),
            failed_views: BTreeSet::new(),
            copies: BTreeMap::new(),
            contexts: BTreeMap::new(),
            title_errors: RefCell::new(BTreeSet::new()),
            empty_errors: RefCell::new(BTreeSet::new()),
            emptiness: RefCell::new(BTreeMap::new()),
            laid_out: RefCell::new(BTreeMap::new()),
            tree: Rc::new(RefCell::new(Layers::new(tree))),
            views: Rc::new(RefCell::new(BTreeMap::new())),
            tellers: BTreeMap::new(),
            focus_handles: BTreeMap::new(),
            bounds: BTreeMap::new(),
            root_focus_handle: None,
            splits: Vec::new(),
            tabs: Vec::new(),
            extension_root: None,
            zoomed: None,
            shown_layout: "yard.default".into(),
            saved_layers: BTreeMap::new(),
            session_layers: None,
            startup_tree: Tree::default(),
            warned_named_files: false,
            views_created: false,
            default_fixed: false,
            splitting: false,
            cycle_drop: None,
            given_zoom: None,
        }
    }
}

struct WindowScope {
    owner: WindowOwner,
    previous: Option<gpui::WindowId>,
    _span: Option<tracing::span::EnteredSpan>,
    logged: Option<(WindowOwner, Option<gpui::WindowId>)>,
}
impl Drop for WindowScope {
    fn drop(&mut self) {
        self.owner.set(self.previous);
        if let Some((logged, previous)) = &self.logged {
            logged.set(*previous);
        }
    }
}
fn owner_scope(id: gpui::WindowId, cx: &App) -> WindowScope {
    let mut scope = quiet_owner_scope(id, cx);
    if let Some(registry) = cx.try_global::<Registry>() {
        let logged = registry.logged_window.clone();
        let previous = logged.replace(Some(id));
        if previous != Some(id) {
            scope._span = Some(tracing::info_span!("window", id = id.as_u64()).entered());
            scope.logged = Some((logged, previous));
        }
    }
    scope
}
fn quiet_owner_scope(id: gpui::WindowId, cx: &App) -> WindowScope {
    let owner = cx
        .try_global::<Registry>()
        .map(|registry| registry.owner.clone())
        .unwrap_or_default();
    let previous = owner.replace(Some(id));
    WindowScope {
        owner,
        previous,
        _span: None,
        logged: None,
    }
}
fn window_scope(window: &Window, cx: &App) -> WindowScope {
    owner_scope(window.window_handle().window_id(), cx)
}
fn window_context(window: &Window, cx: &App) -> WindowScope {
    quiet_owner_scope(window.window_handle().window_id(), cx)
}
fn remember_active(registry: &Registry, id: gpui::WindowId) {
    let mut history = registry.active_history.borrow_mut();
    if history.last() != Some(&id) {
        history.retain(|known| *known != id);
        history.push(id);
    }
}
fn selected_window(cx: &App) -> Option<gpui::WindowId> {
    let registry = cx.try_global::<Registry>()?;
    if let Some(id) = registry.owner.get() {
        return registry.windows.contains_key(&id).then_some(id);
    }
    if let Some(id) = cx.active_window().map(|window| window.window_id())
        && registry.windows.contains_key(&id)
    {
        remember_active(registry, id);
        return Some(id);
    }
    registry
        .active_history
        .borrow()
        .iter()
        .rev()
        .find(|id| registry.windows.contains_key(id))
        .copied()
}
fn try_window_state(cx: &App) -> Option<Rc<RefCell<WindowState>>> {
    let registry = cx.try_global::<Registry>()?;
    Some(
        selected_window(cx)
            .and_then(|id| registry.windows.get(&id)?.upgrade())
            .unwrap_or_else(|| registry.default.clone()),
    )
}
fn window_state(cx: &App) -> Rc<RefCell<WindowState>> {
    try_window_state(cx).expect("the shell registry is initialized before accessing window state")
}
fn no_window(cx: &App) -> bool {
    cx.try_global::<Registry>()
        .is_some_and(|registry| registry.ever_opened && selected_window(cx).is_none())
}
fn selected_scope(cx: &App) -> Option<WindowScope> {
    selected_window(cx).map(|id| owner_scope(id, cx))
}

// Zed chooses the nearest side, with ties ordered top, right, bottom, left.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DropZone {
    Top,
    Right,
    Bottom,
    Left,
    Center,
}

impl DropZone {
    fn name(self) -> &'static str {
        match self {
            Self::Top => "top",
            Self::Right => "right",
            Self::Bottom => "bottom",
            Self::Left => "left",
            Self::Center => "center",
        }
    }
    fn axis(self) -> Option<Axis> {
        match self {
            Self::Left | Self::Right => Some(Axis::Horizontal),
            Self::Top | Self::Bottom => Some(Axis::Vertical),
            Self::Center => None,
        }
    }
    fn edge(self) -> Edge {
        match self {
            Self::Top | Self::Left => Edge::Before,
            _ => Edge::After,
        }
    }
}

fn zone_at(bounds: Bounds<Pixels>, position: Point<Pixels>) -> Option<DropZone> {
    if !bounds.contains(&position) {
        return None;
    }
    let size = bounds.size.width.min(bounds.size.height) * 0.2;
    let sides = [
        (DropZone::Top, position.y - bounds.top()),
        (DropZone::Right, bounds.right() - position.x),
        (DropZone::Bottom, bounds.bottom() - position.y),
        (DropZone::Left, position.x - bounds.left()),
    ];
    let (zone, distance) = sides
        .into_iter()
        .min_by_key(|(_, distance)| *distance)
        .unwrap();
    Some(if distance < size {
        zone
    } else {
        DropZone::Center
    })
}

fn zone_overlay(zone: DropZone, color: gpui::Hsla) -> Div {
    let overlay = div()
        .debug_selector(move || format!("DROP-OVERLAY-{}", zone.name()))
        .absolute()
        .bg(color);
    match zone {
        DropZone::Left => overlay
            .top_0()
            .left_0()
            .bottom_0()
            .w(DefiniteLength::Fraction(0.5)),
        DropZone::Right => overlay
            .top_0()
            .right_0()
            .bottom_0()
            .w(DefiniteLength::Fraction(0.5)),
        DropZone::Top => overlay
            .top_0()
            .left_0()
            .right_0()
            .h(DefiniteLength::Fraction(0.5)),
        DropZone::Bottom => overlay
            .bottom_0()
            .left_0()
            .right_0()
            .h(DefiniteLength::Fraction(0.5)),
        DropZone::Center => overlay.inset_0(),
    }
}

fn drop_target_at(cx: &App, position: Point<Pixels>) -> Option<NodeId> {
    let state_handle = window_state(cx);
    let layout_state = state_handle.borrow();
    let registry = cx.try_global::<Registry>()?;
    // Tab bars retain their own indexed/append drop behavior.
    let drawn_bar = find_in_drawn(registry, &layout_state, cx, |node| {
        layout_state
            .tabs
            .iter()
            .filter_map(WeakEntity::upgrade)
            .any(|tabs| {
                let tabs = tabs.read(cx);
                &tabs.node == node
                    && tabs
                        .bar_bounds
                        .is_some_and(|bounds| bounds.contains(&position))
            })
            .then_some(())
    })
    .is_some();
    if drawn_bar {
        return None;
    }
    find_in_drawn(registry, &layout_state, cx, |node| {
        let tree_handle = layout_state.tree.clone();
        let layers = tree_handle.borrow();
        let tree = layers.current();
        let (parent, _, _, _) = current_location(tree, node)?;
        let component = component_of(tree, &parent)?.as_str();
        if !matches!(component, "yard.tabs" | "yard.row" | "yard.column") {
            return None;
        }
        let zone = zone_at(*layout_state.bounds.get(node)?, position)?;
        Some((
            node.clone(),
            zone != DropZone::Center || component == "yard.tabs",
        ))
    })
    .and_then(|(node, accepts)| accepts.then_some(node))
}

fn drop_target_name(cx: &App, shown: &NodeId) -> NodeId {
    let state_handle = window_state(cx);
    let layout_state = state_handle.borrow();
    let tree_handle = layout_state.tree.clone();
    let layers = tree_handle.borrow();
    let tree = layers.current();
    current_location(tree, shown)
        .and_then(|(parent, _, _, _)| {
            (component_of(tree, &parent)?.as_str() == "yard.tabs").then_some(parent)
        })
        .unwrap_or_else(|| shown.clone())
}

fn drop_on_zone(cx: &mut App, window: &mut Window, node: &NodeId, shown: &NodeId, zone: DropZone) {
    let _window_scope = window_scope(window, cx);
    if is_copy_id(node.as_str()) || live_copy(cx, node.as_str()) {
        shell_move(cx, node, shown, "children", 0);
        return;
    }
    let (parent, component) = {
        let state_handle = window_state(cx);
        let layout_state = state_handle.borrow();
        let tree_handle = layout_state.tree.clone();
        let layers = tree_handle.borrow();
        let tree = layers.current();
        let Some((parent, _, _, _)) = current_location(tree, shown) else {
            return;
        };
        let Some(component) = component_of(tree, &parent) else {
            return;
        };
        (parent, component.clone())
    };
    if component.as_str() == "yard.tabs" {
        if zone == DropZone::Center {
            if node == shown {
                window_state(cx).borrow_mut().cycle_drop = Some(node.clone());
                shell_move(cx, node, &parent, "children", usize::MAX);
                window_state(cx).borrow_mut().cycle_drop = None;
            } else {
                drop_on_tab_bar(cx, node, &parent);
                let in_target = {
                    let state_handle = window_state(cx);
                    let layout_state = state_handle.borrow();
                    let tree_handle = layout_state.tree.clone();
                    let layers = tree_handle.borrow();
                    current_location(layers.current(), node)
                        .is_some_and(|(current, _, _, _)| current == parent)
                };
                if in_target {
                    let tabs = window_state(cx).borrow().tabs.clone();
                    for tabs in tabs {
                        let _ = tabs.update(cx, |tabs, cx| {
                            if tabs.node == parent {
                                tabs.reselect = false;
                                tabs.select(node.clone(), cx);
                            }
                        });
                    }
                }
            }
        } else {
            if node == &parent {
                tracing::info!("moving a node onto itself would make a cycle");
                refresh_after_drop(cx);
                return;
            }
            split_on_drop(cx, window, node, &parent, shown == node, zone);
        }
    } else {
        let axis = if component.as_str() == "yard.row" {
            Axis::Horizontal
        } else {
            Axis::Vertical
        };
        if zone.axis() == Some(axis) {
            let shown_index = window_state(cx)
                .borrow()
                .splits
                .iter()
                .filter_map(WeakEntity::upgrade)
                .find_map(|split| {
                    let split = split.read(cx);
                    (split.node == parent)
                        .then(|| split.shown_nodes.iter().position(|id| id == shown))
                        .flatten()
                });
            if let Some(index) = shown_index {
                drop_on_edge(cx, node, &parent, index, zone.edge());
            }
        } else {
            split_on_drop(cx, window, node, shown, shown == node, zone);
        }
    }
}

fn split_on_drop(
    cx: &mut App,
    window: &mut Window,
    node: &NodeId,
    target: &NodeId,
    self_drop: bool,
    zone: DropZone,
) {
    let _window_scope = window_scope(window, cx);
    let _group =
        tracing::info_span!("split-on-drop", node = %node, target = %target, edge = zone.name())
            .entered();
    let (parent, slot, index, split, tabs, previous) = {
        let state_handle = window_state(cx);
        let layout_state = state_handle.borrow();
        let tree_handle = layout_state.tree.clone();
        let layers = tree_handle.borrow();
        let tree = layers.current();
        let Some((parent, slot, index, _)) = current_location(tree, target) else {
            return;
        };
        let free = |prefix| {
            (1..)
                .map(|number| NodeId::parse(&format!("{prefix}-{number}")).unwrap())
                .find(|id| node_of(tree, id).is_none())
                .unwrap()
        };
        (
            parent,
            slot,
            index,
            free("split"),
            free("tabs"),
            layers.user_layer().to_vec(),
        )
    };
    let children = SlotName::parse("children").unwrap();
    let operations = [
        (
            "add",
            split.clone(),
            Operation::Add {
                node: Node {
                    id: split.clone(),
                    component: ComponentId::parse(if zone.axis() == Some(Axis::Horizontal) {
                        "yard.row"
                    } else {
                        "yard.column"
                    })
                    .unwrap(),
                    extension: "yard".into(),
                    props: serde_json::json!({"shares": {target.as_str(): 1, tabs.as_str(): 1}})
                        .as_object()
                        .unwrap()
                        .clone(),
                },
                parent,
                slot,
                at: Position::Index(index),
            },
        ),
        (
            "move",
            target.clone(),
            Operation::Move {
                node: target.clone(),
                parent: split.clone(),
                slot: children.clone(),
                at: Position::Last,
            },
        ),
        (
            "add",
            tabs.clone(),
            Operation::Add {
                node: Node {
                    id: tabs.clone(),
                    component: ComponentId::parse("yard.tabs").unwrap(),
                    extension: "yard".into(),
                    props: Props::new(),
                },
                parent: split.clone(),
                slot: children.clone(),
                at: if zone.edge() == Edge::Before {
                    Position::First
                } else {
                    Position::Last
                },
            },
        ),
        (
            "move",
            node.clone(),
            Operation::Move {
                node: node.clone(),
                parent: tabs.clone(),
                slot: children,
                at: Position::Last,
            },
        ),
    ];
    window_state(cx).borrow_mut().splitting = true;
    for (done, (op, id, operation)) in operations.into_iter().enumerate() {
        if self_drop && done == 3 {
            window_state(cx).borrow_mut().cycle_drop = Some(node.clone());
        }
        let before = user_layer_len(cx);
        run_op("yard", Some(window), cx, op, id.as_str(), |_, _| {
            Ok(operation)
        });
        window_state(cx).borrow_mut().cycle_drop = None;
        if user_layer_len(cx) == before {
            // Rebuild from the unchanged startup default and the pre-drop layer;
            // selections, sizes, focus, and the file have not been changed.

            let state_handle = window_state(cx);
            let mut layout_state = state_handle.borrow_mut();
            let mut layers = layout_state.tree.borrow_mut();
            if done > 0 {
                layers.reset();
                for operation in previous {
                    layers
                        .apply(operation)
                        .expect("previously applied operation");
                }
            }
            drop(layers);
            layout_state.splitting = false;
            for id in [&tabs, &split] {
                layout_state.focus_handles.remove(id);
                layout_state.bounds.remove(id);
                layout_state.tellers.remove(id);
                layout_state.contexts.remove(id);
                layout_state.emptiness.borrow_mut().remove(id);
                layout_state.empty_errors.borrow_mut().remove(id);
                layout_state.laid_out.borrow_mut().remove(id);
                let dropped = layout_state.views.borrow_mut().remove(id);
                if dropped.is_some() {
                    drop(dropped);
                    tracing::debug!(node = %id, "view dropped");
                }
            }
            layout_state
                .splits
                .retain(|split| split.upgrade().is_some());
            layout_state.tabs.retain(|tabs| tabs.upgrade().is_some());
            tracing::debug!(done, "split on drop undone");
            drop(layout_state);
            schedule_items(cx, false);
            cx.refresh_windows();
            return;
        }
    }
    window_state(cx).borrow_mut().splitting = false;
    reconcile_containers(cx);
    cx.refresh_windows();
    save(cx);
    schedule_items(cx, true);
}

fn refresh_after_drop(cx: &mut App) {
    let _window_scope = selected_scope(cx);
    cx.refresh_windows();
}

/// The props of a split: its children's shares by node ID.
#[derive(Default, serde::Serialize, serde::Deserialize)]
struct SplitProps {
    shares: BTreeMap<String, serde_json::Value>,
    sizes: BTreeMap<String, serde_json::Value>,
}

/// Reads shares and fixed sizes by child ID, keeping entries for absent children.
/// Invalid shares default to 1; invalid fixed sizes leave the child flexible.
fn read_sizes(
    node: &NodeId,
    component: &ComponentId,
    props: &Props,
) -> (BTreeMap<String, f32>, BTreeMap<String, Pixels>) {
    let props = read_props::<SplitProps>(node, component, props);
    let sizes = props.sizes.into_iter().filter_map(|(child, value)| {
        let size = value.as_f64().map(|size| size as f32).filter(|size| size.is_finite() && *size > 0.);
        match size {
            Some(size) => Some((child, gpui::px(size))),
            None => {
                log::warn!("prop sizes of node {node} cannot be read: the size of {child} is not a positive number; the default is used");
                None
            }
        }
    }).collect();
    let shares = props.shares
        .into_iter()
        .map(|(child, value)| {
            let share = value
                .as_f64()
                .map(|share| share as f32)
                .filter(|share| share.is_finite() && *share > 0.);
            let share = share.unwrap_or_else(|| {
                log::warn!(
                    "prop shares of node {node} cannot be read: the share of {child} is not a positive number; the default is used"
                );
                1.
            });
            (child, share)
        })
        .collect();
    (shares, sizes)
}

/// Records the end of a resize as one set-props on the split, as the shell.
fn record_resize(
    split: &NodeId,
    shown: &[NodeId],
    preferred: &[Option<Pixels>],
    resized: yard_ui::Resized,
    cx: &mut App,
) {
    let _window_scope = selected_scope(cx);
    if shown.iter().any(|node| is_copy_id(node.as_str())) {
        let entity = window_state(cx)
            .borrow()
            .splits
            .iter()
            .filter_map(WeakEntity::upgrade)
            .find(|entity| &entity.read(cx).node == split);
        if let Some(entity) = entity {
            entity.update(cx, |split, cx| {
                match resized {
                    yard_ui::Resized::Reset => {
                        for node in shown {
                            split.shares.remove(node.as_str());
                        }
                    }
                    yard_ui::Resized::Dragged { sizes, .. } => {
                        for (node, size) in shown.iter().zip(sizes) {
                            match size {
                                yard_ui::AxisSize::Flex(share) => {
                                    split.shares.insert(node.to_string(), share);
                                }
                                yard_ui::AxisSize::Fixed(size) => {
                                    split.sizes.insert(node.to_string(), size);
                                }
                            }
                        }
                    }
                }
                split.set_sizes(split.shares.clone(), split.sizes.clone(), cx);
                cx.notify();
            });
        }
        tracing::debug!(node = %split, "item copy sizes kept for this run");
        return;
    }
    let (snapped_shut, snapped_open) = match &resized {
        yard_ui::Resized::Dragged {
            snapped_shut,
            snapped_open,
            ..
        } => (snapped_shut.clone(), snapped_open.clone()),
        yard_ui::Resized::Reset => (Vec::new(), Vec::new()),
    };
    let props = match resized {
        yard_ui::Resized::Reset => serde_json::json!({"shares": null}),
        yard_ui::Resized::Dragged { sizes, .. } => {
            let current = {
                let state_handle = window_state(cx);
                let layout_state = state_handle.borrow();
                let tree_handle = layout_state.tree.clone();
                let layers = tree_handle.borrow();
                node_of(layers.current(), split)
                    .map(|node| node.props.clone())
                    .unwrap_or_default()
            };
            let mut patch = Props::new();
            for key in ["shares", "sizes"] {
                let mut entries = current
                    .get(key)
                    .and_then(serde_json::Value::as_object)
                    .cloned()
                    .unwrap_or_default();
                let before = entries.clone();
                for (index, ((child, size), preferred)) in
                    shown.iter().zip(&sizes).zip(preferred).enumerate()
                {
                    if snapped_shut.contains(&index)
                        || (is_hidden(cx, child.as_str()) && !snapped_open.contains(&index))
                    {
                        continue;
                    }
                    let value = match (key, size) {
                        ("shares", yard_ui::AxisSize::Flex(share)) => Some(*share),
                        // A child still at its preferred size keeps no entry,
                        // so that it follows the UI font.
                        ("sizes", yard_ui::AxisSize::Fixed(pixels))
                            if Some(*pixels) != *preferred =>
                        {
                            Some(f32::from(*pixels))
                        }
                        _ => None,
                    };
                    if let Some(value) = value {
                        let value = value.to_string().parse::<f64>().unwrap_or(1.);
                        entries.insert(child.to_string(), serde_json::json!(value));
                    }
                }
                if entries != before {
                    patch.insert(key.into(), serde_json::Value::Object(entries));
                }
            }
            serde_json::Value::Object(patch)
        }
    };
    if props.as_object().is_some_and(|props| !props.is_empty()) {
        let props = encode_props(props);
        run_op("yard", None, cx, "props", split.as_str(), |_, node| {
            Ok(Operation::SetProps {
                node,
                props: props?,
            })
        });
    }
    for (indices, hidden, op) in [
        (&snapped_shut, true, "hide"),
        (&snapped_open, false, "show"),
    ] {
        for index in indices {
            if let Some(node) = shown.get(*index) {
                run_op("yard", None, cx, op, node.as_str(), |_, node| {
                    Ok(Operation::SetHidden { node, hidden })
                });
            }
        }
    }
}

/// The minimum of a component without size hints: 5 rems across and 6.25
/// rems down.
const DEFAULT_MINIMUM: Size<Rems> = Size {
    width: Rems(5.),
    height: Rems(6.25),
};

/// The smallest size of a node, in pixels at `rem_size`: for a split or a
/// tabs container with shown children, what those children need; for any
/// other node, its own `min` hint or the default. Only the shell's own
/// components are containers, and an extension cannot give hints to them.
fn node_minimum(
    registry: &Registry,
    layout_state: &WindowState,
    tree: &Tree,
    views: &ViewMap,
    node: &NodeId,
    rem_size: Pixels,
    cx: &App,
) -> Size<Pixels> {
    let to_pixels = |size: Size<Rems>| size.map(|rems| rems.to_pixels(rem_size));
    let component = component_of(tree, node).map(ComponentId::as_str);
    let leaf = || {
        component_of(tree, node)
            .and_then(|component| registry.size_hints.get(component))
            .map_or(to_pixels(DEFAULT_MINIMUM), |hints| to_pixels(hints.min))
    };
    if !matches!(component, Some("yard.row" | "yard.column" | "yard.tabs")) {
        return leaf();
    }
    // Each shown child's minimum, computed once.
    let children: Vec<Size<Pixels>> =
        slot_nodes(tree, &layout_state.item_children.borrow(), node, "children")
            .iter()
            .filter(|child| {
                !hidden(tree, child)
                    && views.contains_key(*child)
                    && !node_emptiness(
                        registry,
                        layout_state,
                        tree,
                        views,
                        child,
                        Emptiness::Remembered,
                        cx,
                    )
                    .0
            })
            .map(|child| node_minimum(registry, layout_state, tree, views, child, rem_size, cx))
            .collect();
    if children.is_empty() {
        return to_pixels(DEFAULT_MINIMUM);
    }
    let largest = children
        .iter()
        .fold(Size::default(), |largest: Size<Pixels>, size| {
            largest.max(size)
        });
    match component {
        Some("yard.tabs") => Size {
            width: largest.width,
            height: largest.height + yard_ui::tab_bar_height(cx),
        },
        _ => {
            let axis = if component == Some("yard.row") {
                Axis::Horizontal
            } else {
                Axis::Vertical
            };
            let along: Pixels = children.iter().map(|size| size.along(axis)).sum();
            largest.apply_along(axis, |_| along)
        }
    }
}

struct Split {
    node: NodeId,
    shares: BTreeMap<String, f32>,
    sizes: BTreeMap<String, Pixels>,
    shown_nodes: Vec<NodeId>,
    shown_shares: Rc<RefCell<Vec<yard_ui::AxisChild>>>,
    /// The size each shown child with no entry in the props was given: its
    /// preferred size, or a share of 1.
    shown_given: Vec<Option<yard_ui::AxisSize>>,
    /// The window's rem size at the last layout.
    rem_size: Pixels,
    axis: Axis,
    slots: Slots,
}

impl Split {
    fn reconcile(&mut self, cx: &App, mode: Emptiness<'_>) {
        // The axis mutates this vector during mouse events. Save it against the
        // previous IDs before replacing it with the current shown children.
        let mut carried = BTreeMap::new();
        for ((node, share), given) in self
            .shown_nodes
            .iter()
            .zip(self.shown_shares.borrow().iter())
            .zip(&self.shown_given)
        {
            match (given, &share.size) {
                (None, yard_ui::AxisSize::Flex(share)) => {
                    self.shares.insert(node.to_string(), *share);
                }
                (None, yard_ui::AxisSize::Fixed(size)) => {
                    self.sizes.insert(node.to_string(), *size);
                }
                // A child with no entry keeps no entry: a drag that is
                // changing it carries its size to the next frame, until the
                // drag's end records it in the props.
                (Some(given), size) => {
                    if size != given {
                        carried.insert(node.clone(), *size);
                    }
                }
            }
        }
        // Entries of children that left the split, are hidden, or are empty
        // are kept, so that such a child has its share again when it is shown.
        // An empty child gets no room, no handle, and no collapsed line.
        let nodes: Vec<NodeId> = {
            let state_handle = window_state(cx);
            let layout_state = state_handle.borrow();
            let registry = cx.global::<Registry>();
            let layers = self.slots.tree.borrow();
            let tree = layers.current();
            let views = self.slots.views.borrow();
            slot_nodes(
                tree,
                &layout_state.item_children.borrow(),
                &self.node,
                "children",
            )
            .into_iter()
            .filter(|child| {
                views.contains_key(child)
                    && !node_emptiness(registry, &layout_state, tree, &views, child, mode, cx).0
            })
            .collect()
        };
        let same_nodes = nodes == self.shown_nodes;
        let (shares, given): (Vec<_>, Vec<_>) = nodes
            .iter()
            .map(|node| {
                let (mut child, given) = self.size_for(node, cx);
                if same_nodes
                    && given.is_some()
                    && let Some(size) = carried.get(node)
                {
                    child.size = *size;
                }
                if same_nodes {
                    child.collapsed = self.shown_shares.borrow()
                        [self.shown_nodes.iter().position(|id| id == node).unwrap()]
                    .collapsed;
                }
                (child, given)
            })
            .unzip();
        self.shown_given = given;
        if !same_nodes {
            // A changed child set also ends any positional drag in the axis.
            self.shown_shares = Rc::new(RefCell::new(shares));
        } else {
            *self.shown_shares.borrow_mut() = shares;
        }
        self.shown_nodes = nodes;
    }

    /// Chooses a fixed size before consulting the child's flexible share,
    /// and the child's preferred size where it has neither. Also gives the
    /// size it chose for a child with no entry.
    fn size_for(&self, node: &NodeId, cx: &App) -> (yard_ui::AxisChild, Option<yard_ui::AxisSize>) {
        let state_handle = window_state(cx);
        let layout_state = state_handle.borrow();
        let registry = cx.global::<Registry>();
        let tree_handle = layout_state.tree.clone();
        let layers = tree_handle.borrow();
        let tree = layers.current();
        let views = layout_state.views.borrow();
        let preferred = component_of(tree, node)
            .and_then(|component| registry.size_hints.get(component))
            .and_then(|hints| match self.axis {
                Axis::Horizontal => hints.preferred.width,
                Axis::Vertical => hints.preferred.height,
            })
            .map(|preferred| preferred.to_pixels(self.rem_size));
        let (size, given) = if let Some(size) = self
            .sizes
            .get(node.as_str())
            .or_else(|| self.sizes.get(template_id(node).as_str()))
        {
            (yard_ui::AxisSize::Fixed(*size), false)
        } else if let Some(share) = self
            .shares
            .get(node.as_str())
            .or_else(|| self.shares.get(template_id(node).as_str()))
        {
            (yard_ui::AxisSize::Flex(*share), false)
        } else if let Some(preferred) = preferred {
            (yard_ui::AxisSize::Fixed(preferred), true)
        } else {
            (yard_ui::AxisSize::Flex(1.), true)
        };
        let min = node_minimum(
            registry,
            &layout_state,
            tree,
            &views,
            node,
            self.rem_size,
            cx,
        )
        .along(self.axis);
        (
            yard_ui::AxisChild {
                size,
                min,
                collapsed: hidden(tree, node),
            },
            given.then_some(size),
        )
    }

    fn set_sizes(
        &mut self,
        shares: BTreeMap<String, f32>,
        sizes: BTreeMap<String, Pixels>,
        cx: &App,
    ) {
        self.shares = shares;
        self.sizes = sizes;
        let (shown, given) = self
            .shown_nodes
            .iter()
            .map(|node| self.size_for(node, cx))
            .unzip();
        *self.shown_shares.borrow_mut() = shown;
        self.shown_given = given;
    }
}

impl Render for Split {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let _window_scope = window_context(window, cx);
        self.rem_size = window.rem_size();
        self.reconcile(cx, Emptiness::Layout);
        let children = self
            .shown_nodes
            .iter()
            .cloned()
            .filter_map(|id| {
                self.slots
                    .views
                    .borrow()
                    .get(&id)
                    .cloned()
                    .map(|view| (id, view))
            })
            .map(|(child_id, view)| {
                div()
                    .id(SharedString::from(format!("{}:{child_id}", self.node)))
                    .relative()
                    .size_full()
                    .child(view)
                    .into_any_element()
            })
            .collect();
        let split = self.node.clone();
        let shown = self.shown_nodes.clone();
        let preferred: Vec<Option<Pixels>> = self
            .shown_given
            .iter()
            .map(|given| match given {
                Some(yard_ui::AxisSize::Fixed(preferred)) => Some(*preferred),
                _ => None,
            })
            .collect();
        yard_ui::axis(
            SharedString::from(self.node.to_string()),
            self.axis,
            self.shown_shares.clone(),
            children,
        )
        .can_collapse({
            let nodes = self.shown_nodes.clone();
            let split = self.node.clone();
            let owner = window.window_handle().window_id();
            move |index, cx| {
                let _window_scope = owner_scope(owner, cx);
                let Some(node) = nodes.get(index) else {
                    return false;
                };
                if is_copy_id(node.as_str()) {
                    return false;
                }
                let state_handle = window_state(cx);
                let layout_state = state_handle.borrow();
                let registry = cx.global::<Registry>();
                let allowed = required_rule_error(
                    registry,
                    &layout_state,
                    &Operation::SetHidden {
                        node: node.clone(),
                        hidden: true,
                    },
                )
                .is_none();
                if !allowed {
                    tracing::debug!(split = %split, child = %node, "required child kept");
                }
                allowed
            }
        })
        .on_resized(move |resized, window, cx| {
            let _window_scope = window_scope(window, cx);
            record_resize(&split, &shown, &preferred, resized, cx);
        })
    }
}

struct Tabs {
    node: NodeId,
    slots: Slots,
    selected: Option<NodeId>,
    /// Whether the next layout selects the first child with a tab, from what
    /// the views say then: the selection was chosen from remembered emptiness.
    reselect: bool,
    bar_bounds: Option<Bounds<Pixels>>,
    /// The tab bar's scroll position: view state, as the selection is.
    scroll: ScrollHandle,
    /// The selected tab the bar was last asked to scroll into view, and its
    /// index among the shown tabs then: a change of either scrolls again.
    scrolled_to: Option<(NodeId, usize)>,
    /// The bar's offset at the end of the last layout or wheel event.
    seen_offset: Point<Pixels>,
    /// The ID of the tab saved at the last run, waiting for the decision:
    /// selected if it has a tab then. An item copy's ID is matched as written.
    pending: Option<String>,
}

impl Tabs {
    /// The children that have a tab: shown and not empty.
    fn shown(&self, cx: &App, mode: Emptiness<'_>) -> Vec<NodeId> {
        self.slots
            .nodes("children")
            .into_iter()
            .filter(|node| !empty_now(cx, node, mode))
            .collect()
    }

    /// Outside layout: moves the selection off a child that is hidden or
    /// gone, provisionally, and lets the next layout choose from fresh
    /// emptiness. A selected child that only seems empty is left to it.
    fn reconcile_selection(&mut self, cx: &App) {
        let nodes = self.slots.nodes("children");
        if !self
            .selected
            .as_ref()
            .is_some_and(|node| nodes.contains(node))
        {
            self.choose_first(cx);
        }
    }

    /// Selects the first child with a tab by remembered emptiness, and lets
    /// the next layout choose again from fresh emptiness.
    fn choose_first(&mut self, cx: &App) {
        self.selected = self.shown(cx, Emptiness::Remembered).first().cloned();
        self.reselect = true;
    }

    /// Selects the first of `nodes` when the selected child is not among them.
    fn select_shown(&mut self, nodes: &[NodeId]) {
        if !self
            .selected
            .as_ref()
            .is_some_and(|node| nodes.contains(node))
        {
            self.selected = nodes.first().cloned();
        }
    }

    /// Selects the saved tab if it is among `nodes`, the children with a
    /// tab now. Otherwise it waits, or, when `last`, the first of `nodes` is
    /// selected. Logs the decision once.
    fn give_pending(&mut self, nodes: &[NodeId], last: bool) {
        let Some(pending) = self.pending.as_deref() else {
            return;
        };
        if let Some(node) = nodes.iter().find(|node| node.as_str() == pending) {
            tracing::debug!(tabs = %self.node, tab = %node, "tab given");
            self.selected = Some(node.clone());
        } else if last {
            tracing::debug!(tabs = %self.node, tab = pending, "saved tab has no tab");
            self.selected = nodes.first().cloned();
        } else {
            return;
        }
        self.pending = None;
        self.reselect = false;
    }

    fn select(&mut self, node: NodeId, cx: &mut Context<Self>) {
        self.pending = None;
        if self.selected.as_ref() == Some(&node) {
            return;
        }
        self.selected = Some(node.clone());
        cx.notify();
        tracing::debug!(tabs = %self.node, tab = %node, "tab selected");
        state_changed(cx);
    }
}

impl Render for Tabs {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let _window_scope = window_context(window, cx);
        let nodes = self.shown(cx, Emptiness::Layout);
        if self.pending.is_some() {
            self.give_pending(&nodes, true);
        } else if std::mem::take(&mut self.reselect) {
            self.selected = nodes.first().cloned();
        } else {
            self.select_shown(&nodes);
        }
        let selected_index = nodes
            .iter()
            .position(|node| Some(node) == self.selected.as_ref());
        // As Zed's `Pane::update_active_tab`: a newly selected tab is scrolled
        // into view during the next layout.
        let mut into_view = None;
        let selected_at = self.selected.clone().zip(selected_index);
        if self.scrolled_to != selected_at {
            self.scrolled_to = selected_at.clone();
            if let Some((node, index)) = selected_at {
                self.scroll.scroll_to_item(index);
                into_view = Some((node, self.scroll.offset()));
            }
        }
        let parent = self.node.clone();
        let item_tabs = cx.global::<Registry>().item_tabs.contains_key(&self.node);
        let tabs = nodes
            .iter()
            .map(|node| {
                let entity = cx.entity().downgrade();
                let click_node = node.clone();
                let target_node = node.clone();
                let target_parent = parent.clone();
                let (label, icon) = tab_title(node, cx);
                let dragged = DraggedNode {
                    node: node.clone(),
                    label: label.clone(),
                    icon,
                };
                let tab = yard_ui::Tab::new(SharedString::from(node.to_string()), label);
                let tab = if let Some(icon) = icon {
                    tab.icon(icon)
                } else {
                    tab
                };
                let required = {
                    let state_handle = window_state(cx);
                    let layout_state = state_handle.borrow();
                    let registry = cx.global::<Registry>();
                    let tree_handle = layout_state.tree.clone();
                    let layers = tree_handle.borrow();
                    node_of(layers.current(), node)
                        .is_some_and(|node| registry.required.contains(&node.component))
                };
                let tab = if item_tabs && is_copy_id(node.as_str()) {
                    let tabs = parent.clone();
                    let key = SharedString::from(
                        node.as_str()
                            .split_once('@')
                            .map_or("", |(_, key)| key)
                            .to_owned(),
                    );
                    tab.on_close(move |window, cx| close_item_clicked(&tabs, &key, window, cx))
                } else if required || is_copy_id(node.as_str()) {
                    tab
                } else {
                    let node = node.clone();
                    tab.on_close(move |window, cx| {
                        let _window_scope = window_context(window, cx);
                        tracing::debug!(node = %node, "tab close");
                        Extension {
                            name: "yard".into(),
                        }
                        .remove(cx, node.as_str());
                    })
                };
                tab.selected(Some(node) == self.selected.as_ref())
                    .on_click(move |window, cx| {
                        let _window_scope = window_context(window, cx);
                        let _ = entity.update(cx, |tabs, cx| tabs.select(click_node.clone(), cx));
                    })
                    .on_drag(dragged, |dragged, window, cx| {
                        let _window_scope = window_context(window, cx);
                        tracing::debug!(node = %dragged.node, "drag started");
                        cx.new(|_| dragged.clone()).into()
                    })
                    .on_drop::<DraggedNode>(move |dragged, window, cx| {
                        let _window_scope = window_scope(window, cx);
                        drop_on_tab(cx, &dragged.node, &target_parent, &target_node);
                    })
            })
            .collect();
        let bar_parent = self.node.clone();
        let bar_entity = cx.entity().downgrade();
        let bar = yard_ui::TabBar::new(SharedString::from(self.node.to_string()))
            .tabs(tabs)
            .track_scroll(&self.scroll)
            .on_drop::<DraggedNode>(move |dragged, window, cx| {
                let _window_scope = window_scope(window, cx);
                drop_on_tab_bar(cx, &dragged.node, &bar_parent);
            });
        // As Zed's `render_tab_bar`: the new button at the bar's end.
        let bar = if item_tabs {
            let new_node = self.node.clone();
            let selector = self.node.clone();
            bar.end_child(
                div()
                    .debug_selector(move || format!("NEW-{selector}"))
                    .child(
                        yard_ui::IconButton::new("new item", yard_ui::IconName::Plus)
                            .icon_size(yard_ui::IconSize::Small)
                            .tooltip(yard_ui::Tooltip::text("New Item"))
                            .on_click(move |window, cx| new_item_clicked(&new_node, window, cx)),
                    ),
            )
        } else {
            bar
        };
        div()
            .flex()
            .flex_col()
            .size_full()
            .on_children_prepainted(move |children_bounds, window, cx| {
                let _window_scope = window_context(window, cx);
                let _ = bar_entity.update(cx, |tabs, _| {
                    if let Some(&bounds) = children_bounds.first() {
                        tabs.bar_bounds = Some(bounds);
                    }
                    let offset = tabs.scroll.offset();
                    if let Some((node, before)) = &into_view
                        && offset != *before
                    {
                        tracing::debug!(tabs = %tabs.node, node = %node, "tab scrolled into view");
                    }
                    tabs.seen_offset = offset;
                });
            })
            .on_scroll(cx.listener(|tabs, _: &ScrollEvent, _, _| {
                let offset = tabs.scroll.offset();
                if offset != tabs.seen_offset {
                    tabs.seen_offset = offset;
                    tracing::trace!(tabs = %tabs.node, offset = ?offset.x, "tab bar scrolled");
                }
            }))
            .child(bar)
            .children(
                selected_index
                    .and_then(|index| nodes.get(index))
                    .and_then(|node| self.slots.views.borrow().get(node).cloned())
                    .map(|view| div().flex_1().min_h_0().child(view)),
            )
    }
}

fn tab_title(node: &NodeId, cx: &App) -> (SharedString, Option<yard_ui::IconName>) {
    let fallback = || (SharedString::from(node.to_string()), None);
    let state_handle = window_state(cx);
    let layout_state = state_handle.borrow();
    let registry = cx.global::<Registry>();
    let tree_handle = layout_state.tree.clone();
    let layers = tree_handle.borrow();
    let Some(component) = node_of(layers.current(), node).map(|node| &node.component) else {
        return fallback();
    };
    let Some(reader) = registry.titles.get(component) else {
        return fallback();
    };
    let views = layout_state.views.borrow();
    let Some(view) = views.get(node) else {
        return fallback();
    };
    let wrapped = view
        .clone()
        .downcast::<NodeView>()
        .expect("the registry stores node views");
    match reader(&wrapped.read(cx).view, cx) {
        Some((title, icon)) => {
            tracing::trace!(node = %node, title = %title, "tab title");
            (title, icon)
        }
        None => {
            if layout_state.title_errors.borrow_mut().insert(node.clone()) {
                log::error!("view of node {node} is not the type that gives its title");
            }
            tracing::trace!("view of node {node} is not the type that gives its title");
            fallback()
        }
    }
}

/// How a node's emptiness is found.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Emptiness<'a> {
    /// Asks the views, as a container lays out, once per node and frame, and
    /// logs what it reads.
    Layout,
    /// Asks the views and logs nothing.
    Ask,
    /// Asks the views and logs nothing, except the view of this node, which
    /// is being updated: its emptiness is what the last layout read.
    AskBesides(&'a NodeId),
    /// Takes what the last layout read, so that work which may run while an
    /// extension's view is being updated never reads that view.
    Remembered,
}

/// The window's root and the shell's frame are never empty.
fn never_empty(tree: &Tree, node: &NodeId) -> bool {
    is_frame_node(node)
        || NodeId::parse("body").is_ok_and(|body| {
            tree.slots(&body)
                .any(|(_, children)| children.iter().any(|child| &child.id == node))
        })
}

/// Whether a node is empty, and whether it can be: its component said it
/// can be, or it is a container with a child that can be. A container is
/// empty when it has children, at least one of them is empty, and every
/// other one is hidden.
fn node_emptiness(
    registry: &Registry,
    layout_state: &WindowState,
    tree: &Tree,
    views: &ViewMap,
    node: &NodeId,
    mode: Emptiness<'_>,
    cx: &App,
) -> (bool, bool) {
    if mode == Emptiness::Layout
        && let Some(laid_out) = layout_state.laid_out.borrow().get(node)
    {
        return *laid_out;
    }
    let component = component_of(tree, node);
    let (mut empty, mut can) = match component.and_then(|component| registry.empties.get(component))
    {
        None => (false, false),
        Some(reader) => {
            let remembered = match mode {
                Emptiness::Remembered => true,
                Emptiness::AskBesides(leased) => leased == node,
                Emptiness::Layout | Emptiness::Ask => false,
            };
            let empty = if remembered {
                layout_state
                    .emptiness
                    .borrow()
                    .get(node)
                    .copied()
                    .unwrap_or(false)
            } else {
                read_view_empty(layout_state, views, node, reader, cx)
            };
            (empty, true)
        }
    };
    if component.is_some_and(|component| {
        matches!(component.as_str(), "yard.row" | "yard.column" | "yard.tabs")
    }) {
        let children: Vec<NodeId> =
            slot_nodes(tree, &layout_state.item_children.borrow(), node, "children")
                .into_iter()
                .filter(|child| views.contains_key(child))
                .collect();
        let mut any_empty = false;
        let mut none_shown = true;
        for child in &children {
            let (child_empty, child_can) =
                node_emptiness(registry, layout_state, tree, views, child, mode, cx);
            can |= child_can;
            any_empty |= child_empty;
            none_shown &= child_empty || hidden(tree, child);
        }
        empty |= !children.is_empty() && any_empty && none_shown;
    }
    if empty && never_empty(tree, node) {
        empty = false;
    }
    if mode == Emptiness::Layout {
        layout_state
            .laid_out
            .borrow_mut()
            .insert(node.clone(), (empty, can));
        if can {
            tracing::trace!(node = %node, empty, "empty");
            let previous = layout_state
                .emptiness
                .borrow_mut()
                .insert(node.clone(), empty);
            if previous.is_some_and(|previous| previous != empty) {
                tracing::debug!(node = %node, empty, "emptiness changed");
            }
        } else if layout_state.emptiness.borrow_mut().remove(node) == Some(true) {
            // A container that lost the children that could be empty is no
            // longer empty.
            tracing::debug!(node = %node, empty, "emptiness changed");
        }
    }
    (empty, can)
}

fn read_view_empty(
    layout_state: &WindowState,
    views: &ViewMap,
    node: &NodeId,
    reader: &EmptyReader,
    cx: &App,
) -> bool {
    let Some(view) = views.get(node) else {
        return false;
    };
    let wrapped = view
        .clone()
        .downcast::<NodeView>()
        .expect("the registry stores node views");
    match reader(&wrapped.read(cx).view, cx) {
        Some(empty) => empty,
        None => {
            if layout_state.empty_errors.borrow_mut().insert(node.clone()) {
                log::error!("view of node {node} is not the type that says whether it is empty");
            }
            tracing::trace!("view of node {node} is not the type that says whether it is empty");
            false
        }
    }
}

fn empty_now(cx: &App, node: &NodeId, mode: Emptiness<'_>) -> bool {
    let state_handle = window_state(cx);
    let layout_state = state_handle.borrow();
    let Some(registry) = cx.try_global::<Registry>() else {
        return false;
    };
    let tree_handle = layout_state.tree.clone();
    let layers = tree_handle.borrow();
    let views = layout_state.views.borrow();
    node_emptiness(
        registry,
        &layout_state,
        layers.current(),
        &views,
        node,
        mode,
        cx,
    )
    .0
}

fn runtime_id(cx: &App, node: &str) -> Option<NodeId> {
    cx.try_global::<Registry>()?;
    NodeId::parse(node).ok().or_else(|| {
        window_state(cx)
            .borrow()
            .copies
            .keys()
            .find(|id| id.as_str() == node)
            .cloned()
    })
}

/// Returns whether a node is empty now: its view says so, or it is a
/// container whose children are all empty or hidden.
pub fn is_empty(cx: &App, node: &str) -> bool {
    if no_window(cx) {
        tracing::info!("no window");
        return false;
    }
    runtime_id(cx, node).is_some_and(|node| empty_now(cx, &node, Emptiness::Ask))
}

fn current_location(tree: &Tree, target: &NodeId) -> Option<(NodeId, SlotName, usize, usize)> {
    fn find(
        tree: &Tree,
        parent: &NodeId,
        target: &NodeId,
    ) -> Option<(NodeId, SlotName, usize, usize)> {
        for (slot, children) in tree.slots(parent) {
            if let Some(index) = children.iter().position(|child| &child.id == target) {
                return Some((parent.clone(), slot.clone(), index, children.len()));
            }
            for child in children {
                if let Some(found) = find(tree, &child.id, target) {
                    return Some(found);
                }
            }
        }
        None
    }
    find(tree, &tree.root()?.id, &template_id(target))
}

fn slot_index_of(tree: &Tree, parent: &NodeId, slot: &str, child_id: &NodeId) -> Option<usize> {
    tree.slots(parent)
        .find(|(name, _)| name.as_str() == slot)
        .and_then(|(_, children)| children.iter().position(|child| &child.id == child_id))
}

fn is_own_position(tree: &Tree, node: &NodeId, parent: &NodeId, slot: &str, index: usize) -> bool {
    current_location(tree, node).is_some_and(|(cur_parent, cur_slot, cur_index, cur_len)| {
        &cur_parent == parent && cur_slot.as_str() == slot && index.min(cur_len - 1) == cur_index
    })
}

fn drop_on_tab(cx: &mut App, node: &NodeId, parent: &NodeId, target_tab: &NodeId) {
    let _window_scope = selected_scope(cx);
    refresh_after_drop(cx);
    if is_copy_id(node.as_str()) || live_copy(cx, node.as_str()) {
        shell_move(cx, node, parent, "children", 0);
        return;
    }
    let index = {
        let tree_handle = window_state(cx).borrow().tree.clone();
        let layers = tree_handle.borrow();
        let tree = layers.current();
        let Some(index) = slot_index_of(tree, parent, "children", target_tab) else {
            return;
        };
        if is_own_position(tree, node, parent, "children", index) {
            tracing::debug!(node = %node, "drop ignored");
            return;
        }
        index
    };
    shell_move(cx, node, parent, "children", index);
}

fn drop_on_tab_bar(cx: &mut App, node: &NodeId, parent: &NodeId) {
    let _window_scope = selected_scope(cx);
    refresh_after_drop(cx);
    {
        let tree_handle = window_state(cx).borrow().tree.clone();
        let layers = tree_handle.borrow();
        let tree = layers.current();
        if is_own_position(tree, node, parent, "children", usize::MAX) {
            tracing::debug!(node = %node, "drop ignored");
            return;
        }
    }
    shell_move(cx, node, parent, "children", usize::MAX);
}

fn drop_on_edge(cx: &mut App, node: &NodeId, split_id: &NodeId, shown_index: usize, edge: Edge) {
    let _window_scope = selected_scope(cx);
    refresh_after_drop(cx);
    let shown_child = try_window_state(cx).and_then(|state_handle| {
        let layout_state = state_handle.borrow();
        layout_state
            .splits
            .iter()
            .filter_map(|weak| weak.upgrade())
            .find(|split| &split.read(cx).node == split_id)
            .and_then(|split| split.read(cx).shown_nodes.get(shown_index).cloned())
    });
    let Some(shown_child) = shown_child else {
        return;
    };
    let index = {
        let tree_handle = window_state(cx).borrow().tree.clone();
        let layers = tree_handle.borrow();
        let tree = layers.current();
        let Some(slot_pos) = slot_index_of(tree, split_id, "children", &shown_child) else {
            return;
        };
        let gap = match edge {
            Edge::Before => slot_pos,
            Edge::After => slot_pos + 1,
        };
        let index = match current_location(tree, node) {
            Some((cur_parent, cur_slot, cur_index, _))
                if &cur_parent == split_id
                    && cur_slot.as_str() == "children"
                    && cur_index < gap =>
            {
                gap - 1
            }
            _ => gap,
        };
        if is_own_position(tree, node, split_id, "children", index) {
            tracing::debug!(node = %node, "drop ignored");
            return;
        }
        index
    };
    shell_move(cx, node, split_id, "children", index);
}

/// A position as a log field: an index logs as its number.
fn index_field(at: &Position) -> String {
    match at {
        Position::Last => "last".into(),
        Position::First => "first".into(),
        Position::Index(index) => index.to_string(),
        Position::Before(node) => format!("before {node}"),
        Position::After(node) => format!("after {node}"),
    }
}

fn shell_move(cx: &mut App, node: &NodeId, parent: &NodeId, slot: &str, index: usize) {
    run_op("yard", None, cx, "move", node.as_str(), |_, node| {
        let slot = SlotName::parse(slot)
            .map_err(|error| format!("invalid slot name {slot:?}: {error}"))?;
        Ok(Operation::Move {
            node,
            parent: parent.clone(),
            slot,
            at: Position::Index(index),
        })
    });
}

/// Returns a node's own visibility flag for extension controls.
pub fn is_hidden(cx: &App, node: &str) -> bool {
    if no_window(cx) {
        tracing::info!("no window");
        return false;
    }
    let id = if is_copy_id(node) || live_copy(cx, node) {
        node.split_once('@')
            .and_then(|(template, _)| NodeId::parse(template).ok())
    } else {
        NodeId::parse(node).ok()
    };
    id.is_some_and(|node| {
        try_window_state(cx).is_some_and(|state_handle| {
            let layout_state = state_handle.borrow();
            layout_state.tree.borrow().current().is_hidden(&node)
        })
    })
}

/// Returns the number of recorded runtime layout operations.
pub fn user_layer_len(cx: &App) -> usize {
    if no_window(cx) {
        tracing::info!("no window");
        return 0;
    }
    try_window_state(cx).map_or(0, |state_handle| {
        let layout_state = state_handle.borrow();
        layout_state.tree.borrow().user_layer().len()
    })
}

/// Returns the number of recorded runtime layout operations for this window.
pub fn window_user_layer_len(window: &Window, cx: &App) -> usize {
    let _scope = window_context(window, cx);
    user_layer_len(cx)
}

/// Sets the file where the user layer is saved and from which it is restored.
pub fn set_layout_file(cx: &mut App, path: PathBuf) {
    tracing::debug!(path = %path.display(), "layout file set");
    cx.default_global::<Registry>().layout_file = Some(path);
}

/// Sets the title shown in the title bar when its `left` slot has no shown item.
pub fn set_window_title(cx: &mut App, title: impl Into<SharedString>) {
    let title = title.into();

    tracing::debug!(title = %title, "window title");
    cx.default_global::<Registry>().window_title = title;
    cx.refresh_windows();
}

/// Binds the shell's default keys, including the application-wide quit key.
pub fn bind_default_keys(cx: &mut App) {
    #[cfg(target_os = "macos")]
    const DEFAULT_KEYS: &[(&str, LayoutAction)] = &[
        ("shift-escape", LayoutAction::ToggleZoom),
        ("cmd-}", LayoutAction::SelectNextTab),
        ("alt-cmd-right", LayoutAction::SelectNextTab),
        ("cmd-{", LayoutAction::SelectPreviousTab),
        ("alt-cmd-left", LayoutAction::SelectPreviousTab),
        ("cmd-k cmd-left", LayoutAction::FocusLeft),
        ("cmd-k cmd-right", LayoutAction::FocusRight),
        ("cmd-k cmd-up", LayoutAction::FocusUp),
        ("cmd-k cmd-down", LayoutAction::FocusDown),
    ];
    #[cfg(not(target_os = "macos"))]
    const DEFAULT_KEYS: &[(&str, LayoutAction)] = &[
        ("shift-escape", LayoutAction::ToggleZoom),
        ("ctrl-pagedown", LayoutAction::SelectNextTab),
        ("ctrl-pageup", LayoutAction::SelectPreviousTab),
        ("ctrl-k ctrl-left", LayoutAction::FocusLeft),
        ("ctrl-k ctrl-right", LayoutAction::FocusRight),
        ("ctrl-k ctrl-up", LayoutAction::FocusUp),
        ("ctrl-k ctrl-down", LayoutAction::FocusDown),
    ];
    let count = DEFAULT_KEYS.len() + 3;
    let _group = tracing::info_span!("keys-bind", count).entered();
    let mut logged = Vec::with_capacity(count);
    let mut bindings: Vec<KeyBinding> = DEFAULT_KEYS
        .iter()
        .map(|&(keys, action)| {
            let binding = match action {
                LayoutAction::ToggleZoom => KeyBinding::new(keys, ToggleZoom, Some("Shell")),
                LayoutAction::Hide => KeyBinding::new(keys, Hide, Some("Shell")),
                LayoutAction::FocusLeft => KeyBinding::new(keys, FocusLeft, Some("Shell")),
                LayoutAction::FocusRight => KeyBinding::new(keys, FocusRight, Some("Shell")),
                LayoutAction::FocusUp => KeyBinding::new(keys, FocusUp, Some("Shell")),
                LayoutAction::FocusDown => KeyBinding::new(keys, FocusDown, Some("Shell")),
                LayoutAction::SelectNextTab => KeyBinding::new(keys, SelectNextTab, Some("Shell")),
                LayoutAction::SelectPreviousTab => {
                    KeyBinding::new(keys, SelectPreviousTab, Some("Shell"))
                }
            };
            logged.push((keys, binding.action().name(), "Shell"));
            binding
        })
        .collect();
    let dismiss = KeyBinding::new("escape", Dismiss, Some("ModalLayer"));
    logged.push(("escape", dismiss.action().name(), "ModalLayer"));
    bindings.push(dismiss);
    #[cfg(target_os = "macos")]
    let quit_key = "cmd-q";
    #[cfg(not(target_os = "macos"))]
    let quit_key = "ctrl-q";
    let quit = KeyBinding::new(quit_key, Quit, None);
    logged.push((quit_key, quit.action().name(), ""));
    bindings.push(quit);
    #[cfg(target_os = "macos")]
    let new_window_key = "cmd-shift-n";
    #[cfg(not(target_os = "macos"))]
    let new_window_key = "ctrl-shift-n";
    let new_window = KeyBinding::new(new_window_key, NewWindow, Some("Shell"));
    logged.push((new_window_key, new_window.action().name(), "Shell"));
    bindings.push(new_window);
    cx.bind_keys(bindings);
    for (keys, action, context) in logged {
        tracing::debug!(keys, action, context, "bound");
    }
}

/// Returns the node currently filling the body for extension controls.
pub fn zoomed(cx: &App) -> Option<String> {
    if no_window(cx) {
        tracing::info!("no window");
        return None;
    }
    try_window_state(cx).and_then(|state_handle| {
        let layout_state = state_handle.borrow();
        layout_state.zoomed.as_ref().map(ToString::to_string)
    })
}

/// Returns the node currently shown by a tabs container.
pub fn selected_tab(cx: &App, tabs: &str) -> Option<NodeId> {
    if no_window(cx) {
        tracing::info!("no window");
        return None;
    }
    let node = NodeId::parse(tabs).ok()?;
    try_window_state(cx)?
        .borrow()
        .tabs
        .iter()
        .filter_map(|tabs| tabs.upgrade())
        .find_map(|tabs| {
            let tabs = tabs.read(cx);
            (tabs.node == node).then(|| tabs.selected.clone()).flatten()
        })
}

#[derive(Clone, Copy)]
enum LayoutAction {
    ToggleZoom,
    Hide,
    SelectNextTab,
    SelectPreviousTab,
    FocusLeft,
    FocusRight,
    FocusUp,
    FocusDown,
}

impl LayoutAction {
    fn name(self) -> &'static str {
        match self {
            Self::ToggleZoom => "ToggleZoom",
            Self::Hide => "Hide",
            Self::SelectNextTab => "SelectNextTab",
            Self::SelectPreviousTab => "SelectPreviousTab",
            Self::FocusLeft => "FocusLeft",
            Self::FocusRight => "FocusRight",
            Self::FocusUp => "FocusUp",
            Self::FocusDown => "FocusDown",
        }
    }
}

fn layout_action(action: LayoutAction, node: Option<&NodeId>, window: &mut Window, cx: &mut App) {
    let _window_scope = window_scope(window, cx);
    let _group = tracing::info_span!(
        "layout-action",
        action = action.name(),
        node = node.map_or("none", NodeId::as_str),
    )
    .entered();
    let Some(node) = node else {
        tracing::info!(reason = "no node has the focus", "refused");
        return;
    };
    let shell = Extension {
        name: "yard".into(),
    };
    let applied = match action {
        LayoutAction::ToggleZoom => {
            let previous = zoomed(cx);
            if previous.is_some() {
                shell.zoom_out(cx);
            } else {
                shell.set_zoomed(cx, node.as_str());
            }
            zoomed(cx) != previous
        }
        LayoutAction::Hide => {
            shell.set_hidden(cx, node.as_str(), true);
            if is_copy_id(node.as_str()) || live_copy(cx, node.as_str()) {
                false
            } else if is_hidden(cx, node.as_str()) {
                if let Some(handle) = window_state(cx).borrow().root_focus_handle.clone() {
                    handle.focus(window, cx);
                }
                true
            } else {
                false
            }
        }
        LayoutAction::FocusLeft
        | LayoutAction::FocusRight
        | LayoutAction::FocusUp
        | LayoutAction::FocusDown => {
            focus_by_direction(action, node, window, cx);
            return;
        }
        LayoutAction::SelectNextTab | LayoutAction::SelectPreviousTab => select_adjacent_tab(
            &shell,
            cx,
            window,
            node,
            matches!(action, LayoutAction::SelectNextTab),
        ),
    };
    if applied {
        tracing::debug!("done");
    }
}

fn focus_by_direction(action: LayoutAction, node: &NodeId, window: &mut Window, cx: &mut App) {
    let _window_scope = window_scope(window, cx);
    let state_handle = window_state(cx);
    let layout_state = state_handle.borrow();
    let registry = cx.global::<Registry>();
    let reason = match action {
        LayoutAction::FocusLeft => "no node on the left",
        LayoutAction::FocusRight => "no node on the right",
        LayoutAction::FocusUp => "no node above",
        LayoutAction::FocusDown => "no node below",
        _ => unreachable!(),
    };
    let Some(bounds) = layout_state.bounds.get(node) else {
        tracing::info!(reason = "no bounds for the focused node", "refused");
        return;
    };
    let center = bounds.center();
    let distance = gpui::px(4.);
    let mut point = match action {
        LayoutAction::FocusLeft => Point::new(bounds.left() - distance, center.y),
        LayoutAction::FocusRight => Point::new(bounds.right() + distance, center.y),
        LayoutAction::FocusUp => Point::new(center.x, bounds.top() - distance),
        LayoutAction::FocusDown => Point::new(center.x, bounds.bottom() + distance),
        _ => unreachable!(),
    };
    let mut shown = BTreeSet::new();
    find_in_drawn::<()>(registry, &layout_state, cx, |node| {
        shown.insert(node.clone());
        None
    });
    let window_bounds = Bounds::new(Point::default(), window.viewport_size());
    let target = loop {
        let tree_handle = layout_state.tree.clone();
        let layers = tree_handle.borrow();
        let tree = layers.current();
        let mut inside_shown = window_bounds.contains(&point);
        let target = find_in_drawn(registry, &layout_state, cx, |candidate| {
            let candidate_bounds = layout_state.bounds.get(candidate);
            inside_shown |= candidate_bounds.is_some_and(|bounds| bounds.contains(&point));
            let container = component_of(tree, candidate).is_some_and(|component| {
                matches!(
                    component.as_str(),
                    "yard.row"
                        | "yard.column"
                        | "yard.tabs"
                        | "yard.window"
                        | "yard.title_bar"
                        | "yard.status_bar"
                        | "yard.body"
                )
            });
            let has_shown_child = tree
                .slots(candidate)
                .any(|(_, children)| children.iter().any(|child| shown.contains(&child.id)));
            if container || has_shown_child {
                return None;
            }
            let bounds = layout_state.bounds.get(candidate);
            tracing::trace!(candidate = %candidate, bounds = ?bounds, "considered");
            bounds
                .is_some_and(|bounds| bounds.contains(&point))
                .then(|| candidate.clone())
        });
        if target.is_some() || !inside_shown {
            break target;
        }
        match action {
            LayoutAction::FocusLeft => point.x -= distance,
            LayoutAction::FocusRight => point.x += distance,
            LayoutAction::FocusUp => point.y -= distance,
            LayoutAction::FocusDown => point.y += distance,
            _ => unreachable!(),
        }
    };
    if let Some(target) = target
        && let Some(handle) = layout_state.focus_handles.get(&target).cloned()
    {
        handle.focus(window, cx);
        tracing::debug!(target = %target, "done");
        return;
    }
    tracing::info!(reason, bounds = ?bounds, point = ?point, "refused");
}

fn select_adjacent_tab(
    shell: &Extension,
    cx: &mut App,
    window: &mut Window,
    node: &NodeId,
    next: bool,
) -> bool {
    let _window_scope = window_scope(window, cx);
    let tabs = {
        let state_handle = window_state(cx);
        let layout_state = state_handle.borrow();
        let tree_handle = layout_state.tree.clone();
        let layers = tree_handle.borrow();
        let tree = layers.current();
        let mut current = node.clone();
        loop {
            if component_of(tree, &current)
                .is_some_and(|component| component.as_str() == "yard.tabs")
            {
                break layout_state
                    .tabs
                    .iter()
                    .filter_map(|tabs| tabs.upgrade())
                    .find(|tabs| tabs.read(cx).node == current);
            }
            let Some((parent, _, _, _)) = current_location(tree, &current) else {
                break None;
            };
            current = parent;
        }
    };
    let Some(tabs) = tabs else {
        tracing::info!(reason = "no tabs container holds it", "refused");
        return false;
    };
    let selected = tabs.update(cx, |tabs, cx| {
        // The focused node's view is being updated by this action: every
        // other child's emptiness is read now.
        let nodes = tabs.shown(cx, Emptiness::AskBesides(node));
        if std::mem::take(&mut tabs.reselect) {
            tabs.selected = nodes.first().cloned();
        } else {
            tabs.select_shown(&nodes);
        }
        if nodes.is_empty() {
            tracing::info!(reason = "no tabs", "refused");
            return None;
        }
        if nodes.len() == 1 {
            tracing::info!(reason = "one tab", "refused");
            return None;
        }
        let index = nodes
            .iter()
            .position(|node| Some(node) == tabs.selected.as_ref())?;
        let adjacent = if next {
            (index + 1) % nodes.len()
        } else {
            (index + nodes.len() - 1) % nodes.len()
        };
        let zoom_inside = {
            let state_handle = window_state(cx);
            let layout_state = state_handle.borrow();
            let tree_handle = layout_state.tree.clone();
            let layers = tree_handle.borrow();
            let tree = layers.current();
            let mut current = layout_state.zoomed.clone();
            let mut inside = false;
            while let Some(node) = current {
                let Some((parent, _, _, _)) = current_location(tree, &node) else {
                    break;
                };
                if parent == tabs.node {
                    inside = true;
                    break;
                }
                current = Some(parent);
            }
            inside
        };
        if zoom_inside {
            shell.zoom_out(cx);
        }
        tabs.select(nodes[adjacent].clone(), cx);
        Some(tabs.node.clone())
    });
    if let Some(node) = selected {
        if let Some(handle) = window_state(cx).borrow().focus_handles.get(&node).cloned() {
            handle.focus(window, cx);
        }
        true
    } else {
        false
    }
}

/// Returns the innermost shown node that contains the keyboard focus.
pub fn focused_node(window: &Window, cx: &App) -> Option<NodeId> {
    let _window_scope = window_context(window, cx);
    if no_window(cx) {
        tracing::info!("no window");
        return None;
    }
    window.focused(cx)?;
    let state_handle = try_window_state(cx)?;
    let layout_state = state_handle.borrow();
    let registry = cx.try_global::<Registry>()?;
    find_in_drawn(registry, &layout_state, cx, |node| {
        (!is_frame_node(node)
            && layout_state
                .focus_handles
                .get(node)
                .is_some_and(|handle| handle.contains_focused(window, cx)))
        .then(|| node.clone())
    })
}

/// Returns the last drawn bounds of a shown node.
pub fn node_bounds(cx: &App, node: &str) -> Option<Bounds<Pixels>> {
    if no_window(cx) {
        tracing::info!("no window");
        return None;
    }
    let target = runtime_id(cx, node)?;
    let state_handle = window_state(cx);
    let layout_state = state_handle.borrow();
    let registry = cx.try_global::<Registry>()?;
    find_in_drawn(registry, &layout_state, cx, |current| {
        (current == &target)
            .then(|| layout_state.bounds.get(current).copied())
            .flatten()
    })
}

/// Returns a shown node's focus handle.
pub fn focus_handle(cx: &App, node: &str) -> Option<FocusHandle> {
    if no_window(cx) {
        tracing::info!("no window");
        return None;
    }
    let target = runtime_id(cx, node)?;
    if is_frame_node(&target) {
        return None;
    }
    let state_handle = window_state(cx);
    let layout_state = state_handle.borrow();
    let registry = cx.try_global::<Registry>()?;
    find_in_drawn(registry, &layout_state, cx, |current| {
        (current == &target)
            .then(|| layout_state.focus_handles.get(current).cloned())
            .flatten()
    })
}

fn find_in_drawn<T>(
    registry: &Registry,
    layout_state: &WindowState,
    cx: &App,
    mut visit: impl FnMut(&NodeId) -> Option<T>,
) -> Option<T> {
    let tabs_selected: BTreeMap<NodeId, Option<NodeId>> = layout_state
        .tabs
        .iter()
        .filter_map(|weak| weak.upgrade())
        .map(|tabs| {
            let tabs = tabs.read(cx);
            (tabs.node.clone(), tabs.selected.clone())
        })
        .collect();
    let tree_handle = layout_state.tree.clone();
    let layers = tree_handle.borrow();
    let tree = layers.current();
    let views = layout_state.views.borrow();
    let drawn_root = tree.root().map(|root| &root.id)?;
    let zoomed = layout_state
        .zoomed
        .as_ref()
        .filter(|node| views.contains_key(*node));
    let is_empty = |node: &NodeId| {
        node_emptiness(
            registry,
            layout_state,
            tree,
            &views,
            node,
            Emptiness::Remembered,
            cx,
        )
        .0
    };
    #[allow(clippy::too_many_arguments)]
    fn walk<T>(
        tree: &Tree,
        items: &ItemChildren,
        views: &ViewMap,
        is_empty: &dyn Fn(&NodeId) -> bool,
        tabs_selected: &BTreeMap<NodeId, Option<NodeId>>,
        zoomed: Option<&NodeId>,
        current: &NodeId,
        visit: &mut impl FnMut(&NodeId) -> Option<T>,
    ) -> Option<T> {
        if hidden(tree, current) || !views.contains_key(current) || is_empty(current) {
            return None;
        }
        let from_child = if current.as_str() == "body"
            && let Some(zoomed_node) = zoomed
        {
            walk(
                tree,
                items,
                views,
                is_empty,
                tabs_selected,
                None,
                zoomed_node,
                visit,
            )
        } else if let Some(selected) = tabs_selected.get(current) {
            selected.as_ref().and_then(|child| {
                walk(
                    tree,
                    items,
                    views,
                    is_empty,
                    tabs_selected,
                    zoomed,
                    child,
                    visit,
                )
            })
        } else {
            tree.slots(current).find_map(|(slot, _)| {
                slot_nodes(tree, items, current, slot.as_str())
                    .iter()
                    .find_map(|child| {
                        walk(
                            tree,
                            items,
                            views,
                            is_empty,
                            tabs_selected,
                            zoomed,
                            child,
                            visit,
                        )
                    })
            })
        };
        from_child.or_else(|| visit(current))
    }
    walk(
        tree,
        &layout_state.item_children.borrow(),
        &views,
        &is_empty,
        &tabs_selected,
        zoomed,
        drawn_root,
        &mut visit,
    )
}

fn save(cx: &App) {
    super::session::layout_changed(cx);
    let Some((path, len, encoded)) = cx.try_global::<Registry>().and_then(|registry| {
        let state_handle = window_state(cx);
        let layout_state = state_handle.borrow();
        if !layout_state.default_fixed {
            return None;
        }
        let path = layout_path(registry, &layout_state)?;
        let tree_handle = layout_state.tree.clone();
        let layers = tree_handle.borrow();
        let ops = layers.user_layer();
        Some((path, ops.len(), encode(ops)))
    }) else {
        return;
    };
    let _group = tracing::info_span!("layout-save", operations = len).entered();
    if write_atomic(&path, encoded.as_bytes()).is_ok() {
        tracing::debug!("saved");
    }
}

fn is_shown(tree: &Tree, node: &NodeId) -> bool {
    fn find(tree: &Tree, current: &NodeId, target: &NodeId) -> bool {
        !tree.is_hidden(current)
            && (current == target
                || tree.slots(current).any(|(_, children)| {
                    children.iter().any(|child| find(tree, &child.id, target))
                }))
    }
    tree.root().is_some_and(|root| find(tree, &root.id, node))
}

fn node_of<'a>(tree: &'a Tree, id: &NodeId) -> Option<&'a Node> {
    fn find<'a>(tree: &'a Tree, node: &'a Node, id: &NodeId) -> Option<&'a Node> {
        if &node.id == id {
            return Some(node);
        }
        tree.slots(&node.id)
            .flat_map(|(_, children)| children)
            .find_map(|child| find(tree, child, id))
    }
    find(tree, tree.root()?, &template_id(id))
}

fn component_of<'a>(tree: &'a Tree, id: &NodeId) -> Option<&'a ComponentId> {
    node_of(tree, id).map(|node| &node.component)
}

fn collect_nodes(tree: &Tree, node: &Node, out: &mut Vec<Node>) {
    out.push(node.clone());
    for (_, children) in tree.slots(&node.id) {
        for child in children {
            collect_nodes(tree, child, out);
        }
    }
}

fn placement_props(props: serde_json::Value) -> Result<Props, String> {
    if props.is_null() {
        Ok(Props::new())
    } else {
        encode_props(props)
    }
}

fn encode_props(props: impl serde::Serialize) -> Result<Props, String> {
    match serde_json::to_value(props) {
        Ok(serde_json::Value::Object(map)) => Ok(map),
        Ok(_) => Err("props must encode as a JSON object".into()),
        Err(error) => Err(format!("props cannot be encoded as JSON: {error}")),
    }
}

fn read_props<P>(node: &NodeId, component: &ComponentId, props: &Props) -> P
where
    P: serde::Serialize + serde::de::DeserializeOwned + Default,
{
    let _group = tracing::info_span!(
        "props-read",
        node = node.as_str(),
        component = component.as_str(),
    )
    .entered();
    let default = P::default();
    let default_map = match serde_json::to_value(&default) {
        Ok(serde_json::Value::Object(map)) => map,
        Ok(_) => {
            log::error!("default props of component {component} did not encode as a JSON object");
            return default;
        }
        Err(error) => {
            log::error!("default props of component {component} cannot be encoded: {error}");
            return default;
        }
    };
    let mut merged = default_map.clone();
    let mut accepted = Vec::new();
    for (key, value) in props {
        if !default_map.contains_key(key) {
            log::warn!("node {node} has an unknown prop {key}");
            continue;
        }
        let mut candidate = default_map.clone();
        candidate.insert(key.clone(), value.clone());
        match serde_json::from_value::<P>(serde_json::Value::Object(candidate)) {
            Ok(_) => {
                merged.insert(key.clone(), value.clone());
                accepted.push(key.as_str());
            }
            Err(error) => {
                log::warn!(
                    "prop {key} of node {node} cannot be read: {error}; the default is used"
                );
            }
        }
    }
    let keys = accepted.join(",");
    tracing::debug!(keys, "props read");
    serde_json::from_value::<P>(serde_json::Value::Object(merged)).unwrap_or(default)
}

fn reconcile_containers(cx: &mut App) {
    let (splits, tabs) = {
        let state_handle = window_state(cx);
        let mut layout_state = state_handle.borrow_mut();
        layout_state
            .splits
            .retain(|split| split.upgrade().is_some());
        layout_state.tabs.retain(|tabs| tabs.upgrade().is_some());
        (layout_state.splits.clone(), layout_state.tabs.clone())
    };
    for split in splits {
        let _ = split.update(cx, |split, cx| {
            split.reconcile(cx, Emptiness::Remembered);
            let layers = split.slots.tree.borrow();
            for (node, child) in split
                .shown_nodes
                .iter()
                .zip(split.shown_shares.borrow_mut().iter_mut())
            {
                child.collapsed = hidden(layers.current(), node);
            }
        });
    }
    for tabs in tabs {
        let _ = tabs.update(cx, |tabs, cx| tabs.reconcile_selection(cx));
    }
}

fn run_op(
    extension: &str,
    window: Option<&mut Window>,
    cx: &mut App,
    op: &'static str,
    node: &str,
    build: impl FnOnce(String, NodeId) -> Result<Operation, String>,
) {
    let _window_scope = selected_scope(cx);
    let _group = tracing::info_span!("layout-op", op, node, extension).entered();
    if no_window(cx) {
        tracing::info!("no window");
        return;
    }
    if is_copy_id(node) || live_copy(cx, node) {
        tracing::info!("{node} is a copy of an item slot");
        return;
    }
    let node_id = match NodeId::parse(node) {
        Ok(id) => id,
        Err(error) => {
            log::error!("invalid node ID {node:?}: {error}");
            return;
        }
    };
    let target_node = node_id.clone();
    let operation = match build(extension.to_string(), node_id) {
        Ok(operation) => operation,
        Err(message) => {
            log::error!("{message}");
            return;
        }
    };
    if !check_shell_rules(extension, cx, &operation) {
        return;
    }
    let added = match &operation {
        Operation::Add { node, .. } => Some(node.clone()),
        _ => None,
    };
    let applied = {
        let tree = window_state(cx).borrow().tree.clone();
        let mut tree = tree.borrow_mut();
        tree.apply(operation)
    };
    let Ok(applied) = applied else {
        return;
    };
    match applied {
        Applied::Added => {
            if let (Some(node), Some(window)) = (added, window) {
                let location = {
                    let state_handle = window_state(cx);
                    let layout_state = state_handle.borrow();
                    let registry = cx.global::<Registry>();
                    let tree_handle = layout_state.tree.clone();
                    let layers = tree_handle.borrow();
                    current_location(layers.current(), &node.id).and_then(|(owner, slot, _, _)| {
                        let component = component_of(layers.current(), &owner)?.clone();
                        let declared = registry.components.get(&component)?.slots.clone();
                        Some((owner, component, declared, slot))
                    })
                };
                let parent = location.as_ref().map(|(owner, component, declared, slot)| {
                    (owner, component, declared.as_slice(), slot)
                });
                let _ = create_view(&node, parent, window, cx);
            }
        }
        Applied::Removed(removed) => {
            let copies = window_state(cx)
                .borrow()
                .copies
                .keys()
                .filter(|copy| removed.contains(&template_id(copy)))
                .cloned()
                .collect::<Vec<_>>();
            for copy in copies {
                drop_copy(cx, &copy);
            }
            for id in removed {
                window_state(cx).borrow_mut().failed_views.remove(&id);
                let dropped = {
                    let state_handle = window_state(cx);
                    let mut layout_state = state_handle.borrow_mut();
                    layout_state.pending_states.remove(id.as_str());
                    layout_state.focus_handles.remove(&id);
                    layout_state.bounds.remove(&id);
                    layout_state.tellers.remove(&id);
                    layout_state.contexts.remove(&id);
                    layout_state.emptiness.borrow_mut().remove(&id);
                    layout_state.empty_errors.borrow_mut().remove(&id);
                    layout_state.laid_out.borrow_mut().remove(&id);
                    layout_state.views.borrow_mut().remove(&id)
                };
                if dropped.is_some() {
                    drop(dropped);
                    tracing::debug!(node = %id, "view dropped");
                }
            }
        }
        Applied::PropsSet => {
            let tell = {
                let state_handle = window_state(cx);
                let layout_state = state_handle.borrow();
                let teller = layout_state.tellers.get(&target_node).cloned();
                let props = teller.as_ref().and_then(|_| {
                    let tree_handle = layout_state.tree.clone();
                    let layers = tree_handle.borrow();
                    node_of(layers.current(), &target_node).map(|node| node.props.clone())
                });
                teller.zip(props)
            };
            if let Some((teller, props)) = tell {
                teller(&props, cx);
            }
        }
        Applied::Moved => {
            let state_handle = window_state(cx);
            let mut layout_state = state_handle.borrow_mut();
            let tree_handle = layout_state.tree.clone();
            let layers = tree_handle.borrow();
            let tree = layers.current();
            let mut moved = Vec::new();
            if let Some(node) = node_of(tree, &target_node) {
                collect_nodes(tree, node, &mut moved);
            }
            let moved = moved
                .into_iter()
                .map(|node| node.id)
                .collect::<BTreeSet<_>>();
            layout_state
                .failed_views
                .retain(|id| !moved.contains(&template_id(id)));
        }
        Applied::SetHidden(_) => {}
    }
    if window_state(cx).borrow_mut().splitting {
        return;
    }
    {
        let state_handle = window_state(cx);
        let mut layout_state = state_handle.borrow_mut();
        if layout_state.zoomed.as_ref().is_some_and(|node| {
            let tree_handle = layout_state.tree.clone();
            let layers = tree_handle.borrow();
            let tree = layers.current();
            !is_shown(tree, node) || !is_in_body(tree, node)
        }) {
            let node = layout_state.zoomed.take().unwrap();
            tracing::debug!(node = %node, "zoom cleared");
        }
    }
    reconcile_containers(cx);
    cx.refresh_windows();
    save(cx);
    schedule_items(cx, true);
}

fn schedule_registration_items(cx: &mut App) {
    let windows = cx
        .global::<Registry>()
        .windows
        .values()
        .filter_map(Weak::upgrade)
        .flat_map(|state| {
            let state = state.borrow();
            if !state.views_created {
                return Vec::new();
            }
            state
                .item_window
                .as_ref()
                .filter(|(_, owners)| owners.upgrade().is_some())
                .map(|(window, _)| *window)
                .into_iter()
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    if windows.is_empty() {
        return;
    }
    let registration = tracing::Span::current();
    cx.defer(move |cx| {
        for window in windows {
            let _ = window.update(cx, |_, window, cx| {
                let _window_scope = window_scope(window, cx);
                let _registration = registration.enter();
                sync_items(window, cx, true);
                reconcile_containers(cx);
                window.refresh();
            });
        }
    });
}

fn retry_registered_component(cx: &App, component: &ComponentId) {
    let registry = cx.global::<Registry>();
    let states = std::iter::once(registry.default.clone())
        .chain(registry.windows.values().filter_map(Weak::upgrade))
        .collect::<Vec<_>>();
    for state in states {
        let mut state = state.borrow_mut();
        let tree = state.tree.clone();
        let layers = tree.borrow();
        let retry = state
            .failed_views
            .iter()
            .filter(|id| {
                state
                    .copies
                    .get(*id)
                    .map(|node| &node.component)
                    .or_else(|| component_of(layers.current(), id))
                    == Some(component)
            })
            .cloned()
            .collect::<Vec<_>>();
        for id in retry {
            state.failed_views.remove(&id);
        }
    }
}

fn schedule_items(cx: &mut App, completed_operation: bool) {
    let state_handle = window_state(cx);
    let layout_state = state_handle.borrow();
    let registry = cx.global::<Registry>();
    if !layout_state.views_created
        || (registry.item_slots.is_empty() && layout_state.item_states.is_empty())
    {
        drop(layout_state);
        if completed_operation {
            notify_layout(cx);
        }
        return;
    }
    let windows = layout_state
        .item_window
        .iter()
        .filter(|(_, subscriptions)| subscriptions.upgrade().is_some())
        .map(|(window, _)| *window)
        .collect::<Vec<_>>();
    drop(layout_state);
    let owner = selected_window(cx);
    let operation = tracing::Span::current();
    cx.defer(move |cx| {
        let _owner = owner.map(|id| owner_scope(id, cx));
        if no_window(cx) {
            return;
        }
        let _group = operation.enter();
        for window in windows {
            let _ = window.update(cx, |_, window, cx| {
                let _window_scope = window_scope(window, cx);
                sync_items(window, cx, !completed_operation);
                reconcile_containers(cx);
                window.refresh();
            });
        }
        if completed_operation {
            notify_layout(cx);
        }
    });
}

/// Returns the key of an existing item-slot copy.
pub fn item(cx: &App, node: &str) -> Option<SharedString> {
    if no_window(cx) {
        tracing::info!("no window");
        return None;
    }
    let state_handle = try_window_state(cx)?;
    let layout_state = state_handle.borrow();
    layout_state.copies.keys().find(|id| id.as_str() == node)?;
    Some(node.split_once('@')?.1.to_owned().into())
}

fn drop_copy(cx: &mut App, id: &NodeId) {
    let state_handle = window_state(cx);
    let mut layout_state = state_handle.borrow_mut();
    layout_state.copies.remove(id);
    layout_state.failed_views.remove(id);
    layout_state.focus_handles.remove(id);
    layout_state.bounds.remove(id);
    layout_state.tellers.remove(id);
    layout_state.contexts.remove(id);
    layout_state.emptiness.borrow_mut().remove(id);
    layout_state.empty_errors.borrow_mut().remove(id);
    layout_state.title_errors.borrow_mut().remove(id);
    layout_state.laid_out.borrow_mut().remove(id);
    let dropped = layout_state.views.borrow_mut().remove(id);
    drop(layout_state);
    if dropped.is_some() {
        drop(dropped);
        tracing::debug!(node = %id, "view dropped");
    }
}

/// Resolves the model at the slot owner, including the owner's own models.
fn item_model(
    registry: &Registry,
    layout_state: &WindowState,
    tree: &Tree,
    owner: &NodeId,
    model: TypeId,
) -> Option<AnyEntity> {
    let mut current = owner.clone();
    loop {
        if let Some(value) = layout_state
            .contexts
            .get(&current)
            .and_then(|models| models.get(&model))
        {
            return Some(value.clone());
        }
        let Some((parent, _, _, _)) = current_location(tree, &current) else {
            break;
        };
        current = parent;
    }
    registry.window_contexts.get(&model).cloned()
}

fn sync_items(window: &mut Window, cx: &mut App, notify: bool) {
    let _window_scope = window_scope(window, cx);
    let before = window_state(cx).borrow().item_children.borrow().clone();
    let (root, slots) = {
        let state_handle = window_state(cx);
        let layout_state = state_handle.borrow();
        let registry = cx.default_global::<Registry>();
        if registry.item_slots.is_empty() && layout_state.item_states.is_empty() {
            return;
        }
        let tree_handle = layout_state.tree.clone();
        let layers = tree_handle.borrow();
        let tree = layers.current();
        let mut nodes = Vec::new();
        if let Some(root) = tree.root() {
            collect_nodes(tree, root, &mut nodes);
        }
        let slots = nodes
            .iter()
            .flat_map(|node| {
                registry
                    .item_slots
                    .iter()
                    .filter(|((owner, _), _)| owner == &node.id)
                    .map(|((_, slot), declaration)| {
                        let templates = tree
                            .slots(&node.id)
                            .find(|(name, _)| name.as_str() == slot)
                            .map(|(_, nodes)| nodes.to_vec())
                            .unwrap_or_default();
                        (
                            node.id.clone(),
                            slot.clone(),
                            declaration.clone(),
                            templates,
                        )
                    })
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        (tree.root().cloned(), slots)
    };
    let template_views = {
        let state_handle = window_state(cx);
        let layout_state = state_handle.borrow();
        let tree_handle = layout_state.tree.clone();
        let layers = tree_handle.borrow();
        let mut originals = Vec::new();
        for (_, _, _, templates) in &slots {
            for template in templates {
                collect_nodes(layers.current(), template, &mut originals);
            }
        }
        originals
            .into_iter()
            .filter(|node| layout_state.views.borrow().contains_key(&node.id))
            .map(|node| node.id)
            .collect::<BTreeSet<_>>()
    };
    for id in template_views {
        drop_copy(cx, &id);
    }
    // A template moved out of an item slot becomes an ordinary view again.
    if let Some(root) = root {
        let _ = create_view(&root, None, window, cx);
    }
    let mut desired = BTreeSet::new();
    let mut active = BTreeSet::new();
    for (owner, slot, declaration, templates) in slots {
        let model = {
            let state_handle = window_state(cx);
            let layout_state = state_handle.borrow();
            let registry = cx.global::<Registry>();
            let tree_handle = layout_state.tree.clone();
            let layers = tree_handle.borrow();
            item_model(
                registry,
                &layout_state,
                layers.current(),
                &owner,
                declaration.model,
            )
        };
        let address = (owner.clone(), slot.clone());
        active.insert(address.clone());
        let mut state = window_state(cx)
            .borrow_mut()
            .item_states
            .remove(&address)
            .unwrap_or_default();
        let slot_label = format!("{owner}.{slot}");
        let subscriptions = window_state(cx)
            .borrow()
            .item_window
            .as_ref()
            .and_then(|(_, weak)| weak.upgrade());
        if let Some(subscriptions) = subscriptions {
            let model_id = model.as_ref().map(AnyEntity::entity_id);
            let previous = subscriptions.borrow().get(&address).map(|(id, _)| *id);
            if previous != model_id {
                subscriptions.borrow_mut().remove(&address);
                if let Some(model) = &model {
                    let subscription = (declaration.observe)(model, window, cx);
                    subscriptions
                        .borrow_mut()
                        .insert(address.clone(), (model.entity_id(), subscription));
                }
            }
        }
        let raw = match model {
            Some(model) => (declaration.read)(&model, cx),
            None => {
                if state.warned.insert("missing model".into()) {
                    log::warn!("item slot {slot_label} has no {} model", declaration.name);
                }
                tracing::trace!(
                    slot = slot_label.as_str(),
                    model = declaration.name,
                    "item slot has no model"
                );
                Vec::new()
            }
        };
        let mut seen = BTreeSet::new();
        let mut keys = Vec::new();
        for key in raw {
            if NodeId::parse(&key).is_err() || !seen.insert(key.clone()) {
                if state.warned.insert(format!("key:{key}")) {
                    log::warn!("item slot {slot_label} skips invalid or repeated key {key:?}");
                }
                tracing::trace!(
                    slot = slot_label.as_str(),
                    key = key.as_str(),
                    "item slot skips invalid or repeated key"
                );
                continue;
            }
            keys.push(key);
        }
        let added = keys
            .iter()
            .filter(|key| !state.keys.contains(key))
            .map(|key| key.as_str())
            .collect::<Vec<_>>()
            .join(",");
        let removed = state
            .keys
            .iter()
            .filter(|key| !keys.contains(key))
            .map(|key| key.as_str())
            .collect::<Vec<_>>()
            .join(",");
        let _change = (keys != state.keys).then(|| {
            tracing::info_span!("items-changed", slot = slot_label.as_str(), added, removed)
                .entered()
        });
        let old_children = window_state(cx)
            .borrow()
            .item_children
            .borrow()
            .get(&address)
            .cloned()
            .unwrap_or_default();
        for old in old_children {
            let key = old.as_str().split_once('@').map(|(_, key)| key);
            if key.is_some_and(|key| !keys.iter().any(|current| current.as_str() == key)) {
                drop_copy(cx, &old);
            }
        }
        let mut children = Vec::new();
        for key in &keys {
            let pending = templates
                .iter()
                .filter_map(|template| {
                    let id = NodeId::copy(&template.id, key).expect("validated item key");
                    desired.insert(id.clone());
                    let state_handle = window_state(cx);
                    let layout_state = state_handle.borrow();
                    let tree_handle = layout_state.tree.clone();
                    let layers = tree_handle.borrow();
                    let shown = !hidden(layers.current(), &template.id);
                    (shown || layout_state.copies.contains_key(&id)).then(|| {
                        let mut node = template.clone();
                        node.id = id;
                        node
                    })
                })
                .collect::<Vec<_>>();
            let new = pending
                .iter()
                .any(|node| !window_state(cx).borrow().copies.contains_key(&node.id));
            let _copy = new.then(|| {
                tracing::info_span!("item-copy", slot = slot_label.as_str(), key = key.as_str())
                    .entered()
            });
            for node in pending {
                let previous = window_state(cx)
                    .borrow_mut()
                    .copies
                    .insert(node.id.clone(), node.clone());
                let _ = create_view(&node, None, window, cx);
                let teller = previous
                    .as_ref()
                    .filter(|old| old.props != node.props)
                    .and_then(|_| window_state(cx).borrow().tellers.get(&node.id).cloned());
                if let Some(teller) = teller {
                    teller(&node.props, cx);
                }
                children.push(node.id);
            }
        }
        // A pending new-item selection is consumed by the next change of
        // the items, and selects the first key that change adds.
        if keys != state.keys
            && window_state(cx)
                .borrow_mut()
                .item_new_pending
                .remove(&owner)
        {
            let first_added = keys
                .iter()
                .filter(|key| !state.keys.contains(key))
                .find_map(|key| {
                    children
                        .iter()
                        .find(|child| {
                            child
                                .as_str()
                                .split_once('@')
                                .is_some_and(|(_, own)| own == key.as_str())
                        })
                        .cloned()
                });
            match first_added {
                Some(copy) => select_new_item(cx, &owner, copy),
                None => tracing::debug!(tabs = %owner, "no new item to select"),
            }
        }
        state.keys = keys;

        let state_handle = window_state(cx);
        let mut layout_state = state_handle.borrow_mut();
        layout_state
            .item_children
            .borrow_mut()
            .insert(address.clone(), children);
        layout_state.item_states.insert(address, state);
    }
    let stale = window_state(cx)
        .borrow()
        .copies
        .keys()
        .filter(|id| !desired.contains(*id))
        .cloned()
        .collect::<Vec<_>>();
    for id in stale {
        drop_copy(cx, &id);
    }

    let state_handle = window_state(cx);
    let mut layout_state = state_handle.borrow_mut();
    if let Some((_, weak)) = &layout_state.item_window
        && let Some(subscriptions) = weak.upgrade()
    {
        subscriptions
            .borrow_mut()
            .retain(|address, _| active.contains(address));
    }
    layout_state
        .item_states
        .retain(|address, _| active.contains(address));
    layout_state
        .item_children
        .borrow_mut()
        .retain(|address, _| active.contains(address));
    // A container that left the tree has no new item to select.
    layout_state
        .item_new_pending
        .retain(|tabs| active.contains(&(tabs.clone(), "children".to_owned())));
    drop(layout_state);
    if notify && before != *window_state(cx).borrow().item_children.borrow() {
        notify_layout(cx);
    }
}

/// The model of the item tabs `tabs`, and its hooks.
fn item_tabs_model(cx: &App, tabs: &NodeId) -> Option<(ItemTabsHooks, AnyEntity)> {
    let state_handle = window_state(cx);
    let layout_state = state_handle.borrow();
    let registry = cx.global::<Registry>();
    let hooks = registry.item_tabs.get(tabs)?.clone();
    let tree_handle = layout_state.tree.clone();
    let layers = tree_handle.borrow();
    match item_model(registry, &layout_state, layers.current(), tabs, hooks.model) {
        Some(model) => Some((hooks, model)),
        None => {
            log::warn!("item tabs {tabs} have no {} model", hooks.name);
            None
        }
    }
}

/// Asks the model of the item tabs `tabs` inside `span`, which lasts while
/// the click is handled, including the effects GPUI runs right after it.
fn ask_item_model(
    tabs: &NodeId,
    span: tracing::Span,
    new: bool,
    window: &mut Window,
    cx: &mut App,
    ask: impl FnOnce(&ItemTabsHooks, &AnyEntity, &mut Window, &mut App),
) {
    let _window_scope = window_scope(window, cx);
    let _group = span.clone().entered();
    let Some((hooks, model)) = item_tabs_model(cx, tabs) else {
        return;
    };
    let address = (tabs.clone(), "children".to_owned());

    let state_handle = window_state(cx);
    let mut layout_state = state_handle.borrow_mut();
    let before = item_keys(&layout_state, &address);
    layout_state.item_group_serial += 1;
    let serial = layout_state.item_group_serial;
    layout_state.item_group = Some(ItemGroup {
        serial,
        span: span.clone(),
        model: model.entity_id(),
    });
    if new {
        // Several clicks before the items change make one pending selection.
        layout_state.item_new_pending.insert(tabs.clone());
    }
    drop(layout_state);
    ask(&hooks, &model, window, cx);
    // Runs after the effects of the click, a notify during the call included.
    let owner = window.window_handle().window_id();
    cx.defer(move |cx| {
        let _owner = owner_scope(owner, cx);
        if no_window(cx) {
            return;
        }

        let state_handle = window_state(cx);
        let mut layout_state = state_handle.borrow_mut();
        if layout_state
            .item_group
            .as_ref()
            .is_some_and(|group| group.serial == serial)
        {
            layout_state.item_group = None;
        }
        if item_keys(&layout_state, &address) == before {
            let _group = span.entered();
            tracing::debug!("items unchanged");
        }
    });
}

fn item_keys(layout_state: &WindowState, address: &(NodeId, String)) -> Vec<SharedString> {
    layout_state
        .item_states
        .get(address)
        .map(|state| state.keys.clone())
        .unwrap_or_default()
}

fn close_item_clicked(tabs: &NodeId, key: &SharedString, window: &mut Window, cx: &mut App) {
    let _window_scope = window_scope(window, cx);
    let span = tracing::info_span!("item-close", tabs = %tabs, key = key.as_str());
    ask_item_model(tabs, span, false, window, cx, |hooks, model, window, cx| {
        let _window_scope = window_scope(window, cx);
        (hooks.close)(model, key, window, cx)
    });
}

fn new_item_clicked(tabs: &NodeId, window: &mut Window, cx: &mut App) {
    let _window_scope = window_scope(window, cx);
    let span = tracing::info_span!("item-new", tabs = %tabs);
    ask_item_model(tabs, span, true, window, cx, |hooks, model, window, cx| {
        let _window_scope = window_scope(window, cx);
        (hooks.new)(model, window, cx)
    });
}

/// Follows an item model's notify, inside the group of the click being
/// handled when that click asked this model.
fn item_model_notified(model: gpui::EntityId, window: &mut Window, cx: &mut App) {
    let _window_scope = window_scope(window, cx);
    let span = window_state(cx)
        .borrow()
        .item_group
        .as_ref()
        .filter(|group| group.model == model)
        .map(|group| group.span.clone());
    let _group = span.map(tracing::Span::entered);
    sync_items(window, cx, true);
    reconcile_containers(cx);
    window.refresh();
}

/// Selects `copy` in the item tabs `tabs`, as Zed activates a newly added item.
fn select_new_item(cx: &mut App, tabs: &NodeId, copy: NodeId) {
    let containers = window_state(cx).borrow().tabs.clone();
    let entity = containers
        .iter()
        .filter_map(WeakEntity::upgrade)
        .find(|entity| entity.read(cx).node == *tabs);
    if let Some(entity) = entity {
        entity.update(cx, |tabs, cx| {
            tabs.reselect = false;
            tabs.select(copy, cx);
        });
    }
}

fn model_name<M: 'static>() -> &'static str {
    type_name::<M>().rsplit("::").next().unwrap()
}

/// Returns the nearest ancestor's model, falling back to the window's.
pub fn context<M: 'static>(cx: &App, node: &str) -> Option<Entity<M>> {
    if no_window(cx) {
        tracing::info!("no window");
        return None;
    }
    let model = model_name::<M>();
    let mut from = String::from("none");
    let result = (|| {
        let state_handle = try_window_state(cx)?;
        let layout_state = state_handle.borrow();
        let tree_handle = layout_state.tree.clone();
        let layers = tree_handle.borrow();
        let tree = layers.current();
        let mut current =
            if let Some(copy) = layout_state.copies.keys().find(|id| id.as_str() == node) {
                template_id(copy)
            } else {
                NodeId::parse(node).ok()?
            };
        node_of(tree, &current)?;
        while let Some((parent, _, _, _)) = current_location(tree, &current) {
            if let Some(value) = layout_state
                .contexts
                .get(&parent)
                .and_then(|models| models.get(&TypeId::of::<M>()))
            {
                from = parent.to_string();
                return value.clone().downcast::<M>().ok();
            }
            current = parent;
        }
        let value = cx
            .global::<Registry>()
            .window_contexts
            .get(&TypeId::of::<M>())?;
        from = String::from("window");
        value.clone().downcast::<M>().ok()
    })();
    tracing::trace!(node, model, from = from.as_str(), "context");
    result
}

fn operation_context_error(
    registry: &Registry,
    layout_state: &WindowState,
    operation: &Operation,
) -> Option<String> {
    match operation {
        Operation::Add { node, parent, .. } => context_rule_error(
            registry,
            layout_state,
            &node.id,
            &node.component,
            parent,
            false,
        ),
        Operation::Move { node, parent, .. } => {
            let tree_handle = layout_state.tree.clone();
            let layers = tree_handle.borrow();
            let component = component_of(layers.current(), node)?;
            context_rule_error(registry, layout_state, node, component, parent, true)
        }
        _ => None,
    }
}

fn context_rule_error(
    registry: &Registry,
    layout_state: &WindowState,
    node: &NodeId,
    component: &ComponentId,
    parent: &NodeId,
    subtree: bool,
) -> Option<String> {
    let tree_handle = layout_state.tree.clone();
    let layers = tree_handle.borrow();
    let tree = layers.current();
    // Let the tree report a duplicate ID; a new node has no descendants.
    if !subtree && node_of(tree, node).is_some() {
        return None;
    }
    // Invalid targets and cycles are refused by the existing tree rules.
    node_of(tree, parent)?;
    let mut available: BTreeSet<TypeId> = registry.window_contexts.keys().copied().collect();
    let mut current = parent.clone();
    loop {
        if &current == node {
            return None;
        }
        if let Some(providers) =
            component_of(tree, &current).and_then(|id| registry.providers.get(id))
        {
            available.extend(providers.iter().map(|provider| provider.model));
        }
        let Some((ancestor, _, _, _)) = current_location(tree, &current) else {
            break;
        };
        current = ancestor;
    }
    fn missing(
        registry: &Registry,
        tree: Option<&Tree>,
        node: &NodeId,
        component: &ComponentId,
        available: &BTreeSet<TypeId>,
        parent: &NodeId,
    ) -> Option<String> {
        if let Some(requirements) = registry.requirements.get(component) {
            for (model, name) in requirements {
                if !available.contains(model) {
                    return Some(format!(
                        "{node} requires a {name} model, and none is above {parent}"
                    ));
                }
            }
        }
        let tree = tree?;
        let mut available = available.clone();
        if let Some(providers) = registry.providers.get(component) {
            available.extend(providers.iter().map(|provider| provider.model));
        }
        tree.slots(node)
            .flat_map(|(_, children)| children)
            .find_map(|child| {
                missing(
                    registry,
                    Some(tree),
                    &child.id,
                    &child.component,
                    &available,
                    parent,
                )
            })
    }
    missing(
        registry,
        subtree.then_some(tree),
        node,
        component,
        &available,
        parent,
    )
}

fn slot_count_rule_error(
    registry: &Registry,
    layout_state: &WindowState,
    parent: &NodeId,
    slot: &SlotName,
    moving: Option<&NodeId>,
) -> Option<String> {
    let tree_handle = layout_state.tree.clone();
    let layers = tree_handle.borrow();
    let tree = layers.current();
    let component = component_of(tree, parent)?;
    if !registry
        .slots_for_one
        .contains(&(component.clone(), slot.clone()))
    {
        return None;
    }
    let held = tree
        .slots(parent)
        .find(|(name, _)| *name == slot)?
        .1
        .first()?;
    if moving == Some(&held.id) {
        return None;
    }
    Some(format!(
        "{parent}.{slot} holds one node, and already holds {}",
        held.id
    ))
}

fn is_copy_id(value: &str) -> bool {
    value.split_once('@').is_some_and(|(template, key)| {
        NodeId::parse(template).is_ok() && NodeId::parse(key).is_ok()
    })
}

fn live_copy(cx: &App, value: &str) -> bool {
    try_window_state(cx).is_some_and(|state_handle| {
        let layout_state = state_handle.borrow();
        layout_state.copies.keys().any(|id| id.as_str() == value)
    })
}

fn operation_copy(operation: &Operation) -> Option<&NodeId> {
    fn at_copy(at: &Position) -> Option<&NodeId> {
        match at {
            Position::Before(node) | Position::After(node) if is_copy_id(node.as_str()) => {
                Some(node)
            }
            _ => None,
        }
    }
    match operation {
        Operation::Add {
            node, parent, at, ..
        } => {
            if is_copy_id(node.id.as_str()) {
                Some(&node.id)
            } else if is_copy_id(parent.as_str()) {
                Some(parent)
            } else {
                at_copy(at)
            }
        }
        Operation::Move {
            node, parent, at, ..
        } => {
            if is_copy_id(node.as_str()) {
                Some(node)
            } else if is_copy_id(parent.as_str()) {
                Some(parent)
            } else {
                at_copy(at)
            }
        }
        Operation::Remove { node }
        | Operation::SetHidden { node, .. }
        | Operation::SetProps { node, .. } => is_copy_id(node.as_str()).then_some(node),
    }
}

fn operation_id(value: &str) -> Result<NodeId, yard_layout::IdError> {
    if let Some((template, key)) = value.split_once('@') {
        NodeId::copy(&NodeId::parse(template)?, key)
    } else {
        NodeId::parse(value)
    }
}

fn check_shell_rules(extension: &str, cx: &mut App, operation: &Operation) -> bool {
    if let Some(node) = operation_copy(operation) {
        tracing::info!("{node} is a copy of an item slot");
        return false;
    }

    let state_handle = window_state(cx);
    let layout_state = state_handle.borrow();
    let registry = cx.default_global::<Registry>();
    if let Operation::Move { node, .. } = operation
        && layout_state.cycle_drop.as_ref() == Some(node)
    {
        tracing::info!("moving a node onto itself would make a cycle");
        return false;
    }
    match operation {
        Operation::Add {
            node, parent, slot, ..
        } => {
            if let Some(reason) = slot_count_rule_error(registry, &layout_state, parent, slot, None)
            {
                tracing::info!("{reason}");
                return false;
            }

            let namespace = node.component.as_str().split('.').next();
            if namespace != Some(extension) && namespace != Some("yard") {
                tracing::info!(
                    node = %node.id,
                    component = %node.component,
                    extension = %extension,
                    "component is not in the extension's namespace",
                );
                return false;
            }
            if is_frame_component(&node.component) || is_frame_slot(parent, slot) {
                tracing::info!(
                    node = %node.id,
                    component = %node.component,
                    parent = %parent,
                    slot = %slot,
                    "the window's frame belongs to the shell",
                );
                return false;
            }
            if !registry.components.contains_key(&node.component) {
                tracing::info!(
                    node = %node.id,
                    component = %node.component,
                    "component is not registered",
                );
                return false;
            }
            let tree_handle = layout_state.tree.clone();
            let layers = tree_handle.borrow();
            let tree = layers.current();
            if let Some(parent_component) = component_of(tree, parent)
                && !registry
                    .components
                    .get(parent_component)
                    .is_some_and(|component| component.slots.contains(slot))
            {
                tracing::info!(
                    node = %node.id,
                    parent = %parent,
                    parent_component = %parent_component,
                    slot = %slot,
                    "parent component does not declare slot",
                );
                return false;
            }
            if let Some(reason) = operation_context_error(registry, &layout_state, operation) {
                tracing::info!("{reason}");
                return false;
            }
            true
        }
        Operation::Move {
            node,
            parent,
            slot,
            at,
        } => {
            if let Some(reason) =
                slot_count_rule_error(registry, &layout_state, parent, slot, Some(node))
            {
                tracing::info!("{reason}");
                return false;
            }
            if is_frame_node(node) || is_frame_slot(parent, slot) {
                tracing::info!(
                    node = %node,
                    parent = %parent,
                    slot = %slot,
                    index = %index_field(at),
                    "the window's frame belongs to the shell",
                );
                return false;
            }
            if layout_state.extension_root.as_ref() == Some(node) {
                tracing::info!(
                    node = %node,
                    parent = %parent,
                    slot = %slot,
                    index = %index_field(at),
                    "the root cannot be moved",
                );
                return false;
            }
            let tree_handle = layout_state.tree.clone();
            let layers = tree_handle.borrow();
            let tree = layers.current();
            if let Some(parent_component) = component_of(tree, parent)
                && !registry
                    .components
                    .get(parent_component)
                    .is_some_and(|component| component.slots.contains(slot))
            {
                tracing::info!(
                    node = %node,
                    parent = %parent,
                    parent_component = %parent_component,
                    slot = %slot,
                    "parent component does not declare slot",
                );
                return false;
            }
            if let Some(reason) = operation_context_error(registry, &layout_state, operation) {
                tracing::info!("{reason}");
                return false;
            }
            true
        }
        Operation::Remove { node } => {
            if is_frame_node(node) {
                tracing::info!(node = %node, "the window's frame belongs to the shell");
                return false;
            }
            if layout_state.extension_root.as_ref() == Some(node) {
                tracing::info!(node = %node, "the root cannot be removed");
                return false;
            }
            if let Some(reason) = required_rule_error(registry, &layout_state, operation) {
                tracing::info!("{reason}");
                return false;
            }
            true
        }
        Operation::SetHidden { node, .. } => {
            if is_frame_node(node) {
                tracing::info!(node = %node, "the window's frame belongs to the shell");
                return false;
            }
            if layout_state.extension_root.as_ref() == Some(node) {
                tracing::info!(node = %node, "the root cannot be hidden or shown");
                return false;
            }
            if let Some(reason) = required_rule_error(registry, &layout_state, operation) {
                tracing::info!("{reason}");
                return false;
            }
            true
        }
        Operation::SetProps { node, .. } => {
            if is_frame_node(node) {
                tracing::info!(node = %node, "the window's frame belongs to the shell");
                return false;
            }
            true
        }
    }
}

fn shell_rule_error(extension: &str, cx: &mut App, operation: &Operation) -> Result<(), String> {
    if let Some(node) = operation_copy(operation) {
        return Err(format!("{node} is a copy of an item slot"));
    }

    let state_handle = window_state(cx);
    let layout_state = state_handle.borrow();
    let registry = cx.default_global::<Registry>();
    match operation {
        Operation::Add {
            node, parent, slot, ..
        } => {
            if let Some(reason) = slot_count_rule_error(registry, &layout_state, parent, slot, None)
            {
                return Err(reason);
            }

            let namespace = node.component.as_str().split('.').next();
            if namespace != Some(extension) && namespace != Some("yard") {
                return Err("component is not in the extension's namespace".into());
            }
            if is_frame_component(&node.component) || is_frame_slot(parent, slot) {
                return Err("the window's frame belongs to the shell".into());
            }
            if !registry.components.contains_key(&node.component) {
                return Err("component is not registered".into());
            }
            let tree_handle = layout_state.tree.clone();
            let layers = tree_handle.borrow();
            let tree = layers.current();
            if let Some(parent_component) = component_of(tree, parent)
                && !registry
                    .components
                    .get(parent_component)
                    .is_some_and(|component| component.slots.contains(slot))
            {
                return Err("parent component does not declare slot".into());
            }
            operation_context_error(registry, &layout_state, operation).map_or(Ok(()), Err)
        }
        Operation::Move {
            node, parent, slot, ..
        } => {
            if let Some(reason) =
                slot_count_rule_error(registry, &layout_state, parent, slot, Some(node))
            {
                return Err(reason);
            }
            if is_frame_node(node) || is_frame_slot(parent, slot) {
                return Err("the window's frame belongs to the shell".into());
            }
            if layout_state.extension_root.as_ref() == Some(node) {
                return Err("the root cannot be removed, moved, hidden, or shown".into());
            }
            let tree_handle = layout_state.tree.clone();
            let layers = tree_handle.borrow();
            let tree = layers.current();
            if let Some(parent_component) = component_of(tree, parent)
                && !registry
                    .components
                    .get(parent_component)
                    .is_some_and(|component| component.slots.contains(slot))
            {
                return Err("parent component does not declare slot".into());
            }
            operation_context_error(registry, &layout_state, operation).map_or(Ok(()), Err)
        }
        Operation::Remove { node } | Operation::SetHidden { node, .. } => {
            if is_frame_node(node) {
                return Err("the window's frame belongs to the shell".into());
            }
            if layout_state.extension_root.as_ref() == Some(node) {
                return Err("the root cannot be removed, moved, hidden, or shown".into());
            }
            required_rule_error(registry, &layout_state, operation).map_or(Ok(()), Err)
        }
        Operation::SetProps { node, .. } => {
            if is_frame_node(node) {
                return Err("the window's frame belongs to the shell".into());
            }
            Ok(())
        }
    }
}

/// Why a `hide` or `remove` would take a node of a required component off
/// screen, if it would. The default layout is not checked.
fn required_rule_error(
    registry: &Registry,
    layout_state: &WindowState,
    operation: &Operation,
) -> Option<String> {
    let (node, verb) = match operation {
        Operation::Remove { node } => (node, "removed"),
        Operation::SetHidden { node, hidden: true } => (node, "hidden"),
        _ => return None,
    };
    if !layout_state.default_fixed || registry.required.is_empty() {
        return None;
    }
    fn first_required<'a>(registry: &Registry, tree: &'a Tree, node: &'a Node) -> Option<&'a Node> {
        if registry.required.contains(&node.component) {
            return Some(node);
        }
        tree.slots(&node.id)
            .flat_map(|(_, children)| children)
            .find_map(|child| first_required(registry, tree, child))
    }
    let tree_handle = layout_state.tree.clone();
    let layers = tree_handle.borrow();
    let tree = layers.current();
    let required = first_required(registry, tree, node_of(tree, node)?)?;
    Some(if &required.id == node {
        format!("{node} cannot be {verb}: it is required")
    } else {
        format!(
            "{node} cannot be {verb}: it holds the required node {}",
            required.id
        )
    })
}

#[derive(Default)]
struct UrlRouting {
    scheme: String,
    handlers: BTreeMap<String, UrlHandler>,
    pending: VecDeque<Vec<String>>,
    extensions_loaded: bool,
    dispatching: bool,
}

/// Sets the product's URL scheme.
pub fn set_url_scheme(cx: &mut App, scheme: &str) {
    cx.default_global::<Registry>().urls.scheme = scheme.to_owned();
}

/// Queues URLs received from the platform and dispatches them when ready.
pub fn open_urls(cx: &mut App, urls: Vec<String>) {
    if !cx.default_global::<Registry>().urls.extensions_loaded {
        let _group = tracing::info_span!("urls-open", count = urls.len()).entered();
        for url in &urls {
            let _group = url_span(cx, url_parts(url)).entered();
            tracing::debug!("kept until extensions load");
        }
    }
    cx.global_mut::<Registry>().urls.pending.push_back(urls);
    drain_urls(cx);
}

/// Hands over URLs that arrived before extension loading finished.
pub fn extensions_loaded(cx: &mut App) {
    let routing = &mut cx.default_global::<Registry>().urls;
    if routing.extensions_loaded {
        return;
    }
    routing.extensions_loaded = true;
    let count: usize = routing.pending.iter().map(Vec::len).sum();
    if !routing.pending.is_empty() {
        let _group = tracing::info_span!("urls-handover").entered();
        tracing::debug!(count, "handed over");
        drain_urls(cx);
    }
}

fn drain_urls(cx: &mut App) {
    let routing = &mut cx.global_mut::<Registry>().urls;
    if !routing.extensions_loaded || routing.dispatching {
        return;
    }
    // Calls from a handler append to the queue. The outer drain finishes
    // this batch and any older queued batches before it reaches those URLs.
    routing.dispatching = true;
    while let Some(batch) = cx.global_mut::<Registry>().urls.pending.pop_front() {
        let _group = tracing::info_span!("urls-open", count = batch.len()).entered();
        for url in batch {
            dispatch_url(cx, &url);
        }
    }
    cx.global_mut::<Registry>().urls.dispatching = false;
}

fn dispatch_url(cx: &mut App, url: &str) {
    let parts = url_parts(url);
    let _group = url_span(cx, parts).entered();
    match url_handler(cx, parts) {
        Ok(handler) => {
            handler(url, cx);
            tracing::debug!("handled");
        }
        Err(reason) => tracing::info!(%reason, "refused"),
    }
}

fn url_handler(cx: &App, parts: Option<(&str, &str)>) -> Result<UrlHandler, String> {
    let (scheme, host) = parts.ok_or("invalid URL")?;
    let registry = cx.global::<Registry>();
    if !scheme.eq_ignore_ascii_case(&registry.urls.scheme) {
        return Err("wrong scheme".into());
    }
    if host.is_empty() {
        return Err("no host".into());
    }
    let name = host.to_ascii_lowercase();
    if !registry.extensions.contains(&name) {
        // A host that names no extension may be anything, a secret too.
        return Err("no such extension".into());
    }
    registry
        .urls
        .handlers
        .get(&name)
        .cloned()
        .ok_or_else(|| format!("no handler for extension {host}"))
}

/// The group of one URL. It names the host only when the host names a
/// registered extension, since any other host may be anything, a secret too.
fn url_span(cx: &App, parts: Option<(&str, &str)>) -> tracing::Span {
    let (scheme, host) = parts.unwrap_or_default();
    let known = cx
        .try_global::<Registry>()
        .is_some_and(|registry| registry.extensions.contains(&host.to_ascii_lowercase()));
    if known {
        tracing::info_span!("url-open", scheme, host)
    } else {
        tracing::info_span!("url-open", scheme)
    }
}

fn url_parts(url: &str) -> Option<(&str, &str)> {
    let (scheme, rest) = url.split_once(':')?;
    let mut scheme_bytes = scheme.bytes();
    if !scheme_bytes.next()?.is_ascii_alphabetic()
        || !scheme_bytes.all(|byte| byte.is_ascii_alphanumeric() || b"+-.".contains(&byte))
        || url.chars().any(|character| {
            character.is_whitespace() || character.is_control() || "<>\"{}|\\^`".contains(character)
        })
        || url.bytes().enumerate().any(|(index, byte)| {
            byte == b'%'
                && !url
                    .as_bytes()
                    .get(index + 1..index + 3)
                    .is_some_and(|escape| escape.iter().all(u8::is_ascii_hexdigit))
        })
    {
        return None;
    }
    let Some(rest) = rest.strip_prefix("//") else {
        return Some((scheme, ""));
    };
    let authority = rest.split(['/', '?', '#']).next()?;
    let host_port = match authority.split_once('@') {
        Some((user, host_port)) if user.bytes().all(|byte| name_byte(byte) || byte == b':') => {
            host_port
        }
        Some(_) => return None,
        None => authority,
    };
    let (host, port) = if let Some(literal) = host_port.strip_prefix('[') {
        let (address, suffix) = literal.split_once(']')?;
        address.parse::<std::net::Ipv6Addr>().ok()?;
        let port = if suffix.is_empty() {
            ""
        } else {
            suffix.strip_prefix(':')?
        };
        (&host_port[..address.len() + 2], port)
    } else {
        let (host, port) = host_port.split_once(':').unwrap_or((host_port, ""));
        if !host.bytes().all(name_byte) {
            return None;
        }
        (host, port)
    };
    if !port.bytes().all(|byte| byte.is_ascii_digit())
        || (!port.is_empty() && port.parse::<u16>().is_err())
    {
        return None;
    }
    Some((scheme, host))
}

fn name_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || b"-._~!$&'()*+,;=%".contains(&byte)
}

/// The application-assigned namespace for an extension's contributions.
#[derive(Clone)]
pub struct Extension {
    name: String,
}

impl Extension {
    /// Registers the handler for URLs that name this extension.
    pub fn on_open_url(&self, cx: &mut App, handler: impl Fn(&str, &mut App) + 'static) {
        let _group =
            tracing::info_span!("url-handler-register", extension = self.name.as_str()).entered();
        let handlers = &mut cx.default_global::<Registry>().urls.handlers;
        match handlers.entry(self.name.clone()) {
            Entry::Vacant(entry) => {
                entry.insert(Rc::new(handler));
                tracing::debug!("registered");
            }
            Entry::Occupied(entry) => {
                log::error!("extension {} already has a URL handler", entry.key());
            }
        }
    }

    /// Creates an extension handle. Called only by the startup.
    pub fn new(cx: &mut App, name: &str) -> Option<Extension> {
        if name == "yard" {
            log::error!("yard is the shell's own namespace");
            return None;
        }
        // An extension name follows the same slug rule as a node ID.
        if let Err(error) = NodeId::parse(name) {
            log::error!("invalid extension name {name:?}: {error}");
            return None;
        }
        if !cx
            .default_global::<Registry>()
            .extensions
            .insert(name.into())
        {
            log::error!("extension {name} is already initialized");
            return None;
        }
        Some(Self { name: name.into() })
    }

    /// Reads a value from this extension's store.
    pub fn stored(&self, cx: &mut App, key: &str) -> Option<serde_json::Value> {
        tracing::trace!(key, "stored");
        if !valid_store_key(key) {
            return None;
        }
        extension_store(cx, &self.name).values.get(key).cloned()
    }

    /// Sets a value in this extension's store and schedules a save.
    pub fn store(&self, cx: &mut App, key: &str, value: serde_json::Value) {
        tracing::trace!(key, "store");
        if !valid_store_key(key) {
            return;
        }
        let values = &mut extension_store(cx, &self.name).values;
        if values.get(key) == Some(&value) {
            return;
        }
        values.insert(key.into(), value);
        store_changed(cx, &self.name);
    }

    /// Removes a value from this extension's store if it exists.
    pub fn unstore(&self, cx: &mut App, key: &str) {
        tracing::trace!(key, "unstore");
        if valid_store_key(key) && extension_store(cx, &self.name).values.remove(key).is_some() {
            store_changed(cx, &self.name);
        }
    }

    /// Registers operations to apply over the startup default when shown.
    pub fn named_layout(&self, cx: &mut App, name: &str, build: impl FnOnce(&mut NamedLayout)) {
        // Validate before running extension code, and keep refusals in their group.
        let refusal = if name == "yard.default" {
            Some("yard.default is reserved".to_string())
        } else if ComponentId::parse(name).is_err() {
            Some(format!("invalid named layout ID {name:?}"))
        } else if !name.starts_with(&format!("{}.", self.name)) {
            Some(format!(
                "named layout {name} is outside extension {}'s namespace",
                self.name
            ))
        } else if cx
            .default_global::<Registry>()
            .named_layouts
            .contains_key(name)
        {
            Some(format!("named layout {name} is already registered"))
        } else {
            None
        };
        if let Some(reason) = refusal {
            let _group = tracing::info_span!(
                "named-layout-register",
                name,
                extension = self.name.as_str(),
                operations = 0usize
            )
            .entered();
            log::error!("{reason}");
            return;
        }
        let mut layout = NamedLayout {
            extension: self.name.clone(),
            operations: Vec::new(),
        };
        build(&mut layout);
        let _group = tracing::info_span!(
            "named-layout-register",
            name,
            extension = self.name.as_str(),
            operations = layout.operations.len()
        )
        .entered();
        cx.default_global::<Registry>()
            .named_layouts
            .insert(name.into(), layout);
        tracing::debug!("registered");
    }

    /// Shows a registered layout with its own saved user layer.
    pub fn show_layout(&self, window: &mut Window, cx: &mut App, name: &str) {
        let _window_scope = window_scope(window, cx);
        let from = shown_layout(cx, window);
        let _group =
            tracing::info_span!("layout-switch", from = from.as_str(), to = name).entered();
        if no_window(cx) {
            tracing::info!("no window");
            return;
        }
        if from.as_str() == name {
            tracing::debug!("already shown");
            return;
        }
        let named = cx
            .default_global::<Registry>()
            .named_layouts
            .get(name)
            .cloned();
        if name != "yard.default" && named.is_none() {
            tracing::info!(name, "no named layout");
            return;
        }
        let focused = focused_node(window, cx);
        let (before, before_props, before_components) = {
            let state_handle = window_state(cx);
            let mut layout_state = state_handle.borrow_mut();
            let before = layout_state
                .views
                .borrow()
                .iter()
                .map(|(id, view)| (id.clone(), view.entity_id()))
                .collect::<BTreeMap<_, _>>();
            let tree_handle = layout_state.tree.clone();
            let layers = tree_handle.borrow();
            let mut nodes = Vec::new();
            if let Some(root) = layers.current().root() {
                collect_nodes(layers.current(), root, &mut nodes);
            }
            let props = nodes
                .into_iter()
                .map(|node| (node.id, node.props))
                .collect::<BTreeMap<_, _>>();
            let components = before
                .keys()
                .filter_map(|id| {
                    node_of(layers.current(), &template_id(id))
                        .map(|node| (id.clone(), (node.component.clone(), node.extension.clone())))
                })
                .collect::<BTreeMap<_, _>>();
            // The main window reads its layers from its files; the copy
            // kept here is what the session saves for it.
            layout_state
                .saved_layers
                .insert(from.to_string(), layers.user_layer().to_vec());
            (before, props, components)
        };
        let zoom_cleared;
        {
            let state_handle = window_state(cx);
            let mut layout_state = state_handle.borrow_mut();
            let registry = cx.default_global::<Registry>();
            layout_state.failed_views.clear();
            zoom_cleared = layout_state.zoomed.take().is_some();
            layout_state.shown_layout = name.to_string().into();
            layout_state.named_nodes.clear();
            *layout_state.tree.borrow_mut() = Layers::new(layout_state.startup_tree.clone());
            if name != "yard.default" && registry.named_layout_files.is_none() {
                if !layout_state.warned_named_files {
                    log::warn!("named layout files are not set; user layers live in memory only");
                    layout_state.warned_named_files = true;
                }
                tracing::debug!("named layout user layer is in memory");
            }
        }
        if let Some(named) = named {
            for operation in named.operations {
                match operation {
                    Ok(operation) => {
                        let added = match &operation {
                            Operation::Add { node, .. } => Some(node.id.clone()),
                            _ => None,
                        };
                        if replay_operation(cx, &named.extension, operation)
                            && let Some(id) = added
                        {
                            window_state(cx).borrow_mut().named_nodes.insert(id);
                        }
                    }
                    Err(reason) => tracing::info!(reason, "skipped"),
                }
            }
        }
        window_state(cx).borrow().tree.borrow_mut().set_default();
        let session_file = window_state(cx)
            .borrow()
            .session_layers
            .as_ref()
            .filter(|(_, given)| given.contains(name))
            .map(|(file, _)| file.clone());
        if let Some(file) = session_file {
            window_state(cx).borrow_mut().item_new_pending.clear();
            let saved = window_state(cx)
                .borrow()
                .saved_layers
                .get(name)
                .cloned()
                .unwrap_or_default();
            let _group = tracing::info_span!("layout-restore", file = %file.display()).entered();
            apply_restored(cx, saved);
        } else if layout_path(cx.global::<Registry>(), &window_state(cx).borrow()).is_some() {
            restore(cx);
        } else {
            window_state(cx).borrow_mut().item_new_pending.clear();
            let saved = window_state(cx)
                .borrow()
                .saved_layers
                .get(name)
                .cloned()
                .unwrap_or_default();
            for operation in saved {
                let extension = match &operation {
                    Operation::Add { node, .. } => node.extension.clone(),
                    _ => "yard".into(),
                };
                replay_operation(cx, &extension, operation);
            }
        }
        let (root, removed) = {
            let state_handle = window_state(cx);
            let layout_state = state_handle.borrow();
            let tree_handle = layout_state.tree.clone();
            let layers = tree_handle.borrow();
            let tree = layers.current();
            let removed = layout_state
                .views
                .borrow()
                .keys()
                .filter(|id| {
                    node_of(tree, &template_id(id)).is_none_or(|node| {
                        before_components.get(*id)
                            != Some(&(node.component.clone(), node.extension.clone()))
                    })
                })
                .cloned()
                .collect::<Vec<_>>();
            (tree.root().cloned(), removed)
        };
        for id in removed {
            drop_copy(cx, &id);
        }
        if let Some(root) = root {
            let _ = create_view(&root, None, window, cx);
        }
        let to_tell = {
            let state_handle = window_state(cx);
            let layout_state = state_handle.borrow();
            let tree_handle = layout_state.tree.clone();
            let layers = tree_handle.borrow();
            let mut nodes = Vec::new();
            if let Some(root) = layers.current().root() {
                collect_nodes(layers.current(), root, &mut nodes);
            }
            nodes
                .into_iter()
                .filter(|node| {
                    before.contains_key(&node.id) && before_props.get(&node.id) != Some(&node.props)
                })
                .filter_map(|node| {
                    layout_state
                        .tellers
                        .get(&node.id)
                        .cloned()
                        .map(|teller| (teller, node.props))
                })
                .collect::<Vec<_>>()
        };
        for (teller, props) in to_tell {
            teller(&props, cx);
        }
        sync_items(window, cx, false);
        reconcile_containers(cx);
        let focus_kept = focused.is_some_and(|node| {
            find_in_drawn(
                cx.global::<Registry>(),
                &window_state(cx).borrow(),
                cx,
                |shown| (shown == &node).then_some(()),
            )
            .is_some()
        });
        if !focus_kept && let Some(handle) = window_state(cx).borrow().root_focus_handle.clone() {
            handle.focus(window, cx);
        }
        let after = window_state(cx)
            .borrow()
            .views
            .borrow()
            .iter()
            .map(|(id, view)| (id.clone(), view.entity_id()))
            .collect::<BTreeMap<_, _>>();
        let kept = before
            .iter()
            .filter(|(id, entity)| after.get(*id) == Some(*entity))
            .count();
        let dropped = before.len() - kept;
        let created = after.len() - kept;
        tracing::debug!(kept, dropped, created, "switched");
        if zoom_cleared {
            state_changed(cx);
        }
        notify_layout(cx);
        super::session::layout_changed(cx);
        window.refresh();
        cx.refresh_windows();
    }

    /// Adds a new node at the end of a parent's declared slot and creates its view.
    pub fn add(
        &self,
        window: &mut Window,
        cx: &mut App,
        node: &str,
        component: &str,
        parent: &str,
        slot: &str,
    ) {
        let _window_scope = window_scope(window, cx);
        self.add_internal(
            window,
            cx,
            node,
            component,
            parent,
            slot,
            Ok(Props::new()),
            Position::Last,
        );
    }

    /// Adds a new node at a position in a parent's declared slot and creates its view.
    #[allow(clippy::too_many_arguments)]
    pub fn add_at(
        &self,
        window: &mut Window,
        cx: &mut App,
        node: &str,
        component: &str,
        parent: &str,
        slot: &str,
        at: Position,
    ) {
        let _window_scope = window_scope(window, cx);
        self.add_internal(
            window,
            cx,
            node,
            component,
            parent,
            slot,
            Ok(Props::new()),
            at,
        );
    }

    /// Adds a new node with the props and at the position of `placement`, and creates its view.
    #[allow(clippy::too_many_arguments)]
    pub fn add_with(
        &self,
        window: &mut Window,
        cx: &mut App,
        node: &str,
        component: &str,
        parent: &str,
        slot: &str,
        placement: Placement,
    ) {
        let _window_scope = window_scope(window, cx);
        self.add_internal(
            window,
            cx,
            node,
            component,
            parent,
            slot,
            placement_props(placement.props),
            placement.at,
        );
    }

    /// Adds a new node with props at the end of a parent's declared slot and creates its view.
    #[allow(clippy::too_many_arguments)]
    pub fn add_with_props(
        &self,
        window: &mut Window,
        cx: &mut App,
        node: &str,
        component: &str,
        parent: &str,
        slot: &str,
        props: impl serde::Serialize,
    ) {
        let _window_scope = window_scope(window, cx);
        self.add_internal(
            window,
            cx,
            node,
            component,
            parent,
            slot,
            encode_props(props),
            Position::Last,
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn add_internal(
        &self,
        window: &mut Window,
        cx: &mut App,
        node: &str,
        component: &str,
        parent: &str,
        slot: &str,
        props: Result<Props, String>,
        at: Position,
    ) {
        let _window_scope = window_scope(window, cx);
        run_op(
            &self.name,
            Some(window),
            cx,
            "add",
            node,
            |extension, id| {
                let props = props?;
                let component = ComponentId::parse(component)
                    .map_err(|error| format!("invalid component ID {component:?}: {error}"))?;
                let parent = operation_id(parent)
                    .map_err(|error| format!("invalid parent ID {parent:?}: {error}"))?;
                let slot = SlotName::parse(slot)
                    .map_err(|error| format!("invalid slot name {slot:?}: {error}"))?;
                Ok(Operation::Add {
                    node: Node {
                        id,
                        component,
                        extension,
                        props,
                    },
                    parent,
                    slot,
                    at,
                })
            },
        );
    }

    /// Sets or clears keys on any node's props and tells its view.
    pub fn set_props(&self, cx: &mut App, node: &str, props: impl serde::Serialize) {
        let props = encode_props(props);
        run_op(&self.name, None, cx, "props", node, |_, node| {
            Ok(Operation::SetProps {
                node,
                props: props?,
            })
        });
    }

    /// Removes a non-root node and its subtree, dropping their views.
    pub fn remove(&self, cx: &mut App, node: &str) {
        run_op(&self.name, None, cx, "remove", node, |_, node| {
            Ok(Operation::Remove { node })
        });
    }

    /// Moves a non-root node with its subtree into a declared slot at `index`.
    pub fn move_node(&self, cx: &mut App, node: &str, parent: &str, slot: &str, index: usize) {
        self.move_to(cx, node, parent, slot, Position::Index(index));
    }

    /// Moves a non-root node with its subtree to a position in a declared slot.
    pub fn move_to(&self, cx: &mut App, node: &str, parent: &str, slot: &str, at: Position) {
        run_op(&self.name, None, cx, "move", node, |_, node| {
            let parent = operation_id(parent)
                .map_err(|error| format!("invalid parent ID {parent:?}: {error}"))?;
            let slot = SlotName::parse(slot)
                .map_err(|error| format!("invalid slot name {slot:?}: {error}"))?;
            Ok(Operation::Move {
                node,
                parent,
                slot,
                at,
            })
        });
    }

    /// Hides or shows any non-root node while retaining its view.
    pub fn set_hidden(&self, cx: &mut App, node: &str, hidden: bool) {
        let op = if hidden { "hide" } else { "show" };
        run_op(&self.name, None, cx, op, node, |_, node| {
            Ok(Operation::SetHidden { node, hidden })
        });
    }

    /// Zooms any shown non-root node without changing the layout tree.
    pub fn set_zoomed(&self, cx: &mut App, node: &str) {
        let _window_scope = selected_scope(cx);
        let _group = tracing::info_span!("zoom", node, extension = self.name.as_str()).entered();
        if no_window(cx) {
            tracing::info!("no window");
            return;
        }
        if is_copy_id(node) || live_copy(cx, node) {
            tracing::info!("{node} is a copy of an item slot");
            return;
        }
        let id = match NodeId::parse(node) {
            Ok(id) => id,
            Err(error) => {
                log::error!("invalid node ID {node:?}: {error}");
                return;
            }
        };

        let state_handle = window_state(cx);
        let mut layout_state = state_handle.borrow_mut();
        if let Some(reason) = zoom_refusal(&layout_state, &id) {
            tracing::info!("{reason}");
            return;
        }
        let previous = layout_state.zoomed.replace(id.clone());
        if let Some(previous) = &previous
            && *previous != id
        {
            tracing::debug!(previous = %previous, "replaced");
        }
        tracing::debug!(node, "zoomed");
        drop(layout_state);
        cx.refresh_windows();
        if previous.as_ref() != Some(&id) {
            view_state_changed(cx);
        }
    }

    /// Restores the tree with its existing views, shares, and tab selection.
    pub fn zoom_out(&self, cx: &mut App) {
        let _window_scope = selected_scope(cx);
        let _group = tracing::info_span!("zoom-out", extension = self.name.as_str()).entered();
        if no_window(cx) {
            tracing::info!("no window");
            return;
        }
        let Some(node) = window_state(cx).borrow_mut().zoomed.take() else {
            tracing::debug!("nothing zoomed");
            return;
        };
        tracing::debug!(node = %node, "zoomed out");
        cx.refresh_windows();
        view_state_changed(cx);
    }

    /// Drops runtime operations and reconciles the default tree and view state.
    pub fn reset_layout(&self, window: &mut Window, cx: &mut App) {
        let _window_scope = window_scope(window, cx);
        let _group = tracing::info_span!("layout-reset", extension = self.name.as_str()).entered();
        if no_window(cx) {
            tracing::info!("no window");
            return;
        }
        window_state(cx).borrow_mut().item_new_pending.clear();
        let (added, before_props) = {
            let state_handle = window_state(cx);
            let layout_state = state_handle.borrow();
            let tree_handle = layout_state.tree.clone();
            let layers = tree_handle.borrow();
            let added: BTreeSet<_> = layers
                .user_layer()
                .iter()
                .filter_map(|op| match op {
                    Operation::Add { node, .. } => Some(node.id.clone()),
                    _ => None,
                })
                .collect();
            let mut before = Vec::new();
            if let Some(root) = layers.current().root() {
                collect_nodes(layers.current(), root, &mut before);
            }
            let before_props: BTreeMap<NodeId, Props> = before
                .into_iter()
                .map(|node| (node.id, node.props))
                .collect();
            (added, before_props)
        };
        if window_state(cx).borrow().tree.borrow_mut().reset() == 0 {
            return;
        }
        let (root, kept, dropped) = {
            let state_handle = window_state(cx);
            let mut layout_state = state_handle.borrow_mut();
            layout_state.failed_views.clear();
            let tree_handle = layout_state.tree.clone();
            let layers = tree_handle.borrow();
            let tree = layers.current();
            let removed: Vec<_> = layout_state
                .views
                .borrow()
                .keys()
                .filter(|id| added.contains(*id) || component_of(tree, &template_id(id)).is_none())
                .cloned()
                .collect();
            let mut dropped = Vec::new();
            for id in removed {
                layout_state.focus_handles.remove(&id);
                layout_state.bounds.remove(&id);
                layout_state.tellers.remove(&id);
                layout_state.contexts.remove(&id);
                layout_state.emptiness.borrow_mut().remove(&id);
                layout_state.empty_errors.borrow_mut().remove(&id);
                layout_state.laid_out.borrow_mut().remove(&id);
                dropped.push((id.clone(), layout_state.views.borrow_mut().remove(&id)));
            }
            let kept: BTreeSet<_> = layout_state.views.borrow().keys().cloned().collect();
            (tree.root().cloned(), kept, dropped)
        };
        for (id, view) in dropped {
            drop(view);
            tracing::debug!(node = %id, "view dropped");
        }
        if let Some(root) = root {
            let _ = create_view(&root, None, window, cx);
        }
        let to_tell: Vec<(Teller, Props)> = {
            let state_handle = window_state(cx);
            let layout_state = state_handle.borrow();
            let tree_handle = layout_state.tree.clone();
            let layers = tree_handle.borrow();
            let tree = layers.current();
            let mut after = Vec::new();
            if let Some(root) = tree.root() {
                collect_nodes(tree, root, &mut after);
            }
            after
                .into_iter()
                .filter(|node| {
                    kept.contains(&node.id) && before_props.get(&node.id) != Some(&node.props)
                })
                .filter_map(|node| {
                    layout_state
                        .tellers
                        .get(&node.id)
                        .cloned()
                        .map(|teller| (teller, node.props.clone()))
                })
                .collect()
        };
        for (teller, props) in to_tell {
            teller(&props, cx);
        }
        sync_items(window, cx, false);
        let (splits, tabs, zoom_cleared) = {
            let state_handle = window_state(cx);
            let mut layout_state = state_handle.borrow_mut();
            layout_state
                .splits
                .retain(|split| split.upgrade().is_some());
            layout_state.tabs.retain(|tabs| tabs.upgrade().is_some());
            let zoom_cleared = layout_state.zoomed.take().is_some();
            (
                layout_state.splits.clone(),
                layout_state.tabs.clone(),
                zoom_cleared,
            )
        };
        for split in splits {
            // The shares stay as read from the props, which the tellers updated.
            let _ = split.update(cx, |split, cx| {
                split.shown_nodes.clear();
                split.shown_shares = Rc::new(RefCell::new(Vec::new()));
                split.shown_given.clear();
                split.reconcile(cx, Emptiness::Remembered);
            });
        }
        for tabs in tabs {
            let _ = tabs.update(cx, |tabs, cx| {
                tabs.choose_first(cx);
            });
        }
        if zoom_cleared {
            state_changed(cx);
        }
        cx.refresh_windows();
        save(cx);
        notify_layout(cx);
    }

    /// Registers the first factory for a valid component ID.
    pub fn register(
        &self,
        cx: &mut App,
        component: &str,
        slots: &[&str],
        create: impl Fn(&NodeId, Slots, &mut Window, &mut App) -> AnyView + 'static,
    ) {
        self.register_factory(
            cx,
            component,
            slots,
            Rc::new(move |node, slots, window, cx| create(&node.id, slots, window, cx)),
        );
    }

    /// Gives a model from each node of this component to its descendants.
    pub fn provide<M: 'static>(
        &self,
        cx: &mut App,
        component: &str,
        create: impl Fn(&NodeId, &mut App) -> Entity<M> + 'static,
    ) {
        let model = model_name::<M>();
        let _group =
            tracing::info_span!("context-declare", component, model, kind = "provide").entered();
        let Some(component) = self.context_component(component) else {
            return;
        };
        let providers = cx
            .default_global::<Registry>()
            .providers
            .entry(component.clone())
            .or_default();
        if providers
            .iter()
            .any(|provider| provider.model == TypeId::of::<M>())
        {
            log::error!("component {component} already provides a {model} model");
            return;
        }
        providers.push(Provider {
            model: TypeId::of::<M>(),
            name: model,
            create: Rc::new(move |node, cx| create(node, cx).into()),
        });
        tracing::debug!("declared");
    }

    /// Gives a model to the whole window, retaining the first of its type.
    pub fn provide_window<M: 'static>(&self, cx: &mut App, model: Entity<M>) {
        let name = model_name::<M>();
        let _group = tracing::info_span!(
            "context-declare",
            component = "window",
            model = name,
            kind = "window"
        )
        .entered();
        match cx
            .default_global::<Registry>()
            .window_contexts
            .entry(TypeId::of::<M>())
        {
            Entry::Vacant(entry) => {
                entry.insert(model.into());
                tracing::debug!("declared");
            }
            Entry::Occupied(_) => log::error!("the window already provides a {name} model"),
        }
    }

    /// Requires a model above each node of this component.
    pub fn require_context<M: 'static>(&self, cx: &mut App, component: &str) {
        let model = model_name::<M>();
        let _group =
            tracing::info_span!("context-declare", component, model, kind = "require").entered();
        let Some(component) = self.context_component(component) else {
            return;
        };
        let requirements = cx
            .default_global::<Registry>()
            .requirements
            .entry(component.clone())
            .or_default();
        if requirements.iter().any(|(id, _)| *id == TypeId::of::<M>()) {
            log::error!("component {component} already requires a {model} model");
            return;
        }
        requirements.push((TypeId::of::<M>(), model));
        tracing::debug!("declared");
    }

    fn context_component(&self, component: &str) -> Option<ComponentId> {
        let id = match ComponentId::parse(component) {
            Ok(id) => id,
            Err(error) => {
                log::error!("invalid component ID {component:?}: {error}");
                return None;
            }
        };
        if id.as_str().split('.').next() != Some(self.name.as_str()) {
            log::error!(
                "extension {} cannot declare contexts for component {component}",
                self.name
            );
            return None;
        }
        Some(id)
    }

    /// Declares a slot that repeats its templates for the keys of `M`.
    pub fn item_slot<M: Items>(&self, cx: &mut App, node: &str, slot: &str) {
        let model = model_name::<M>();
        let _group = tracing::info_span!("item-slot-declare", node, slot, model).entered();
        if self.declare_item_slot::<M>(cx, node, slot, false) {
            schedule_registration_items(cx);
        }
    }

    /// Declares the slot `children` of the `yard.tabs` node `node` an item
    /// slot over `M`, whose tabs close and add items through the model.
    pub fn item_tabs<M: ItemTabs>(&self, cx: &mut App, node: &str) {
        let model = model_name::<M>();
        let slot = "children";
        let _group = tracing::info_span!("item-slot-declare", node, slot, model).entered();
        if !self.declare_item_slot::<M>(cx, node, slot, true) {
            return;
        }
        let id = NodeId::parse(node).expect("declared item slot");
        cx.default_global::<Registry>().item_tabs.insert(
            id,
            ItemTabsHooks {
                model: TypeId::of::<M>(),
                name: model,
                close: Rc::new(|entity, key, window, cx| {
                    let _window_scope = window_scope(window, cx);
                    let entity = entity.clone().downcast::<M>().expect("declared item model");
                    entity.update(cx, |model, cx| model.close_item(key, window, cx));
                }),
                new: Rc::new(|entity, window, cx| {
                    let _window_scope = window_scope(window, cx);
                    let entity = entity.clone().downcast::<M>().expect("declared item model");
                    entity.update(cx, |model, cx| model.new_item(window, cx));
                }),
            },
        );
        tracing::debug!("item tabs");
        schedule_registration_items(cx);
    }

    /// Declares an item slot inside the caller's group; `tabs` also requires
    /// the node to be a `yard.tabs`. Returns whether it was declared.
    fn declare_item_slot<M: Items>(
        &self,
        cx: &mut App,
        node: &str,
        slot: &str,
        tabs: bool,
    ) -> bool {
        let model = model_name::<M>();
        let id = match NodeId::parse(node) {
            Ok(id) => id,
            Err(error) => {
                log::error!("invalid node ID {node:?}: {error}");
                return false;
            }
        };
        let name = match SlotName::parse(slot) {
            Ok(name) => name,
            Err(error) => {
                log::error!("invalid slot name {slot:?}: {error}");
                return false;
            }
        };

        let state_handle = window_state(cx);
        let layout_state = state_handle.borrow();
        let registry = cx.default_global::<Registry>();
        {
            let tree_handle = layout_state.tree.clone();
            let layers = tree_handle.borrow();
            let Some(placed) = node_of(layers.current(), &id) else {
                log::error!("node {node} is not in the tree");
                return false;
            };
            if placed.extension != self.name {
                log::error!(
                    "extension {} cannot declare an item slot on node {node} placed by {}",
                    self.name,
                    placed.extension
                );
                return false;
            }
            if tabs && placed.component.as_str() != "yard.tabs" {
                log::error!(
                    "node {node} is a {}, not a yard.tabs: it cannot have item tabs",
                    placed.component
                );
                return false;
            }
            if let Some(component) = registry.components.get(&placed.component)
                && !component.slots.contains(&name)
            {
                log::error!(
                    "component {} does not declare slot {slot}",
                    placed.component
                );
                return false;
            }
        }
        let address = (id, slot.to_owned());
        if registry.item_slots.contains_key(&address) {
            log::error!("slot {node}.{slot} is already an item slot");
            return false;
        }
        registry.item_slots.insert(
            address,
            ItemDeclaration {
                model: TypeId::of::<M>(),
                name: model,
                read: Rc::new(|entity, cx| {
                    entity
                        .clone()
                        .downcast::<M>()
                        .expect("declared item model")
                        .read(cx)
                        .items()
                }),
                observe: Rc::new(|entity, window, cx| {
                    let _window_scope = window_scope(window, cx);
                    let entity = entity.clone().downcast::<M>().expect("declared item model");
                    let window = window.window_handle();
                    cx.observe(&entity, move |entity, cx| {
                        let model = entity.entity_id();
                        let _ = window.update(cx, |_, window, cx| {
                            let _window_scope = window_scope(window, cx);
                            item_model_notified(model, window, cx);
                        });
                    })
                }),
            },
        );
        tracing::debug!("declared");
        true
    }

    /// Declares a slot of this extension's component a slot for one node.
    pub fn slot_for_one(&self, cx: &mut App, component: &str, slot: &str) {
        let _group = tracing::info_span!("slot-declare", component, slot).entered();
        let component = match ComponentId::parse(component) {
            Ok(component) => component,
            Err(error) => {
                log::error!("invalid component ID {component:?}: {error}");
                return;
            }
        };
        let slot = match SlotName::parse(slot) {
            Ok(slot) => slot,
            Err(error) => {
                log::error!("invalid slot name {slot:?}: {error}");
                return;
            }
        };
        if component.as_str().split('.').next() != Some(self.name.as_str()) {
            log::error!(
                "extension {} cannot declare slot {component}.{slot} for one",
                self.name
            );
            return;
        }

        let registry = cx.default_global::<Registry>();
        if let Some(registered) = registry.components.get(&component)
            && !registered.slots.contains(&slot)
        {
            log::error!("component {component} does not declare slot {slot}");
            return;
        }
        if registry
            .slots_for_one
            .contains(&(component.clone(), slot.clone()))
        {
            log::error!("slot {component}.{slot} is already declared for one");
            return;
        }
        registry.slots_for_one.push((component, slot));
        tracing::debug!("slot for one");
    }

    /// Declares a component of this extension required: its nodes, and the
    /// nodes above them, cannot be hidden or removed.
    pub fn require(&self, cx: &mut App, component: &str) {
        let _group = tracing::info_span!("component-require", component).entered();
        let component = match ComponentId::parse(component) {
            Ok(component) => component,
            Err(error) => {
                log::error!("invalid component ID {component:?}: {error}");
                return;
            }
        };
        if component.as_str().split('.').next() != Some(self.name.as_str()) {
            log::error!(
                "extension {} cannot require component {component}",
                self.name
            );
            return;
        }
        if cx
            .default_global::<Registry>()
            .required
            .insert(component.clone())
        {
            tracing::debug!("required");
        } else {
            log::error!("component {component} is already required");
        }
    }

    /// Declares the sizes of a component of this extension, in rems: the
    /// smallest a split may make it, and the size it starts at along a split.
    pub fn size_hints(&self, cx: &mut App, component: &str, hints: SizeHints) {
        let _group = tracing::info_span!("size-hints", component).entered();
        let component = match ComponentId::parse(component) {
            Ok(component) => component,
            Err(error) => {
                log::error!("invalid component ID {component:?}: {error}");
                return;
            }
        };
        if component.as_str().split('.').next() != Some(self.name.as_str()) {
            log::error!(
                "extension {} cannot declare the sizes of component {component}",
                self.name
            );
            return;
        }
        let positive = |size: Rems| size.0.is_finite() && size.0 > 0.;
        if !positive(hints.min.width) || !positive(hints.min.height) {
            log::error!("the minimum size of component {component} is not positive");
            return;
        }
        for (dimension, preferred, min) in [
            ("width", hints.preferred.width, hints.min.width),
            ("height", hints.preferred.height, hints.min.height),
        ] {
            let Some(preferred) = preferred else {
                continue;
            };
            if !positive(preferred) {
                log::error!("the preferred {dimension} of component {component} is not positive");
                return;
            }
            if preferred.0 < min.0 {
                log::error!(
                    "the preferred {dimension} of component {component} is below its minimum"
                );
                return;
            }
        }
        match cx
            .default_global::<Registry>()
            .size_hints
            .entry(component.clone())
        {
            Entry::Vacant(entry) => {
                entry.insert(hints);
            }
            Entry::Occupied(_) => {
                log::error!("component {component} already has size hints");
                return;
            }
        }
        tracing::debug!(min = ?hints.min, preferred = ?hints.preferred, "declared");
        cx.refresh_windows();
    }

    /// Registers the view type that saves and restores a component's state.
    pub fn register_state<V: NodeState + 'static>(&self, cx: &mut App, component: &str) {
        let _group = tracing::info_span!("state-register", component = %component).entered();
        let component = match ComponentId::parse(component) {
            Ok(component) => component,
            Err(error) => {
                log::error!("invalid component ID {component:?}: {error}");
                return;
            }
        };
        if component.as_str().split('.').next() != Some(self.name.as_str()) {
            log::error!(
                "extension {} cannot register state for component {component}",
                self.name
            );
            return;
        }
        match cx.default_global::<Registry>().states.entry(component) {
            Entry::Vacant(entry) => {
                entry.insert(StateRegistration {
                    view_type: TypeId::of::<V>(),
                    save: |view, cx| {
                        let entity = view
                            .clone()
                            .downcast::<V>()
                            .expect("checked state view type");
                        entity.read(cx).save_state(cx)
                    },
                    restore: |view, state, window, cx| {
                        let entity = view
                            .clone()
                            .downcast::<V>()
                            .expect("checked state view type");
                        entity.update(cx, |view, cx| view.restore_state(state, window, cx));
                    },
                });
                tracing::debug!("registered");
            }
            Entry::Occupied(entry) => log::error!("component {} already has state", entry.key()),
        }
    }

    /// Registers the view type that supplies a component's titles.
    pub fn register_title<V: Titled + 'static>(&self, cx: &mut App, component: &str) {
        let _group = tracing::info_span!("title-register", component).entered();
        let component = match ComponentId::parse(component) {
            Ok(component) => component,
            Err(error) => {
                log::error!("invalid component ID {component:?}: {error}");
                return;
            }
        };
        if component.as_str().split('.').next() != Some(self.name.as_str()) {
            log::error!(
                "extension {} cannot register titles for component {component}",
                self.name
            );
            return;
        }
        match cx.default_global::<Registry>().titles.entry(component) {
            Entry::Vacant(entry) => {
                entry.insert(Rc::new(|view, cx| {
                    let entity = view.clone().downcast::<V>().ok()?;
                    let view = entity.read(cx);
                    Some((view.title(cx), view.icon(cx)))
                }));
                tracing::debug!("gives titles");
            }
            Entry::Occupied(entry) => log::error!("component {} already gives titles", entry.key()),
        }
    }

    /// Declares that the views of a component of this extension can say they
    /// are empty, so that their containers give them no room while they are.
    pub fn register_empty<V: Emptiable + 'static>(&self, cx: &mut App, component: &str) {
        let _group = tracing::info_span!("empty-register", component).entered();
        let component = match ComponentId::parse(component) {
            Ok(component) => component,
            Err(error) => {
                log::error!("invalid component ID {component:?}: {error}");
                return;
            }
        };
        if component.as_str().split('.').next() != Some(self.name.as_str()) {
            log::error!(
                "extension {} cannot declare the emptiness of component {component}",
                self.name
            );
            return;
        }
        match cx.default_global::<Registry>().empties.entry(component) {
            Entry::Vacant(entry) => {
                entry.insert(Rc::new(|view, cx| {
                    let entity = view.clone().downcast::<V>().ok()?;
                    Some(entity.read(cx).is_empty(cx))
                }));
                tracing::debug!("can be empty");
            }
            Entry::Occupied(entry) => {
                log::error!("component {} already declares its emptiness", entry.key())
            }
        }
    }

    /// Registers a component whose view is created from and told typed props.
    pub fn register_with_props<P, V>(
        &self,
        cx: &mut App,
        component: &str,
        slots: &[&str],
        create: impl Fn(P, &NodeId, Slots, &mut Window, &mut Context<V>) -> V + 'static,
    ) where
        P: serde::Serialize + serde::de::DeserializeOwned + Default + 'static,
        V: PropsView<P> + 'static,
    {
        self.register_factory(
            cx,
            component,
            slots,
            Rc::new(move |node, slots, window, cx| {
                let _window_scope = window_scope(window, cx);
                let parsed = read_props::<P>(&node.id, &node.component, &node.props);
                let node_id = node.id.clone();
                let entity = cx.new(|cx| create(parsed, &node_id, slots, window, cx));
                let weak = entity.downgrade();
                let tell_node = node.id.clone();
                let tell_component = node.component.clone();
                let teller: Teller = Rc::new(move |props, cx| {
                    let Some(entity) = weak.upgrade() else {
                        return;
                    };
                    let parsed = read_props::<P>(&tell_node, &tell_component, props);
                    entity.update(cx, |view, cx| {
                        view.set_props(parsed, cx);
                        cx.notify();
                    });
                    tracing::debug!(node = %tell_node, "view told");
                });
                window_state(cx)
                    .borrow_mut()
                    .tellers
                    .insert(node.id.clone(), teller);
                entity.into()
            }),
        );
    }

    fn register_factory(&self, cx: &mut App, component: &str, slots: &[&str], create: Factory) {
        let _group =
            tracing::info_span!("component-register", component, slots = slots.join(",")).entered();
        let component = match ComponentId::parse(component) {
            Ok(component) => component,
            Err(error) => {
                log::error!("invalid component ID {component:?}: {error}");
                return;
            }
        };
        if component.as_str().split('.').next() != Some(self.name.as_str()) {
            log::error!(
                "extension {} cannot register component {component}",
                self.name
            );
            return;
        }
        let mut names = Vec::new();
        for slot in slots {
            let name = match SlotName::parse(slot) {
                Ok(name) => name,
                Err(error) => {
                    log::error!("invalid slot name {slot:?}: {error}");
                    return;
                }
            };
            if names.contains(&name) {
                log::error!("component {component} declares slot {name} twice");
                return;
            }
            names.push(name);
        }

        let state_handle = window_state(cx);
        let layout_state = state_handle.borrow();
        let registry = cx.default_global::<Registry>();
        if !registry.components.contains_key(&component) {
            let tree_handle = layout_state.tree.clone();
            let layers = tree_handle.borrow();
            registry.item_slots.retain(|(node, slot), declaration| {
                if component_of(layers.current(), node) == Some(&component)
                    && !names.iter().any(|name| name.as_str() == slot)
                {
                    let _group = tracing::info_span!(
                        "item-slot-declare",
                        node = node.as_str(),
                        slot,
                        model = declaration.name
                    )
                    .entered();
                    log::error!("component {component} does not declare slot {slot}");
                    false
                } else {
                    true
                }
            });
            drop(layers);
            registry.slots_for_one.retain(|(declared_component, slot)| {
                if declared_component == &component && !names.contains(slot) {
                    log::error!("component {component} does not declare slot {slot}");
                    false
                } else {
                    true
                }
            });
        }
        match registry.components.entry(component.clone()) {
            Entry::Vacant(entry) => {
                entry.insert(Component {
                    slots: names,
                    create,
                });
                tracing::debug!("registered");
            }
            Entry::Occupied(entry) => {
                log::error!("component {} is already registered", entry.key())
            }
        }
        drop(layout_state);
        retry_registered_component(cx, &component);
        schedule_registration_items(cx);
    }

    /// Places a valid root, whether or not its component is registered yet.
    pub fn place_root(&self, cx: &mut App, node: &str, component: &str) {
        self.place_root_internal(cx, node, component, Ok(Props::new()));
    }

    /// Places a valid root with props, whether or not its component is registered yet.
    pub fn place_root_with_props(
        &self,
        cx: &mut App,
        node: &str,
        component: &str,
        props: impl serde::Serialize,
    ) {
        self.place_root_internal(cx, node, component, encode_props(props));
    }

    fn place_root_internal(
        &self,
        cx: &mut App,
        node: &str,
        component: &str,
        props: Result<Props, String>,
    ) {
        let _group = tracing::info_span!("root-place", node, component).entered();
        if no_window(cx) {
            tracing::info!("no window");
            return;
        }
        if window_state(cx).borrow_mut().default_fixed {
            tracing::info!("the window is open; add the node with add");
            return;
        }
        let props = match props {
            Ok(props) => props,
            Err(message) => {
                log::error!("{message}");
                return;
            }
        };
        let id = match NodeId::parse(node) {
            Ok(id) => id,
            Err(error) => {
                log::error!("invalid node ID {node:?}: {error}");
                return;
            }
        };
        let component = match ComponentId::parse(component) {
            Ok(component) => component,
            Err(error) => {
                log::error!("invalid component ID {component:?}: {error}");
                return;
            }
        };
        if component.as_str().split('.').next() != Some(self.name.as_str())
            && component.as_str().split('.').next() != Some("yard")
        {
            log::error!("extension {} cannot place component {component}", self.name);
            return;
        }
        if is_frame_component(&component) {
            tracing::info!(
                node = %id,
                component = %component,
                extension = %self.name,
                "the window's frame belongs to the shell",
            );
            return;
        }

        let state_handle = window_state(cx);
        let mut layout_state = state_handle.borrow_mut();
        let registry = cx.default_global::<Registry>();
        if let Some(root) = &layout_state.extension_root {
            tracing::info!(
                node = %id,
                component = %component,
                extension = %self.name,
                root = %root,
                "a root is already placed",
            );
            return;
        }
        let body = NodeId::parse("body").unwrap();
        let children = SlotName::parse("children").unwrap();
        if let Some(reason) =
            context_rule_error(registry, &layout_state, &id, &component, &body, false)
        {
            tracing::info!("{reason}");
            return;
        }
        let placed = layout_state
            .tree
            .borrow_mut()
            .current_mut()
            .expect("startup placement is open")
            .place(
                Node {
                    id: id.clone(),
                    component,
                    extension: self.name.clone(),
                    props,
                },
                &body,
                children,
            );
        if placed.is_ok() {
            layout_state.extension_root = Some(id);
        }
    }

    /// Places a node under an already placed parent.
    pub fn place(&self, cx: &mut App, node: &str, component: &str, parent: &str, slot: &str) {
        self.place_internal(
            cx,
            node,
            component,
            parent,
            slot,
            Ok(Props::new()),
            Position::Last,
        );
    }

    /// Places a node at a position under an already placed parent.
    pub fn place_at(
        &self,
        cx: &mut App,
        node: &str,
        component: &str,
        parent: &str,
        slot: &str,
        at: Position,
    ) {
        self.place_internal(cx, node, component, parent, slot, Ok(Props::new()), at);
    }

    /// Places a node with the props and at the position of `placement`.
    pub fn place_with(
        &self,
        cx: &mut App,
        node: &str,
        component: &str,
        parent: &str,
        slot: &str,
        placement: Placement,
    ) {
        self.place_internal(
            cx,
            node,
            component,
            parent,
            slot,
            placement_props(placement.props),
            placement.at,
        );
    }

    /// Places a node with props under an already placed parent.
    pub fn place_with_props(
        &self,
        cx: &mut App,
        node: &str,
        component: &str,
        parent: &str,
        slot: &str,
        props: impl serde::Serialize,
    ) {
        self.place_internal(
            cx,
            node,
            component,
            parent,
            slot,
            encode_props(props),
            Position::Last,
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn place_internal(
        &self,
        cx: &mut App,
        node: &str,
        component: &str,
        parent: &str,
        slot: &str,
        props: Result<Props, String>,
        at: Position,
    ) {
        let _group = tracing::info_span!("node-place", node, component, parent, slot).entered();
        if no_window(cx) {
            tracing::info!("no window");
            return;
        }
        if window_state(cx).borrow_mut().default_fixed {
            tracing::info!("the window is open; add the node with add");
            return;
        }
        let props = match props {
            Ok(props) => props,
            Err(message) => {
                log::error!("{message}");
                return;
            }
        };
        let id = match NodeId::parse(node) {
            Ok(id) => id,
            Err(error) => {
                log::error!("invalid node ID {node:?}: {error}");
                return;
            }
        };
        let component = match ComponentId::parse(component) {
            Ok(component) => component,
            Err(error) => {
                log::error!("invalid component ID {component:?}: {error}");
                return;
            }
        };
        if component.as_str().split('.').next() != Some(self.name.as_str())
            && component.as_str().split('.').next() != Some("yard")
        {
            log::error!("extension {} cannot place component {component}", self.name);
            return;
        }
        let parent = match NodeId::parse(parent) {
            Ok(parent) => parent,
            Err(error) => {
                log::error!("invalid parent ID {parent:?}: {error}");
                return;
            }
        };
        let slot = match SlotName::parse(slot) {
            Ok(slot) => slot,
            Err(error) => {
                log::error!("invalid slot name {slot:?}: {error}");
                return;
            }
        };
        if is_frame_component(&component) || is_frame_slot(&parent, &slot) {
            tracing::info!(
                node = %id,
                component = %component,
                extension = %self.name,
                parent = %parent,
                slot = %slot,
                "the window's frame belongs to the shell",
            );
            return;
        }
        if let Some(reason) = slot_count_rule_error(
            cx.global::<Registry>(),
            &window_state(cx).borrow(),
            &parent,
            &slot,
            None,
        ) {
            tracing::info!("{reason}");
            return;
        }
        if let Some(reason) = context_rule_error(
            cx.global::<Registry>(),
            &window_state(cx).borrow(),
            &id,
            &component,
            &parent,
            false,
        ) {
            tracing::info!("{reason}");
            return;
        }
        let _ = window_state(cx)
            .borrow()
            .tree
            .borrow_mut()
            .current_mut()
            .expect("startup placement is open")
            .place_at(
                Node {
                    id,
                    component,
                    extension: self.name.clone(),
                    props,
                },
                &parent,
                slot,
                at,
            );
    }

    /// Binds default keys for actions in the given key contexts.
    pub fn bind_keys(&self, cx: &mut App, bindings: &[(&str, Box<dyn Action>, &str)]) {
        let _group = tracing::info_span!(
            "keys-bind",
            extension = self.name.as_str(),
            count = bindings.len(),
        )
        .entered();
        let mut valid = Vec::with_capacity(bindings.len());
        let mut logged = Vec::with_capacity(bindings.len());
        for (keys, action, context) in bindings {
            let predicate = match KeyBindingContextPredicate::parse(context) {
                Ok(predicate) => Rc::new(predicate),
                Err(error) => {
                    log::error!("invalid key context {context:?} for keys {keys:?}: {error}");
                    continue;
                }
            };
            if keys.split_whitespace().next().is_none() {
                log::error!("invalid key binding {keys:?}: empty key sequence");
                continue;
            }
            let action_name = action.name();
            let binding = match KeyBinding::load(
                keys,
                action.boxed_clone(),
                Some(predicate),
                false,
                None,
                cx,
            ) {
                Ok(binding) => binding,
                Err(error) => {
                    log::error!("invalid key binding {keys:?}: {error}");
                    continue;
                }
            };
            if binding
                .keystrokes()
                .iter()
                .any(|keystroke| keystroke.key().is_empty())
            {
                log::error!("invalid key binding {keys:?}: missing key after `-`");
                continue;
            }
            logged.push((*keys, action_name, *context));
            valid.push(binding);
        }
        if !valid.is_empty() {
            cx.bind_keys(valid);
        }
        for (keys, action, context) in logged {
            tracing::debug!(keys, action, context, "bound");
        }
    }

    /// Registers a window-wide handler for an action in the extension's namespace.
    pub fn on_action<A: Action>(
        &self,
        cx: &mut App,
        handler: impl Fn(&A, &mut Window, &mut App) + 'static,
    ) {
        let action = A::name_for_type();
        if !action
            .strip_prefix(self.name.as_str())
            .is_some_and(|rest| rest.starts_with("::"))
        {
            log::error!("extension {} cannot handle {action}", self.name);
            return;
        }
        let handler = Rc::new(handler);
        let extension = self.name.clone();
        cx.default_global::<Registry>()
            .action_handlers
            .push(Rc::new(move |div| {
                let handler = handler.clone();
                let extension = extension.clone();
                div.on_action::<A>(move |value, window, cx| {
                    let _window_scope = window_scope(window, cx);
                    let _group = tracing::info_span!(
                        "extension-action",
                        extension = extension.as_str(),
                        action,
                    )
                    .entered();
                    handler(value, window, cx);
                })
            }));
        tracing::debug!(extension = self.name.as_str(), action, "action handled");
    }

    /// Shows a toast in this window.
    pub fn show_toast(&self, toast: yard_ui::Toast, window: &mut Window, cx: &mut App) {
        let _window_scope = window_scope(window, cx);
        if let Some(layer) = toast_layer(window, cx) {
            layer.update(cx, |layer, cx| layer.show_toast(toast, cx));
        }
    }

    /// Closes the toast in this window, if one is shown.
    pub fn hide_toast(&self, window: &mut Window, cx: &mut App) {
        let _window_scope = window_scope(window, cx);
        if let Some(layer) = toast_layer(window, cx) {
            layer.update(cx, |layer, cx| {
                layer.hide_toast(yard_ui::ToastHideReason::HideToast, cx)
            });
        }
    }

    /// Toggles a modal of type `V` in the window's modal layer.
    pub fn toggle_modal<V: yard_ui::ModalView>(
        &self,
        window: &mut Window,
        cx: &mut App,
        build: impl FnOnce(&mut Window, &mut Context<V>) -> V,
    ) {
        let _window_scope = window_scope(window, cx);
        let Some(layer) = modal_layer(window, cx) else {
            return;
        };
        layer.update(cx, |layer, cx| {
            layer.toggle_modal(window, cx, build);
        });
    }

    /// Closes the active modal in the window's modal layer, if one is open.
    pub fn hide_modal(&self, window: &mut Window, cx: &mut App) {
        let _window_scope = window_scope(window, cx);
        let Some(layer) = modal_layer(window, cx) else {
            return;
        };
        layer.update(cx, |layer, cx| {
            layer.hide_modal(yard_ui::ModalHideReason::HideModal, window, cx);
        });
    }

    /// Registers the section `T`, whose key must be this extension's name.
    pub fn register_settings<T: yard_settings::Settings>(&self, cx: &mut App) {
        if T::KEY != self.name {
            log::error!(
                "extension {} cannot declare the section {}",
                self.name,
                T::KEY
            );
            return;
        }
        yard_settings::register::<T>(cx);
    }
}

fn toast_layer(window: &Window, cx: &App) -> Option<Entity<yard_ui::ToastLayer>> {
    let _window_scope = window_context(window, cx);
    Some(
        window
            .root::<Shell>()
            .flatten()?
            .read(cx)
            .toast_layer
            .clone(),
    )
}

/// Returns whether this window shows a toast.
pub fn has_toast(window: &Window, cx: &App) -> bool {
    let _window_scope = window_context(window, cx);
    toast_layer(window, cx).is_some_and(|layer| layer.read(cx).has_active_toast())
}

fn modal_layer(window: &Window, cx: &App) -> Option<Entity<yard_ui::ModalLayer>> {
    let _window_scope = window_context(window, cx);
    let shell = window.root::<Shell>().flatten()?;
    Some(shell.read(cx).modal_layer.clone())
}

/// Returns whether the window's modal layer currently shows a modal.
pub fn has_modal(window: &Window, cx: &App) -> bool {
    let _window_scope = window_context(window, cx);
    modal_layer(window, cx).is_some_and(|layer| layer.read(cx).has_active_modal())
}

/// The window's root view, owning its layout state and node views.
pub struct Shell {
    state: Rc<RefCell<WindowState>>,
    handle: FocusHandle,
    root: Option<AnyView>,
    modal_layer: Entity<yard_ui::ModalLayer>,
    toast_layer: Entity<yard_ui::ToastLayer>,
    last_focused: Option<NodeId>,
    _focus_lost: Subscription,
    _activation: Subscription,
    _item_subscriptions: Rc<RefCell<ItemSubscriptions>>,
    _session_bounds: Subscription,
    _release: Subscription,
}

impl Shell {
    fn sync_focus(&mut self, window: &Window, cx: &App) {
        let _window_scope = window_context(window, cx);
        let current = focused_node(window, cx);
        if self.last_focused == current {
            return;
        }
        self.last_focused = current.clone();
        let node = current.as_ref().map_or("none", NodeId::as_str);
        let _group = tracing::info_span!("focus-change", node = %node).entered();
        tracing::debug!("focused");
    }
}

/// Creates the initial nodes once, parent before children, when the window opens.
pub fn new_shell(window: &mut Window, cx: &mut App) -> Entity<Shell> {
    create_shell(window, cx, None)
}

/// The shell of a window recreated from the session: as `new_shell`, but
/// showing the window's named layout with its user layers.
pub fn new_session_shell(
    window: &mut Window,
    cx: &mut App,
    saved: &SessionWindow,
) -> Entity<Shell> {
    create_shell(window, cx, Some(saved))
}

fn create_shell(window: &mut Window, cx: &mut App, saved: Option<&SessionWindow>) -> Entity<Shell> {
    let id = window.window_handle().window_id();
    let owned_state = {
        let registry = cx.default_global::<Registry>();
        let mut default = registry.default.borrow_mut();
        if !registry.ever_opened {
            default.default_fixed = true;
        }
        let mut state = WindowState::new(default.tree.borrow().current().clone());
        state.extension_root = default.extension_root.clone();
        if !registry.ever_opened {
            state.zoomed = default.zoomed.clone();
        }
        state.startup_tree = state.tree.borrow().current().clone();
        state.tree.borrow_mut().set_default();
        state.default_fixed = true;
        state.main = !registry.ever_opened;
        Rc::new(RefCell::new(state))
    };
    {
        let registry = cx.default_global::<Registry>();
        registry.windows.insert(id, Rc::downgrade(&owned_state));
        registry.ever_opened = true;
        remember_active(registry, id);
    }
    register_shell_states(cx);
    super::session::window_opened(window, cx);
    let pending_states = state_window_opened(
        cx,
        window.window_handle(),
        saved.is_some(),
        owned_state.borrow().main,
    );
    let _window = window_scope(window, cx);
    let item_subscriptions = Rc::new(RefCell::new(BTreeMap::new()));
    owned_state.borrow_mut().item_window =
        Some((window.window_handle(), Rc::downgrade(&item_subscriptions)));
    let restored = match saved {
        Some(saved) => restore_session(cx, saved),
        None => restore(cx),
    };
    owned_state.borrow_mut().pending_states = pending_states;
    let (frame_root, ext_root) = {
        let state = owned_state.borrow();
        (
            state.tree.borrow().current().root().cloned(),
            state.extension_root.clone(),
        )
    };
    let _group = tracing::info_span!("shell-create",
        root = %ext_root.as_ref().map_or("none", NodeId::as_str), window = id.as_u64())
    .entered();
    if ext_root.is_none() {
        tracing::info!("no root placed; the window shows the background");
    }
    let root = frame_root.and_then(|node| create_view(&node, None, window, cx));
    {
        let mut state = owned_state.borrow_mut();
        state.views_created = true;
        if let Some(given) = state.given_zoom.take() {
            give_zoom(&mut state, given);
        }
        let invalid = state.zoomed.as_ref().is_some_and(|node| {
            let tree = state.tree.borrow();
            !state.views.borrow().contains_key(node)
                || !is_shown(tree.current(), node)
                || !is_in_body(tree.current(), node)
        });
        if invalid {
            let node = state.zoomed.take().unwrap();
            tracing::debug!(node = %node, "zoom cleared");
        }
    }
    let before_items = owned_state.borrow().item_children.borrow().clone();
    sync_items(window, cx, false);
    if restored || before_items != *owned_state.borrow().item_children.borrow() {
        notify_layout(cx);
    }
    reconcile_containers(cx);
    // The saved tabs are given once the window's views and item copies
    // exist, also to a container a zoom keeps from being drawn; a tab that
    // has none yet waits for its container's first layout.
    let tabs = owned_state.borrow().tabs.clone();
    for tabs in tabs {
        let _ = tabs.update(cx, |tabs, cx| {
            if tabs.pending.is_some() {
                let nodes = tabs.shown(cx, Emptiness::Ask);
                tabs.give_pending(&nodes, false);
            }
        });
    }
    state_changed(cx);
    let toast_layer = cx.new(|_| yard_ui::ToastLayer::new());
    let modal_layer = cx.new(|_| yard_ui::ModalLayer::new());
    cx.new(|cx| {
        let handle = cx.focus_handle();
        owned_state.borrow_mut().root_focus_handle = Some(handle.clone());
        if window.focused(cx).is_none() {
            handle.focus(window, cx);
        }
        Shell {
            state: owned_state,
            handle,
            root,
            modal_layer,
            toast_layer,
            last_focused: None,
            _item_subscriptions: item_subscriptions,
            _session_bounds: cx.observe_window_bounds(window, |_, window, cx| {
                super::session::bounds_changed(window, cx);
            }),
            _release: cx.on_release(move |shell: &mut Shell, cx| {
                let _group = tracing::info_span!("window-close", window = id.as_u64()).entered();
                close_session_window(cx, id);
                cx.default_global::<Registry>().windows.remove(&id);
                cx.global::<Registry>()
                    .active_history
                    .borrow_mut()
                    .retain(|known| *known != id);
                shell._item_subscriptions.borrow_mut().clear();
                shell.root.take();
                let views = std::mem::take(&mut *shell.state.borrow().views.borrow_mut());
                for (node, view) in views {
                    drop(view);
                    tracing::debug!(node = %node, "view dropped");
                }
            }),
            _activation: cx.observe_window_activation(window, |_, window, cx| {
                if window.is_window_active() {
                    remember_active(cx.global::<Registry>(), window.window_handle().window_id());
                }
            }),
            _focus_lost: cx.on_focus_lost(window, |shell: &mut Shell, window, cx| {
                let _window_scope = window_scope(window, cx);
                shell.handle.focus(window, cx);
                shell.sync_focus(window, cx);
                window.on_next_frame(|window, _| window.refresh());
            }),
        }
    })
}

fn restore(cx: &mut App) -> bool {
    window_state(cx).borrow_mut().item_new_pending.clear();
    let Some(path) = layout_path(cx.global::<Registry>(), &window_state(cx).borrow()) else {
        return false;
    };
    let _group = tracing::info_span!("layout-restore", file = %path.display()).entered();
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            tracing::debug!("no saved layout");
            return false;
        }
        Err(error) => {
            log::warn!("the saved layout cannot be read: {error}");
            return false;
        }
    };
    let operations = match decode(&text) {
        Ok(operations) => operations,
        Err(error) => {
            log::warn!("the saved layout cannot be read: {error}");
            return false;
        }
    };
    apply_restored(cx, operations)
}

/// Applies saved operations to the window's layout, each checked by the
/// rules of a live one; a refused one is skipped with an `INFO`.
fn apply_restored(cx: &mut App, operations: Vec<Operation>) -> bool {
    window_state(cx).borrow_mut().failed_views.clear();
    let mut applied = 0usize;
    let mut skipped = 0usize;
    for operation in operations {
        let (op, node) = match &operation {
            Operation::Add { node, .. } => ("add", node.id.clone()),
            Operation::Remove { node } => ("remove", node.clone()),
            Operation::Move { node, .. } => ("move", node.clone()),
            Operation::SetHidden { node, hidden: true } => ("hide", node.clone()),
            Operation::SetHidden {
                node,
                hidden: false,
            } => ("show", node.clone()),
            Operation::SetProps { node, .. } => ("props", node.clone()),
        };
        let extension = match &operation {
            Operation::Add { node, .. } => node.extension.as_str(),
            _ => "yard",
        };
        if let Err(reason) = shell_rule_error(extension, cx, &operation) {
            tracing::info!(
                op,
                node = node.as_str(),
                reason = reason.as_str(),
                "skipped"
            );
            skipped += 1;
            continue;
        }
        let result = {
            let tree = window_state(cx).borrow().tree.clone();
            let mut tree = tree.borrow_mut();
            tree.apply(operation)
        };
        match result {
            Ok(_) => applied += 1,
            Err(error) => {
                let reason = error.to_string();
                tracing::info!(
                    op,
                    node = node.as_str(),
                    reason = reason.as_str(),
                    "skipped"
                );
                skipped += 1;
            }
        }
    }
    tracing::debug!(applied, skipped, "restored");
    applied > 0
}

/// Shows the session's named layout with its user layers in the window
/// being created, in place of the main window's files.
fn restore_session(cx: &mut App, saved: &SessionWindow) -> bool {
    window_state(cx).borrow_mut().item_new_pending.clear();
    let name = saved.layout.as_str();
    let named = if name == "yard.default" {
        None
    } else {
        let named = cx.global::<Registry>().named_layouts.get(name).cloned();
        if named.is_none() {
            tracing::info!(name, "no named layout");
        }
        named
    };
    let shown = if named.is_some() {
        name
    } else {
        "yard.default"
    };
    {
        let state_handle = window_state(cx);
        let mut layout_state = state_handle.borrow_mut();
        layout_state.saved_layers = saved
            .layers
            .iter()
            .filter(|(layout, _)| layout.as_str() != shown)
            .map(|(layout, operations)| (layout.clone(), operations.clone()))
            .collect();
        layout_state.shown_layout = shown.to_string().into();
        let mut given = saved.layers.keys().cloned().collect::<BTreeSet<_>>();
        given.insert(shown.to_string());
        layout_state.session_layers = Some((saved.file.clone(), given));
    }
    if let Some(named) = named {
        {
            let state_handle = window_state(cx);
            let layout_state = state_handle.borrow();
            *layout_state.tree.borrow_mut() = Layers::new(layout_state.startup_tree.clone());
        }
        for operation in named.operations {
            match operation {
                Ok(operation) => {
                    let added = match &operation {
                        Operation::Add { node, .. } => Some(node.id.clone()),
                        _ => None,
                    };
                    if replay_operation(cx, &named.extension, operation)
                        && let Some(id) = added
                    {
                        window_state(cx).borrow_mut().named_nodes.insert(id);
                    }
                }
                Err(reason) => tracing::info!(reason, "skipped"),
            }
        }
        window_state(cx).borrow().tree.borrow_mut().set_default();
    }
    let _group = tracing::info_span!("layout-restore", file = %saved.file.display()).entered();
    let operations = saved.layers.get(shown).cloned().unwrap_or_default();
    apply_restored(cx, operations)
}

#[derive(Default)]
struct ExtensionStores {
    dir: Option<PathBuf>,
    extensions: BTreeMap<String, ExtensionStore>,
    no_folder_logged: bool,
    quitting: bool,
}

impl Global for ExtensionStores {}

#[derive(Default)]
struct ExtensionStore {
    values: StoreValues,
    burst: Option<StateSaveBurst>,
    continuation: Option<StateSaveBurst>,
    save_task: Option<gpui::Task<()>>,
    revision: u64,
    written: Arc<Mutex<u64>>,
}

/// Sets the folder used to load and persist extension values.
pub fn set_store_dir(cx: &mut App, dir: PathBuf) {
    let stores = cx.default_global::<ExtensionStores>();
    if stores.dir.is_some() || !stores.extensions.is_empty() {
        log::error!("the store folder must be set once before any extension store is read");
        return;
    }
    stores.dir = Some(dir);
    cx.on_app_quit(|cx| {
        let stores = cx.global_mut::<ExtensionStores>();
        stores.quitting = true;
        let names = stores
            .extensions
            .iter_mut()
            .filter_map(|(name, store)| {
                store.save_task.take();
                let pending = match store.written.try_lock() {
                    Ok(written) => store.revision > *written,
                    Err(std::sync::TryLockError::Poisoned(error)) => {
                        store.revision > *error.into_inner()
                    }
                    // The background save may still succeed. Check again after it finishes.
                    Err(std::sync::TryLockError::WouldBlock) => true,
                };
                pending.then(|| name.clone())
            })
            .collect::<Vec<_>>();
        let writes = names
            .iter()
            .map(|name| write_store_file(cx, name, StateSaveReason::Quit))
            .collect::<Vec<_>>();
        async move {
            for write in writes {
                write.await;
            }
        }
    })
    .detach();
}

fn valid_store_key(key: &str) -> bool {
    if key.is_empty() {
        tracing::info!(reason = "empty key", "refused");
        return false;
    }
    true
}

fn extension_store<'a>(cx: &'a mut App, name: &str) -> &'a mut ExtensionStore {
    let stores = cx.default_global::<ExtensionStores>();
    stores.extensions.entry(name.into()).or_insert_with(|| {
        let values = if let Some(dir) = &stores.dir {
            read_store_file(name, &dir.join(format!("{name}.json")))
        } else {
            if !std::mem::replace(&mut stores.no_folder_logged, true) {
                tracing::debug!("no store folder");
            }
            StoreValues::new()
        };
        ExtensionStore {
            values,
            ..Default::default()
        }
    })
}

fn read_store_file(name: &str, path: &Path) -> StoreValues {
    let _group =
        tracing::info_span!("store-read", extension = name, file = %path.display()).entered();
    let result = match std::fs::read_to_string(path) {
        Ok(text) => decode_store(&text),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            tracing::debug!("no saved values");
            return StoreValues::new();
        }
        Err(error) => Err(error.to_string()),
    };
    match result {
        Ok(values) => {
            tracing::debug!(keys = values.len(), "read");
            values
        }
        Err(error) => {
            log::warn!(
                "the saved values at {} cannot be read: {error}",
                path.display()
            );
            StoreValues::new()
        }
    }
}

fn store_changed(cx: &mut App, name: &str) {
    let now = cx.background_executor().now();
    let stores = cx.global_mut::<ExtensionStores>();
    if stores.quitting || stores.dir.is_none() {
        return;
    }
    let store = stores.extensions.get_mut(name).expect("store is loaded");
    store.revision += 1;
    store.burst = Some(StateSaveBurst::changed(
        now,
        store.burst,
        &mut store.continuation,
    ));
    if store.save_task.is_some() {
        return;
    }

    let extension = name.to_owned();
    let task = cx.spawn(async move |cx| {
        let executor = cx.background_executor().clone();
        while let Some(burst) = wait_for_save(&executor, || {
            cx.update(|cx| cx.global::<ExtensionStores>().extensions[&extension].burst)
        })
        .await
        {
            let write = cx.update(|cx| {
                let store = extension_store(cx, &extension);
                store.burst = None;
                store.continuation = (burst.deadline() < burst.quiet_until).then_some(burst);
                write_store_file(cx, &extension, StateSaveReason::Change)
            });
            write.await;
        }
        cx.update(|cx| {
            if let Some(task) = extension_store(cx, &extension).save_task.take() {
                task.detach();
            }
        });
    });
    extension_store(cx, name).save_task = Some(task);
}

fn write_store_file(cx: &mut App, name: &str, reason: StateSaveReason) -> gpui::Task<()> {
    let stores = cx.global_mut::<ExtensionStores>();
    let path = stores
        .dir
        .as_ref()
        .expect("store folder is set")
        .join(format!("{name}.json"));
    let store = stores.extensions.get_mut(name).expect("store is loaded");
    let revision = store.revision;
    let written = store.written.clone();
    let values = store.values.clone();
    let extension = name.to_owned();
    let keys = values.len();
    let save_group =
        move || tracing::info_span!("store-save", extension, keys, reason = reason.as_str());
    let group = (reason == StateSaveReason::Change).then(&save_group);
    cx.background_executor().spawn(async move {
        let mut written = written.lock().unwrap_or_else(PoisonError::into_inner);
        if *written >= revision {
            return;
        }
        let _group = group.unwrap_or_else(save_group).entered();
        let text = encode_store(&values);
        if write_atomic(&path, text.as_bytes()).is_ok() {
            *written = revision;
            tracing::debug!("saved");
        }
    })
}

const STATE_SAVE_DELAY: Duration = Duration::from_millis(300);
const STATE_SAVE_MAX_WAIT: Duration = Duration::from_secs(2);

#[derive(Clone, Copy)]
struct StateSaveBurst {
    first: Instant,
    quiet_until: Instant,
}

impl StateSaveBurst {
    fn changed(now: Instant, burst: Option<Self>, continuation: &mut Option<Self>) -> Self {
        let first = burst.map_or_else(
            || {
                // A capped burst can continue across its save. Anchor the next
                // cap to the preceding one while changes are still arriving.
                continuation
                    .take()
                    .filter(|burst| now < burst.quiet_until)
                    .map_or(now, Self::deadline)
            },
            |burst| burst.first,
        );
        Self {
            first,
            quiet_until: now + STATE_SAVE_DELAY,
        }
    }

    fn deadline(self) -> Instant {
        self.quiet_until.min(self.first + STATE_SAVE_MAX_WAIT)
    }
}

async fn wait_for_save(
    executor: &gpui::BackgroundExecutor,
    mut pending: impl FnMut() -> Option<StateSaveBurst>,
) -> Option<StateSaveBurst> {
    while let Some(burst) = pending() {
        let delay = burst.deadline().saturating_duration_since(executor.now());
        if delay.is_zero() {
            return Some(burst);
        }
        executor.timer(delay).await;
    }
    None
}

#[derive(Default)]
struct StateFile {
    path: PathBuf,
    pending: VecDeque<WindowStates>,
    windows: Vec<gpui::AnyWindowHandle>,
    closing: Option<gpui::WindowId>,
    last_closed: RefCell<Option<WindowStates>>,
    quitting: bool,
    burst: Option<StateSaveBurst>,
    continuation: Option<StateSaveBurst>,
    save_task: Option<gpui::Task<()>>,
    writing: bool,
    sequence: u64,
    written: Arc<Mutex<u64>>,
}

impl Global for StateFile {}

/// Reads the node states for startup and saves to the same file at quit.
pub fn set_state_file(cx: &mut App, file: PathBuf) {
    let first = cx.try_global::<StateFile>().is_none();
    let pending = read_state_file(&file);
    let state_file = cx.default_global::<StateFile>();
    state_file.path = file;
    state_file.pending = pending.into();
    if first {
        cx.on_app_quit(|cx| {
            let write = flush_state_file(cx);
            async move {
                if let Some(write) = write {
                    write.await;
                }
            }
        })
        .detach();
    }
}

/// The calling view's state changed: save the states soon.
pub fn state_changed(cx: &mut App) {
    tracing::trace!("state changed");
    let now = cx.background_executor().now();
    let Some(file) = cx.try_global::<StateFile>() else {
        return;
    };
    if file.quitting || file.last_closed.borrow().is_some() {
        return;
    }

    let file = cx.global_mut::<StateFile>();
    if file.writing && file.burst.is_none() {
        tracing::debug!("state save queued");
    }
    file.burst = Some(StateSaveBurst::changed(
        now,
        file.burst,
        &mut file.continuation,
    ));
    if file.save_task.is_none() {
        let task = cx.spawn(async |cx| {
            let executor = cx.background_executor().clone();
            while let Some(burst) =
                wait_for_save(&executor, || cx.update(|cx| cx.global::<StateFile>().burst)).await
            {
                let write = cx.update(|cx| {
                    let file = cx.global_mut::<StateFile>();
                    file.burst = None;
                    file.continuation = (burst.deadline() < burst.quiet_until).then_some(burst);
                    file.writing = true;
                    write_state_file(cx, StateSaveReason::Change)
                });
                write.await;
                cx.update(|cx| cx.global_mut::<StateFile>().writing = false);
            }
            cx.update(|cx| {
                if let Some(task) = cx.global_mut::<StateFile>().save_task.take() {
                    task.detach();
                }
            });
        });
        cx.global_mut::<StateFile>().save_task = Some(task);
    }
}

fn read_state_file(file: &Path) -> Vec<WindowStates> {
    let _group = tracing::info_span!("state-restore", file = %file.display()).entered();
    let text = match std::fs::read_to_string(file) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            tracing::debug!("no saved state");
            return Vec::new();
        }
        Err(error) => {
            log::warn!("the saved state cannot be read: {error}");
            return Vec::new();
        }
    };
    match state_format::decode(&text) {
        Ok(windows) => {
            tracing::debug!(
                windows = windows.len(),
                nodes = windows.iter().map(BTreeMap::len).sum::<usize>(),
                "read"
            );
            windows
        }
        Err(error) => {
            log::warn!("the saved state cannot be read: {error}");
            Vec::new()
        }
    }
}

fn state_window_opened(
    cx: &mut App,
    handle: gpui::AnyWindowHandle,
    from_session: bool,
    main: bool,
) -> WindowStates {
    if cx.try_global::<StateFile>().is_none() {
        return WindowStates::new();
    }
    let state_file = cx.global_mut::<StateFile>();
    let pending = if from_session {
        state_file.pending.pop_front().unwrap_or_default()
    } else if main {
        let first = state_file.pending.pop_front().unwrap_or_default();
        state_file.pending.clear();
        first
    } else {
        WindowStates::new()
    };
    state_file.windows.push(handle);
    *state_file.last_closed.get_mut() = None;
    pending
}

fn close_session_window(cx: &mut App, id: gpui::WindowId) {
    if cx.try_global::<StateFile>().is_some() {
        let state_file = cx.global_mut::<StateFile>();
        if !state_file.quitting {
            state_file.windows.retain(|window| window.window_id() != id);
            state_file.closing = Some(id);
        }
        super::session::window_closed(cx, id);
        let file = cx.global_mut::<StateFile>();
        file.closing = None;
        if file.last_closed.get_mut().is_some() {
            file.burst = None;
            file.continuation = None;
            if !file.writing {
                file.save_task.take();
            }
        } else {
            state_changed(cx);
        }
    } else {
        super::session::window_closed(cx, id);
    }
}

fn capture_kept_window(cx: &App, id: gpui::WindowId, state: &Rc<RefCell<WindowState>>) {
    let Some(state_file) = cx
        .try_global::<StateFile>()
        .filter(|file| file.closing == Some(id))
    else {
        return;
    };
    // The view is going away, but another window can open before quit.
    // Keep its state without opening a save group until there is a write.
    let saved = StateCapture::prepare(cx, id, state.clone()).read(cx);
    *state_file.last_closed.borrow_mut() = Some(saved);
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum StateSaveReason {
    Change,
    Quit,
}

impl StateSaveReason {
    fn as_str(self) -> &'static str {
        match self {
            Self::Change => "change",
            Self::Quit => "quit",
        }
    }
}

fn flush_state_file(cx: &mut App) -> Option<gpui::Task<()>> {
    let file = cx.global_mut::<StateFile>();
    if std::mem::replace(&mut file.quitting, true) {
        return None;
    }
    // Dropping the worker also cancels its timer if a save is waiting.
    file.save_task.take();
    file.burst = None;
    Some(write_state_file(cx, StateSaveReason::Quit))
}

fn write_state_file(cx: &mut App, reason: StateSaveReason) -> gpui::Task<()> {
    let state_file = cx.global_mut::<StateFile>();
    let path = state_file.path.clone();
    let handles = if reason == StateSaveReason::Quit {
        std::mem::take(&mut state_file.windows)
    } else {
        state_file.windows.clone()
    };
    state_file.sequence += 1;
    let sequence = state_file.sequence;
    let written = state_file.written.clone();
    let (group, states) = if let Some(saved) = state_file.last_closed.get_mut().take() {
        let group = tracing::info_span!(
            "state-save",
            windows = 1usize,
            nodes = saved.len(),
            reason = reason.as_str(),
        );
        (group, vec![saved])
    } else {
        let windows = handles
            .into_iter()
            .filter_map(|handle| {
                let id = handle.window_id();
                handle
                    .update(cx, |_, _, cx| {
                        let state = session_window_state(cx, id)?;
                        Some(StateCapture::prepare(cx, id, state))
                    })
                    .ok()
                    .flatten()
            })
            .collect::<Vec<_>>();
        let group = tracing::info_span!(
            "state-save",
            windows = windows.len(),
            nodes = windows.iter().map(StateCapture::len).sum::<usize>(),
            reason = reason.as_str(),
        );
        let states = group.in_scope(|| {
            windows
                .into_iter()
                .map(|capture| capture.read(cx))
                .collect()
        });
        (group, states)
    };
    cx.background_executor().spawn(
        async move {
            let mut written = written.lock().unwrap_or_else(PoisonError::into_inner);
            if *written > sequence {
                tracing::debug!("skipped: a newer state save is written");
                return;
            }
            let text = state_format::encode(&states);
            if write_atomic(&path, text.as_bytes()).is_ok() {
                *written = sequence;
                tracing::debug!("saved");
            }
        }
        .instrument(group),
    )
}

struct StateCapture {
    window: gpui::WindowId,
    owner: Rc<RefCell<WindowState>>,
    waiting: WindowStates,
    views: Vec<(NodeId, ComponentId, AnyView, StateRegistration)>,
    wrong_types: Vec<NodeId>,
}

impl StateCapture {
    // Determine the fields of the save span before calling any component's save_state.
    fn prepare(cx: &App, window: gpui::WindowId, owner: Rc<RefCell<WindowState>>) -> Self {
        let registry = cx.global::<Registry>();
        let mut capture = Self {
            window,
            owner: owner.clone(),
            waiting: WindowStates::new(),
            views: Vec::new(),
            wrong_types: Vec::new(),
        };
        let state = owner.borrow();
        capture.waiting = state.pending_states.clone();
        let tree = state.tree.borrow();
        for (node, view) in state.views.borrow().iter() {
            if node.as_str().contains('@') {
                continue;
            }
            let Some(component) = component_of(tree.current(), node) else {
                continue;
            };
            let Some(registration) = registry.states.get(component) else {
                continue;
            };
            let wrapped = view
                .clone()
                .downcast::<NodeView>()
                .expect("the registry stores node views");
            let view = &wrapped.read(cx).view;
            if view.entity_type() != registration.view_type {
                capture.waiting.remove(node.as_str());
                capture.wrong_types.push(node.clone());
            } else if !capture.waiting.contains_key(node.as_str()) {
                capture
                    .views
                    .push((node.clone(), component.clone(), view.clone(), *registration));
            }
        }
        capture
    }

    fn len(&self) -> usize {
        self.waiting.len() + self.views.len()
    }

    fn read(self, cx: &App) -> WindowStates {
        let _group = tracing::info_span!("state-capture", window = self.window.as_u64()).entered();
        let _window = quiet_owner_scope(self.window, cx);
        for node in &self.wrong_types {
            wrong_state_view(node, &self.owner.borrow());
        }
        let mut saved = self.waiting;
        for (node, component, view, registration) in self.views {
            saved.insert(
                node.to_string(),
                SavedNode {
                    component: component.to_string(),
                    state: (registration.save)(&view, cx),
                },
            );
        }
        saved
    }
}

fn wrong_state_view(node: &NodeId, state: &WindowState) {
    if state.state_errors.borrow_mut().insert(node.clone()) {
        log::error!("view of node {node} is not the type that gives its state");
    }
    tracing::trace!(%node, "view is not the type that gives its state");
}

fn restore_view_state(node: &Node, view: &AnyView, window: &mut Window, cx: &mut App) {
    let state = window_state(cx);
    let registration = cx.global::<Registry>().states.get(&node.component).copied();
    let (registration, saved) = {
        let mut state = state.borrow_mut();
        if registration.is_some_and(|registration| registration.view_type != view.entity_type()) {
            wrong_state_view(&node.id, &state);
            state.pending_states.remove(node.id.as_str());
            return;
        }
        let Some(saved) = state.pending_states.get(node.id.as_str()) else {
            return;
        };
        if saved.component != node.component.as_str() {
            tracing::debug!(node = %node.id, saved = %saved.component, component = %node.component, "state skipped");
            state.pending_states.remove(node.id.as_str());
            return;
        }
        let Some(registration) = registration else {
            tracing::debug!(node = %node.id, component = %node.component, "state waiting: component has no state registration");
            return;
        };
        let saved = state.pending_states.remove(node.id.as_str()).unwrap();
        (registration, saved)
    };
    (registration.restore)(view, saved.state, window, cx);
    tracing::debug!(node = %node.id, "state given");
}

/// The shell keeps the state of its own containers as an extension keeps a
/// node's: the tab a tabs container shows, and the node that fills the window.
fn register_shell_states(cx: &mut App) {
    let shell_states = [
        (
            "yard.tabs",
            StateRegistration {
                view_type: TypeId::of::<Tabs>(),
                save: |view, cx| {
                    let tabs = view
                        .clone()
                        .downcast::<Tabs>()
                        .expect("checked state view type");
                    let tabs = tabs.read(cx);
                    let selected = tabs
                        .pending
                        .as_deref()
                        .or(tabs.selected.as_ref().map(NodeId::as_str));
                    serde_json::json!({ "selected": selected })
                },
                restore: |view, state, _, cx| {
                    let tabs = view
                        .clone()
                        .downcast::<Tabs>()
                        .expect("checked state view type");
                    tabs.update(cx, |tabs, _| {
                        if let Some(saved) = read_view_state(&tabs.node, &state, "selected") {
                            tabs.pending = saved;
                        }
                    });
                },
            },
        ),
        (
            "yard.window",
            StateRegistration {
                view_type: TypeId::of::<WindowFrame>(),
                save: |_, cx| {
                    let zoomed =
                        try_window_state(cx).and_then(|state| state.borrow().zoomed.clone());
                    serde_json::json!({ "zoomed": zoomed.as_ref().map(NodeId::as_str) })
                },
                restore: |_, state, _, cx| {
                    let node = NodeId::parse("window").unwrap();
                    if let Some(given) = read_view_state(&node, &state, "zoomed") {
                        window_state(cx).borrow_mut().given_zoom = Some(given);
                    }
                },
            },
        ),
    ];
    for (component, registration) in shell_states {
        let component = ComponentId::parse(component).unwrap();
        let registry = cx.default_global::<Registry>();
        if registry.states.contains_key(&component) {
            continue;
        }
        let _group = tracing::info_span!("state-register", component = %component).entered();
        registry.states.insert(component, registration);
        tracing::debug!("registered");
    }
}

/// Reads `{"<key>": "<node ID>" | null}`; `None` when it cannot be read,
/// after a warning, so that the default is kept.
fn read_view_state(node: &NodeId, state: &serde_json::Value, key: &str) -> Option<Option<String>> {
    let read = match state.get(key) {
        Some(serde_json::Value::Null) => Some(None),
        Some(serde_json::Value::String(value)) => Some(Some(value.clone())),
        _ => None,
    };
    if read.is_none() {
        log::warn!("the saved state of node {node} cannot be read: {state}");
    }
    read
}

/// Why a node cannot be zoomed now, if it cannot: a node in the body, not
/// its root, shown, and with a view once the window's views exist.
fn zoom_refusal(state: &WindowState, id: &NodeId) -> Option<&'static str> {
    let layers = state.tree.borrow();
    let tree = layers.current();
    if component_of(tree, id).is_none() {
        Some("the node is not in the tree")
    } else if is_frame_node(id) {
        Some("the window's frame belongs to the shell")
    } else if !is_in_body(tree, id) {
        Some("only a node in the body can be zoomed")
    } else if state.extension_root.as_ref() == Some(id) {
        Some("the root already fills the body")
    } else if !is_shown(tree, id) {
        Some("the node is hidden or under a hidden node")
    } else if state.views_created && !state.views.borrow().contains_key(id) {
        Some("the node has no view")
    } else {
        None
    }
}

/// Replaces the extension's starting zoom with the saved one, if the saved
/// node can be zoomed now, as `set_zoomed` allows.
fn give_zoom(state: &mut WindowState, given: Option<String>) {
    let Some(saved) = given else {
        state.zoomed = None;
        return;
    };
    let node = NodeId::parse(&saved)
        .ok()
        .filter(|node| zoom_refusal(state, node).is_none());
    if let Some(node) = node {
        tracing::debug!(node = %node, "zoom given");
        state.zoomed = Some(node);
    } else {
        tracing::debug!(node = %saved, "saved zoom not given");
        state.zoomed = None;
    }
}

/// A zoom in or out by the user: save it soon. The starting zoom an
/// extension sets before any window opens is not saved for itself.
fn view_state_changed(cx: &mut App) {
    if selected_window(cx).is_some() {
        state_changed(cx);
    }
}

fn session_window_state(cx: &App, id: gpui::WindowId) -> Option<Rc<RefCell<WindowState>>> {
    cx.try_global::<Registry>()?.windows.get(&id)?.upgrade()
}

/// The named layout a window shows and its user layer for each named
/// layout it has shown, as the session saves them.
pub(super) fn session_layers(
    cx: &App,
    id: gpui::WindowId,
) -> Option<(String, BTreeMap<String, Vec<Operation>>)> {
    let state_handle = session_window_state(cx, id)?;
    let (layout, layers) = {
        let state = state_handle.borrow();
        let layout = state.shown_layout.to_string();
        let mut layers = state.saved_layers.clone();
        layers.insert(layout.clone(), state.tree.borrow().user_layer().to_vec());
        (layout, layers)
    };
    // During the close callback the session asks for layers only if it is keeping this window.
    capture_kept_window(cx, id, &state_handle);
    Some((layout, layers))
}

fn create_view(
    node: &Node,
    parent: Option<(&NodeId, &ComponentId, &[SlotName], &SlotName)>,
    window: &mut Window,
    cx: &mut App,
) -> Option<AnyView> {
    let _window_scope = window_scope(window, cx);
    if parent.is_some_and(|(owner, _, _, slot)| {
        cx.global::<Registry>()
            .item_slots
            .contains_key(&(owner.clone(), slot.to_string()))
    }) || window_state(cx).borrow().failed_views.contains(&node.id)
    {
        return None;
    }
    let kept = window_state(cx)
        .borrow()
        .views
        .borrow()
        .get(&node.id)
        .cloned();
    let _group = kept.is_none().then(|| {
        tracing::info_span!(
            "component-create",
            node = %node.id,
            component = %node.component,
            extension = %node.extension,
        )
        .entered()
    });
    if let Some((_, component, declared, slot)) = parent
        && !declared.contains(slot)
    {
        window_state(cx)
            .borrow_mut()
            .failed_views
            .insert(node.id.clone());
        log::error!(
            "node {} is placed in slot {} not declared by parent component {}",
            node.id,
            slot,
            component
        );
        return None;
    }
    let (component, children, slots) = {
        let state_handle = window_state(cx);
        let layout_state = state_handle.borrow();
        let registry = cx.default_global::<Registry>();
        let component = registry.components.get(&node.component).cloned();
        let children = layout_state
            .tree
            .borrow()
            .current()
            .slots(&node.id)
            .map(|(slot, nodes)| (slot.clone(), nodes.to_vec()))
            .collect::<Vec<_>>();
        let slots = Slots {
            node: node.id.clone(),
            tree: layout_state.tree.clone(),
            views: layout_state.views.clone(),
            items: layout_state.item_children.clone(),
        };
        (component, children, slots)
    };
    let Some(component) = component else {
        window_state(cx)
            .borrow_mut()
            .failed_views
            .insert(node.id.clone());
        log::error!(
            "node {} names unregistered component {}",
            node.id,
            node.component
        );
        return None;
    };
    let created = kept.is_none();
    let view = kept.unwrap_or_else(|| {
        let handle = (!is_frame_node(&node.id)).then(|| {
            let handle = cx.focus_handle();
            window_state(cx)
                .borrow_mut()
                .focus_handles
                .insert(node.id.clone(), handle.clone());
            handle
        });
        let providers = cx
            .default_global::<Registry>()
            .providers
            .get(&node.component)
            .cloned()
            .unwrap_or_default();
        for provider in providers {
            let model = (provider.create)(&node.id, cx);
            window_state(cx)
                .borrow_mut()
                .contexts
                .entry(node.id.clone())
                .or_default()
                .insert(provider.model, model);
            tracing::debug!(node = %node.id, model = provider.name, "context created");
        }
        let view = (component.create)(node, slots.clone(), window, cx);
        restore_view_state(node, &view, window, cx);
        let wrapped: AnyView = cx
            .new(|_| NodeView {
                node: node.id.clone(),
                handle,
                view,
                zone: None,
            })
            .into();

        let state_handle = window_state(cx);
        let layout_state = state_handle.borrow();
        layout_state
            .views
            .borrow_mut()
            .insert(node.id.clone(), wrapped.clone());
        wrapped
    });
    for (slot, nodes) in children {
        for child in &nodes {
            let _ = create_view(
                child,
                Some((&node.id, &node.component, &component.slots, &slot)),
                window,
                cx,
            );
        }
    }
    if created
        && let Some(axis) = match node.component.as_str() {
            "yard.row" => Some("row"),
            "yard.column" => Some("column"),
            _ => None,
        }
    {
        tracing::debug!(
            axis,
            children = slots.views("children").len(),
            "split created"
        );
    }
    if created && node.component.as_str() == "yard.tabs" {
        tracing::debug!(tabs = slots.views("children").len(), "tabs created");
    }
    Some(view)
}

impl Render for Shell {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let _window_scope = window_context(window, cx);
        tracing::trace!("rendering shell");
        let zoomed = try_window_state(cx).and_then(|state_handle| {
            let layout_state = state_handle.borrow();
            layout_state.laid_out.borrow_mut().clear();
            layout_state.zoomed.clone()
        });
        // A zoomed node that becomes empty is zoomed out of, so that its
        // container gives it no room and the focus leaves it.
        if let Some(node) = zoomed
            && empty_now(cx, &node, Emptiness::Layout)
        {
            cx.defer_in(window, move |_, window, cx| {
                let _window_scope = window_scope(window, cx);
                if try_window_state(cx).is_some_and(|state_handle| {
                    let layout_state = state_handle.borrow();
                    layout_state.zoomed.as_ref() == Some(&node)
                }) {
                    tracing::debug!(node = %node, "zoomed node is empty");
                    Extension {
                        name: "yard".into(),
                    }
                    .zoom_out(cx);
                }
            });
        }
        cx.defer_in(window, |shell, window, cx| shell.sync_focus(window, cx));
        window.set_rem_size(yard_ui::ui_font_size(cx));
        let action_handlers = cx
            .try_global::<Registry>()
            .map(|registry| registry.action_handlers.clone())
            .unwrap_or_default();
        let view = self.root.clone();
        let modal_layer = self.modal_layer.clone();
        let mut root = div();
        for attach in action_handlers {
            root = attach(root);
        }
        let root = root
            .id("shell")
            .key_context("Shell")
            .track_focus(&self.handle)
            .on_action(|_: &NewWindow, _, cx| {
                let group = tracing::info_span!("window-new");
                let _group = group.enter();
                let registry = cx.default_global::<Registry>();
                if let Some(open) = registry.window_opener.clone() {
                    let group = group.clone();
                    // GPUI can cascade only after the source window leaves the update stack.
                    cx.defer(move |cx| {
                        let _group = group.enter();
                        open(cx);
                    });
                } else if !registry.warned_window_opener {
                    registry.warned_window_opener = true;
                    log::warn!("no window opener registered");
                } else {
                    tracing::warn!("no window opener registered");
                }
            })
            .on_action(|_: &ToggleZoom, window, cx| {
                let _window_scope = window_scope(window, cx);
                layout_action(LayoutAction::ToggleZoom, None, window, cx);
            })
            .on_action(|_: &Hide, window, cx| {
                let _window_scope = window_scope(window, cx);
                layout_action(LayoutAction::Hide, None, window, cx);
            })
            .on_action(|_: &SelectNextTab, window, cx| {
                let _window_scope = window_scope(window, cx);
                layout_action(LayoutAction::SelectNextTab, None, window, cx);
            })
            .on_action(|_: &FocusLeft, window, cx| {
                let _window_scope = window_scope(window, cx);
                layout_action(LayoutAction::FocusLeft, None, window, cx);
            })
            .on_action(|_: &FocusRight, window, cx| {
                let _window_scope = window_scope(window, cx);
                layout_action(LayoutAction::FocusRight, None, window, cx);
            })
            .on_action(|_: &FocusUp, window, cx| {
                let _window_scope = window_scope(window, cx);
                layout_action(LayoutAction::FocusUp, None, window, cx);
            })
            .on_action(|_: &FocusDown, window, cx| {
                let _window_scope = window_scope(window, cx);
                layout_action(LayoutAction::FocusDown, None, window, cx);
            })
            .on_action(|_: &SelectPreviousTab, window, cx| {
                let _window_scope = window_scope(window, cx);
                layout_action(LayoutAction::SelectPreviousTab, None, window, cx);
            })
            .relative()
            .size_full()
            .grid()
            .grid_cols(1)
            .grid_rows(1)
            .font(yard_ui::ui_font(cx))
            .on_drop::<DraggedNode>(|dragged, window, cx| {
                let _window_scope = window_scope(window, cx);
                refresh_after_drop(cx);
                tracing::debug!(node = %dragged.node, "drop ignored");
            })
            .children(view)
            .child(self.toast_layer.clone())
            .child(
                div()
                    .absolute()
                    .inset_0()
                    .size_full()
                    .on_action(move |_: &Dismiss, window, cx| {
                        let _window_scope = window_scope(window, cx);
                        modal_layer.update(cx, |layer, cx| {
                            layer.hide_modal(yard_ui::ModalHideReason::Escape, window, cx);
                        });
                    })
                    .child(self.modal_layer.clone()),
            );
        // The window's frame, drawn by YARD on Linux, and its background.
        yard_ui::client_decorations(root, window, cx)
    }
}

/// Handles `yard::Quit` for the whole application: it quits.
pub fn handle_quit(cx: &mut App) {
    cx.on_action(|_: &Quit, cx| {
        let _group = tracing::info_span!("app-quit").entered();
        cx.quit();
    });
}

/// Sets the application's menus.
pub fn set_menus(cx: &mut App, menus: Vec<gpui::Menu>) {
    let _group = tracing::info_span!("menus-set", count = menus.len()).entered();
    for menu in &menus {
        tracing::debug!(name = %menu.name, items = menu.items.len() as u64, "menu");
    }
    cx.set_menus(menus);
}

/// Gives the shell the composition root's operation for opening another window.
pub fn set_window_opener(cx: &mut App, open: impl Fn(&mut App) + 'static) {
    cx.default_global::<Registry>().window_opener = Some(Rc::new(open));
}
