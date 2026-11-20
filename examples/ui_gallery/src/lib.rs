mod key_hint;
mod scroll;
mod text_input;
mod tooltip;
mod window_controls;

use gpui::{
    App, AppContext, Context, Div, IntoElement, ParentElement, Render, Styled, Window, div, rems,
};
use yard_ui::{
    Button, ButtonSize, ButtonStyle, Divider, DividerColor, ElevationIndex, Headline, HeadlineSize,
    Icon, IconButton, IconButtonShape, IconName, IconSize, Label, LabelSize, List, ListHeader,
    ListItem, ListItemSpacing, ListSeparator, Modal, ModalFooter, ModalHeader, Section,
    SectionHeader, TextColor, TintColor, h_flex, v_flex,
};

const HEADLINE_SIZES: [(&str, HeadlineSize); 5] = [
    ("XLarge", HeadlineSize::XLarge),
    ("Large", HeadlineSize::Large),
    ("Medium", HeadlineSize::Medium),
    ("Small", HeadlineSize::Small),
    ("XSmall", HeadlineSize::XSmall),
];

const LABEL_SIZES: [(&str, LabelSize); 4] = [
    ("Large", LabelSize::Large),
    ("Default", LabelSize::Default),
    ("Small", LabelSize::Small),
    ("XSmall", LabelSize::XSmall),
];

const TEXT_COLORS: [(&str, TextColor); 8] = [
    ("Default", TextColor::Default),
    ("Muted", TextColor::Muted),
    ("Accent", TextColor::Accent),
    ("Disabled", TextColor::Disabled),
    ("Placeholder", TextColor::Placeholder),
    ("Error", TextColor::Error),
    ("Warning", TextColor::Warning),
    ("Success", TextColor::Success),
];

const ICON_SIZES: [IconSize; 5] = [
    IconSize::Indicator,
    IconSize::XSmall,
    IconSize::Small,
    IconSize::Medium,
    IconSize::XLarge,
];

const DIVIDER_COLORS: [DividerColor; 3] = [
    DividerColor::Border,
    DividerColor::BorderFaded,
    DividerColor::BorderVariant,
];

pub fn init(cx: &mut App, ext: &yard_shell::Extension) {
    let case = std::env::var("UI_GALLERY").unwrap_or_default();
    if case.is_empty() {
        return;
    }
    match case.as_str() {
        "all" => {
            let pages = 11;
            let _group = tracing::info_span!("gallery-build", pages).entered();
            ext.register(cx, "ui_gallery.text", &[], |_, _, _, cx| {
                cx.new(|_| TextPage).into()
            });
            ext.register(cx, "ui_gallery.icon", &[], |_, _, _, cx| {
                cx.new(|_| IconPage).into()
            });
            ext.register(cx, "ui_gallery.button", &[], |_, _, _, cx| {
                cx.new(|_| ButtonPage {
                    clicks: 0,
                    toggled: false,
                })
                .into()
            });
            ext.register(cx, "ui_gallery.divider", &[], |_, _, _, cx| {
                cx.new(|_| DividerPage).into()
            });
            ext.register(cx, "ui_gallery.modal", &[], |_, _, _, cx| {
                cx.new(|_| ModalPage { dismissals: 0 }).into()
            });
            ext.register(cx, "ui_gallery.list", &[], |_, _, _, cx| {
                cx.new(|_| ListPage::new()).into()
            });
            text_input::register(cx, ext);
            scroll::register(cx, ext);
            key_hint::register(cx, ext);
            tooltip::register(cx, ext);
            window_controls::register(cx, ext);
            ext.place_root(cx, "root", "yard.tabs");
            ext.place(cx, "text", "ui_gallery.text", "root", "children");
            tracing::debug!(page = "text", "page placed");
            ext.place(cx, "icon", "ui_gallery.icon", "root", "children");
            tracing::debug!(page = "icon", "page placed");
            ext.place(cx, "button", "ui_gallery.button", "root", "children");
            tracing::debug!(page = "button", "page placed");
            ext.place(cx, "divider", "ui_gallery.divider", "root", "children");
            tracing::debug!(page = "divider", "page placed");
            ext.place(cx, "modal", "ui_gallery.modal", "root", "children");
            tracing::debug!(page = "modal", "page placed");
            {
                ext.place(cx, "list", "ui_gallery.list", "root", "children");
                tracing::debug!(page = "list", "page placed");
            }
            {
                ext.place(
                    cx,
                    "text-input",
                    "ui_gallery.text_input",
                    "root",
                    "children",
                );
                tracing::debug!(page = "text-input", "page placed");
            }
            {
                ext.place(cx, "scroll", "ui_gallery.scroll", "root", "children");
                tracing::debug!(page = "scroll", "page placed");
            }
            key_hint::place(cx, ext);
            tooltip::place(cx, ext);
            window_controls::place(cx, ext);
        }
        "window-controls" => {
            let _group = tracing::info_span!("gallery-build", pages = 1).entered();
            window_controls::register(cx, ext);
            window_controls::place_alone(cx, ext);
        }
        _ => log::warn!("unknown UI_GALLERY case {case:?}; known cases: all, window-controls"),
    }
}

struct TextPage;

struct IconPage;

fn group(title: &str, content: impl IntoElement) -> Div {
    v_flex()
        .gap(rems(0.25))
        .child(
            h_flex()
                .gap(rems(0.75))
                .child(
                    Label::new(title.to_uppercase())
                        .size(LabelSize::XSmall)
                        .color(TextColor::Muted),
                )
                .child(
                    div()
                        .flex_1()
                        .child(Divider::horizontal().color(DividerColor::Border)),
                ),
        )
        .child(content)
}

impl Render for TextPage {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        tracing::trace!(page = "text", "rendering page");
        let palette = yard_ui::palette(cx);
        v_flex()
            .size_full()
            .p(rems(1.))
            .gap(rems(0.75))
            .bg(palette.background)
            .child(group(
                "Headline",
                v_flex().gap(rems(0.25)).children(
                    HEADLINE_SIZES
                        .into_iter()
                        .map(|(name, size)| Headline::new(name).size(size)),
                ),
            ))
            .child(group(
                "Label size",
                v_flex().gap(rems(0.25)).children(
                    LABEL_SIZES
                        .into_iter()
                        .map(|(name, size)| Label::new(name).size(size)),
                ),
            ))
            .child(group(
                "Label color",
                v_flex().gap(rems(0.25)).children(
                    TEXT_COLORS
                        .into_iter()
                        .map(|(name, color)| Label::new(name).color(color)),
                ),
            ))
            .child(group(
                "Row",
                h_flex()
                    .gap(rems(0.75))
                    .children(["one", "two", "three"].map(Label::new)),
            ))
            .child(group(
                "Column",
                v_flex()
                    .gap(rems(0.25))
                    .children(["one", "two", "three"].map(Label::new)),
            ))
    }
}

impl Render for IconPage {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        tracing::trace!(page = "icon", "rendering page");
        let palette = yard_ui::palette(cx);
        v_flex()
            .size_full()
            .p(rems(1.))
            .gap(rems(0.75))
            .bg(palette.background)
            .child(group(
                "Size",
                v_flex()
                    .gap(rems(0.25))
                    .children(ICON_SIZES.into_iter().map(|size| {
                        h_flex()
                            .gap(rems(0.5))
                            .children(IconName::ALL.iter().map(|&name| Icon::new(name).size(size)))
                    })),
            ))
            .child(group(
                "Color",
                h_flex().gap(rems(0.5)).children(
                    TEXT_COLORS
                        .into_iter()
                        .map(|(_, color)| Icon::new(IconName::Check).color(color)),
                ),
            ))
            .child(group(
                "Name",
                v_flex()
                    .gap(rems(0.25))
                    .children(IconName::ALL.iter().map(|&name| {
                        h_flex()
                            .gap(rems(0.5))
                            .child(Icon::new(name))
                            .child(Label::new(format!("{name:?}")))
                    })),
            ))
    }
}

struct ButtonPage {
    clicks: usize,
    toggled: bool,
}

impl Render for ButtonPage {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        tracing::trace!(page = "button", "rendering page");
        let palette = yard_ui::palette(cx);
        let counter = cx.weak_entity();
        let toggle = cx.weak_entity();
        v_flex()
            .size_full()
            .p(rems(1.))
            .gap(rems(0.75))
            .bg(palette.background)
            .child(group(
                "Style",
                h_flex().flex_wrap().gap(rems(0.5)).children(
                    [
                        ButtonStyle::Filled,
                        ButtonStyle::Tinted(TintColor::Accent),
                        ButtonStyle::Tinted(TintColor::Error),
                        ButtonStyle::Tinted(TintColor::Warning),
                        ButtonStyle::Tinted(TintColor::Success),
                        ButtonStyle::Outlined,
                        ButtonStyle::Subtle,
                        ButtonStyle::Transparent,
                    ]
                    .into_iter()
                    .enumerate()
                    .map(|(index, style)| {
                        Button::new(("style", index), format!("{style:?}")).style(style)
                    }),
                ),
            ))
            .child(group(
                "Size",
                h_flex().flex_wrap().gap(rems(0.5)).children(
                    [
                        ButtonSize::Large,
                        ButtonSize::Medium,
                        ButtonSize::Default,
                        ButtonSize::Compact,
                        ButtonSize::None,
                    ]
                    .into_iter()
                    .enumerate()
                    .map(|(index, size)| {
                        Button::new(("size", index), format!("{size:?}")).size(size)
                    }),
                ),
            ))
            .child(group(
                "Icon",
                h_flex()
                    .flex_wrap()
                    .gap(rems(0.5))
                    .child(Button::new("start", "Start icon").start_icon(IconName::Check))
                    .child(Button::new("end", "End icon").end_icon(IconName::ChevronDown))
                    .child(
                        Button::new("both", "Both icons")
                            .start_icon(IconName::Check)
                            .end_icon(IconName::ChevronDown),
                    )
                    .child(IconButton::new("square", IconName::Plus).shape(IconButtonShape::Square))
                    .child(IconButton::new("wide", IconName::Plus).shape(IconButtonShape::Wide)),
            ))
            .child(group(
                "State",
                h_flex()
                    .flex_wrap()
                    .gap(rems(0.5))
                    .child(Button::new("disabled", "Disabled").disabled(true))
                    .child(IconButton::new("disabled-icon", IconName::Close).disabled(true))
                    .child(Button::new("toggled", "Toggled").toggle_state(true))
                    .child(IconButton::new("toggled-icon", IconName::Check).toggle_state(true)),
            ))
            .child(group(
                "Click",
                h_flex()
                    .gap(rems(0.5))
                    .child(
                        Button::new("count-clicks", format!("clicked {} times", self.clicks))
                            .on_click(move |_, cx| {
                                let _ = counter.update(cx, |this, cx| {
                                    this.clicks += 1;
                                    tracing::info!(count = this.clicks, "button clicked");
                                    cx.notify();
                                });
                            }),
                    )
                    .child(
                        IconButton::new("toggle-clicks", IconName::Check)
                            .toggle_state(self.toggled)
                            .on_click(move |_, cx| {
                                let _ = toggle.update(cx, |this, cx| {
                                    this.toggled = !this.toggled;
                                    tracing::info!(state = this.toggled, "icon button toggled");
                                    cx.notify();
                                });
                            }),
                    ),
            ))
    }
}

struct DividerPage;

impl Render for DividerPage {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        tracing::trace!(page = "divider", "rendering page");
        let palette = yard_ui::palette(cx);
        v_flex()
            .size_full()
            .p(rems(1.))
            .gap(rems(0.75))
            .bg(palette.background)
            .child(group(
                "Horizontal",
                v_flex()
                    .gap(rems(0.75))
                    .children(DIVIDER_COLORS.into_iter().map(|color| {
                        v_flex()
                            .gap(rems(0.25))
                            .child(Label::new(format!("{color:?} — solid")))
                            .child(Divider::horizontal().color(color))
                            .child(Label::new(format!("{color:?} — dashed")))
                            .child(Divider::horizontal_dashed().color(color))
                            .child(Label::new(format!("{color:?} — inset")))
                            .child(Divider::horizontal().inset().color(color))
                            .child(Label::new(format!("{color:?}")))
                    })),
            ))
            .child(group(
                "Vertical",
                h_flex()
                    .h(rems(1.5))
                    .gap(rems(0.75))
                    .child(Label::new("Border"))
                    .child(Divider::vertical().color(DividerColor::Border))
                    .child(Label::new("Dashed"))
                    .child(Divider::vertical_dashed().color(DividerColor::Border))
                    .child(Label::new("BorderFaded"))
                    .child(Divider::vertical().color(DividerColor::BorderFaded))
                    .child(Label::new("Dashed"))
                    .child(Divider::vertical_dashed().color(DividerColor::BorderFaded))
                    .child(Label::new("BorderVariant"))
                    .child(Divider::vertical().color(DividerColor::BorderVariant))
                    .child(Label::new("Dashed"))
                    .child(Divider::vertical_dashed().color(DividerColor::BorderVariant))
                    .child(Label::new("End")),
            ))
    }
}

struct ModalPage {
    dismissals: usize,
}

impl ModalPage {
    fn dismiss(&mut self, cx: &mut Context<Self>) {
        self.dismissals += 1;
        tracing::info!(count = self.dismissals, "modal dismissed");
        cx.notify();
    }
}

impl Render for ModalPage {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        tracing::trace!(page = "modal", "rendering page");
        let palette = yard_ui::palette(cx);
        let close = cx.weak_entity();
        let cancel = cx.weak_entity();
        v_flex()
            .size_full()
            .p(rems(1.))
            .gap(rems(0.75))
            .bg(palette.background)
            .child(group(
                "Header",
                h_flex()
                    .items_start()
                    .flex_wrap()
                    .gap(rems(0.75))
                    .child(
                        Modal::new("headline-only").header(ModalHeader::new().headline("Headline")),
                    )
                    .child(
                        Modal::new("with-description").header(
                            ModalHeader::new()
                                .headline("Headline")
                                .description("A description"),
                        ),
                    )
                    .child(
                        Modal::new("with-close").header(
                            ModalHeader::new()
                                .headline("Headline")
                                .description("A description")
                                .on_dismiss(|_, _| tracing::info!("header modal dismissed")),
                        ),
                    ),
            ))
            .child(group(
                "Sections",
                Modal::new("sections")
                    .section(Section::new().child(Label::new("A section without a header")))
                    .section(
                        Section::new()
                            .header(SectionHeader::new("First section"))
                            .child(Label::new("First section text")),
                    )
                    .section(
                        Section::new()
                            .header(SectionHeader::new("Second section"))
                            .child(Label::new("Second section text")),
                    ),
            ))
            .child(group(
                "Footer",
                h_flex()
                    .items_start()
                    .flex_wrap()
                    .gap(rems(0.75))
                    .child(
                        Modal::new("end-only").footer(
                            ModalFooter::new().end_slot(
                                h_flex()
                                    .gap(rems(0.5))
                                    .child(Button::new("end-cancel", "Cancel"))
                                    .child(
                                        Button::new("end-confirm", "Confirm")
                                            .style(ButtonStyle::Filled)
                                            .layer(ElevationIndex::ModalSurface),
                                    ),
                            ),
                        ),
                    )
                    .child(
                        Modal::new("both-slots").footer(
                            ModalFooter::new()
                                .start_slot(
                                    Label::new("Additional information").color(TextColor::Muted),
                                )
                                .end_slot(
                                    h_flex()
                                        .gap(rems(0.5))
                                        .child(Button::new("both-cancel", "Cancel"))
                                        .child(
                                            Button::new("both-confirm", "Confirm")
                                                .style(ButtonStyle::Filled)
                                                .layer(ElevationIndex::ModalSurface),
                                        ),
                                ),
                        ),
                    ),
            ))
            .child(group(
                "Dismiss",
                Modal::new("dismiss-counter")
                    .header(
                        ModalHeader::new()
                            .headline("Dismiss")
                            .description("The owner counts dismissals")
                            .on_dismiss(move |_, cx| {
                                let _ = close.update(cx, |this, cx| this.dismiss(cx));
                            }),
                    )
                    .section(
                        Section::new()
                            .header(SectionHeader::new("Dismissals"))
                            .child(Label::new(format!("dismissed {} times", self.dismissals))),
                    )
                    .footer(
                        ModalFooter::new().end_slot(
                            h_flex()
                                .gap(rems(0.5))
                                .child(Button::new("dismiss-cancel", "Cancel").on_click(
                                    move |_, cx| {
                                        let _ = cancel.update(cx, |this, cx| this.dismiss(cx));
                                    },
                                ))
                                .child(
                                    Button::new("dismiss-confirm", "Confirm")
                                        .style(ButtonStyle::Filled)
                                        .layer(ElevationIndex::ModalSurface),
                                ),
                        ),
                    ),
            ))
    }
}

struct ThemeRow {
    id: &'static str,
    label: &'static str,
    icon: IconName,
    disabled: bool,
    shortcut: Option<&'static str>,
}

const THEME_ROWS: [ThemeRow; 5] = [
    ThemeRow {
        id: "one-dark",
        label: "One Dark",
        icon: IconName::Check,
        disabled: false,
        shortcut: None,
    },
    ThemeRow {
        id: "one-light",
        label: "One Light",
        icon: IconName::Info,
        disabled: false,
        shortcut: None,
    },
    ThemeRow {
        id: "ayu-dark",
        label: "Ayu Dark",
        icon: IconName::Plus,
        disabled: false,
        shortcut: None,
    },
    ThemeRow {
        id: "ayu-mirage",
        label: "Ayu Mirage",
        icon: IconName::Warning,
        disabled: true,
        shortcut: None,
    },
    ThemeRow {
        id: "gruvbox-dark",
        label: "Gruvbox Dark",
        icon: IconName::ChevronRight,
        disabled: false,
        shortcut: Some("cmd-k cmd-t"),
    },
];

const SPARSE_ROWS: [(&str, &str); 3] = [
    ("sparse-one-dark", "One Dark"),
    ("sparse-one-light", "One Light"),
    ("sparse-ayu-dark", "Ayu Dark"),
];

const INDENT_ROWS: [(usize, &str, &str); 3] = [
    (0, "indent-0", "Level 0"),
    (1, "indent-1", "Level 1"),
    (2, "indent-2", "Level 2"),
];

struct ListPage {
    selected: usize,
    sparse_selected: usize,
    indent_selected: Option<usize>,
}

impl ListPage {
    fn new() -> Self {
        Self {
            selected: 1,
            sparse_selected: 0,
            indent_selected: None,
        }
    }
}

impl Render for ListPage {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        tracing::trace!(page = "list", "rendering page");
        let palette = yard_ui::palette(cx);
        let selected = self.selected;
        let sparse_selected = self.sparse_selected;
        let indent_selected = self.indent_selected;
        let page = cx.weak_entity();
        let sparse_page = cx.weak_entity();
        let indent_page = cx.weak_entity();

        let themes = List::new().header(ListHeader::new("Themes")).children(
            THEME_ROWS.into_iter().enumerate().map(|(index, row)| {
                let page = page.clone();
                let icon_color = if row.disabled {
                    TextColor::Disabled
                } else {
                    TextColor::Muted
                };
                let label_color = if row.disabled {
                    TextColor::Disabled
                } else {
                    TextColor::Default
                };
                let mut item = ListItem::new(row.id)
                    .toggle_state(selected == index)
                    .disabled(row.disabled)
                    .start_slot(Icon::new(row.icon).size(IconSize::Small).color(icon_color))
                    .child(Label::new(row.label).color(label_color))
                    .on_click(move |_, cx| {
                        let _ = page.update(cx, |this, cx| {
                            this.selected = index;
                            tracing::info!(id = row.id, index, "list item selected");
                            cx.notify();
                        });
                    });
                if let Some(shortcut) = row.shortcut {
                    item = item.end_slot(
                        Label::new(shortcut)
                            .size(LabelSize::Small)
                            .color(TextColor::Muted),
                    );
                }
                item
            }),
        );

        let sparse = List::new().children(SPARSE_ROWS.into_iter().enumerate().map(
            |(index, (id, label))| {
                let sparse_page = sparse_page.clone();
                ListItem::new(id)
                    .spacing(ListItemSpacing::Sparse)
                    .inset(true)
                    .toggle_state(sparse_selected == index)
                    .child(Label::new(label))
                    .on_click(move |_, cx| {
                        let _ = sparse_page.update(cx, |this, cx| {
                            this.sparse_selected = index;
                            tracing::info!(id, index, "sparse list item selected");
                            cx.notify();
                        });
                    })
            },
        ));

        let indented = List::new().children(INDENT_ROWS.into_iter().map(|(level, id, label)| {
            let indent_page = indent_page.clone();
            ListItem::new(id)
                .indent_level(level)
                .toggle_state(indent_selected == Some(level))
                .child(Label::new(label))
                .on_click(move |_, cx| {
                    let _ = indent_page.update(cx, |this, cx| {
                        this.indent_selected = Some(level);
                        tracing::info!(id, level, "indented list item selected");
                        cx.notify();
                    });
                })
        }));

        v_flex()
            .size_full()
            .p(rems(1.))
            .gap(rems(0.75))
            .bg(palette.background)
            .child(group("Items", themes))
            .child(group("Separator", ListSeparator))
            .child(group("Sparse", sparse))
            .child(group("Indent", indented))
            .child(group("Empty", List::new()))
    }
}
