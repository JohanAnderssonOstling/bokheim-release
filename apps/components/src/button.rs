//! Application buttons: layout, colors and interaction belong to the app.

use gpui::prelude::*;
use gpui::{AnyElement, App, ClickEvent, Div, ElementId, Interactivity, MouseButton, Refineable, RenderOnce, Role, SharedString, Stateful, StyleRefinement, Window, div, rems, transparent_black};
use gpui_component::global_state::GlobalState;
use gpui_component::menu::DropdownMenu;
use gpui_component::spinner::Spinner;
use gpui_component::tooltip::Tooltip;
use gpui_component::{Disableable, Icon, Selectable, Sizable, Size, StyledExt};

use crate::{TEXT_SM, TEXT_XS, browser_theme};

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum Variant {
    #[default]
    Default,
    Primary,
    Danger,
    Ghost,
}

type ClickHandler = Box<dyn Fn(&ClickEvent, &mut Window, &mut App)>;

/// Shared button behavior for navigation, settings and actions. GPUI provides
/// focus and mouse/keyboard activation; this component owns the visual states.
#[derive(IntoElement)]
pub struct Button {
    id: ElementId,
    base: Stateful<Div>,
    style: StyleRefinement,
    hover_style: StyleRefinement,
    variant: Variant,
    selected: bool,
    disabled: bool,
    loading: bool,
    size: Size,
    label: Option<SharedString>,
    icon: Option<Icon>,
    children: Vec<AnyElement>,
    tooltip: Option<SharedString>,
    on_click: Option<ClickHandler>,
}

impl Button {
    pub fn new(id: impl Into<ElementId>) -> Self {
        let id = id.into();
        Self {
            base: div().id(id.clone()),
            id,
            style: StyleRefinement::default(),
            hover_style: StyleRefinement::default(),
            variant: Variant::Default,
            selected: false,
            disabled: false,
            loading: false,
            size: Size::Medium,
            label: None,
            icon: None,
            children: Vec::new(),
            tooltip: None,
            on_click: None,
        }
    }

    pub fn primary(mut self) -> Self {
        self.variant = Variant::Primary;
        self
    }

    pub fn danger(mut self) -> Self {
        self.variant = Variant::Danger;
        self
    }

    pub fn ghost(mut self) -> Self {
        self.variant = Variant::Ghost;
        self
    }

    pub fn label(mut self, label: impl Into<SharedString>) -> Self {
        self.label = Some(label.into());
        self
    }

    pub fn icon(mut self, icon: impl Into<Icon>) -> Self {
        self.icon = Some(icon.into());
        self
    }

    pub fn tooltip(mut self, tooltip: impl Into<SharedString>) -> Self {
        self.tooltip = Some(tooltip.into());
        self
    }

    pub fn loading(mut self, loading: bool) -> Self {
        self.loading = loading;
        self
    }

    pub fn on_click(mut self, handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> Self {
        self.on_click = Some(Box::new(handler));
        self
    }

    /// Refine the component's hover without installing a second GPUI hover.
    pub fn hover(mut self, style: impl FnOnce(StyleRefinement) -> StyleRefinement) -> Self {
        self.hover_style = style(self.hover_style);
        self
    }
}

impl Disableable for Button {
    fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }
}

impl Selectable for Button {
    fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    fn is_selected(&self) -> bool {
        self.selected
    }
}

impl Sizable for Button {
    fn with_size(mut self, size: impl Into<Size>) -> Self {
        self.size = size.into();
        self
    }
}

impl Styled for Button {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

impl ParentElement for Button {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements);
    }
}

impl InteractiveElement for Button {
    fn interactivity(&mut self) -> &mut Interactivity {
        self.base.interactivity()
    }
}

impl DropdownMenu for Button {}

impl From<Button> for AnyElement {
    fn from(button: Button) -> Self {
        button.into_any_element()
    }
}

impl RenderOnce for Button {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = browser_theme(cx);
        let unavailable = self.disabled || self.loading;
        let focus = window.use_keyed_state(self.id, cx, |_, cx| cx.focus_handle()).read(cx).clone().tab_index(0).tab_stop(true);
        let focused = !unavailable && focus.is_focused(window);
        let semantic = matches!(self.variant, Variant::Primary | Variant::Danger);
        let filled = semantic && !cfg!(feature = "kobo");
        let danger = self.variant == Variant::Danger;
        let background = if filled {
            if danger { theme.danger } else { theme.accent }
        } else if self.variant == Variant::Ghost {
            transparent_black()
        } else {
            theme.page_bg
        };
        let foreground = if filled {
            if danger { theme.danger_surface } else { theme.accent_text }
        } else if danger {
            theme.danger
        } else {
            theme.text
        };
        let hover = if self.selected || (filled && !danger) {
            theme.accent_hover
        } else if danger {
            if filled { theme.danger.opacity(0.88) } else { theme.danger_surface }
        } else {
            theme.hover
        };
        let active = if self.selected && cfg!(feature = "kobo") {
            theme.hover
        } else if self.selected || (filled && !danger) {
            theme.accent_active
        } else if danger {
            if filled { theme.danger.opacity(0.76) } else { theme.danger_surface }
        } else {
            theme.accent_wash
        };
        let rem = window.rem_size();
        let (height, padding, text_size) = match self.size {
            Size::XSmall => (rem * 1.25, rem * 0.25, 0.75),
            Size::Small => (rem * 1.5, rem * 0.5, TEXT_XS),
            Size::Medium => (rem * 2.0, rem * 0.625, TEXT_SM),
            Size::Large => (rem * 2.0, rem * 0.75, TEXT_SM),
            Size::Size(size) => (size, size * 0.2, TEXT_SM),
        };
        let icon_only = self.label.is_none() && self.children.is_empty();
        let icon_size = match self.size {
            Size::Size(size) => Size::Size(size * 0.75),
            size => size,
        };
        let mut button = self
            .base
            .role(Role::Button)
            .aria_selected(self.selected)
            .when_some(self.label.clone(), |button, label| button.aria_label(label))
            .relative()
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .gap(rems(if self.size == Size::Small || self.size == Size::XSmall { 0.25 } else { 0.5 }))
            .when(self.style.size.height.is_none(), |button| button.min_h(height))
            .px(padding)
            .text_size(rems(text_size))
            .line_height_auto()
            .bg(background)
            .text_color(foreground)
            .when(icon_only, |button| button.w(height).p_0())
            .when(self.variant == Variant::Default || (semantic && cfg!(feature = "kobo")), |button| button.border_1().border_color(if semantic { if danger { theme.danger } else { theme.accent } } else { theme.rule }))
            .when(self.selected && !cfg!(feature = "kobo"), |button| button.bg(theme.accent).text_color(theme.accent_text))
            .when(self.selected && cfg!(feature = "kobo"), |button| button.bg(theme.page_bg).text_color(theme.text).border_2().border_color(theme.accent))
            .refine_style(&self.style)
            .when(!unavailable, |button| {
                button
                    .track_focus(&focus)
                    .hover(move |style| {
                        let mut style = style.bg(hover);
                        style.refine(&self.hover_style);
                        style
                    })
                    .active(move |style| style.bg(active))
            })
            .when(unavailable, |button| button.cursor_default().when(self.selected, |button| button.opacity(0.5)).when(!self.selected, |button| button.bg(theme.page_bg).text_color(theme.text_muted.opacity(0.5)).border_color(theme.rule)))
            .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                if unavailable {
                    cx.stop_propagation();
                    return;
                }
                window.prevent_default();
                GlobalState::suppress_text_selection(cx);
            })
            .when_some(self.on_click, |button, handler| {
                button.on_click(move |event, window, cx| {
                    if unavailable {
                        cx.stop_propagation();
                        return;
                    }
                    handler(event, window, cx);
                })
            })
            .when_some(self.icon, |button, icon| button.child(if self.loading { Spinner::new().with_size(icon_size).into_any_element() } else { icon.with_size(icon_size).into_any_element() }))
            .children(self.label)
            .children(self.children)
            .when(focused, |button| button.child(div().absolute().inset_0().border_1().border_color(theme.accent.opacity(0.4))));
        if let Some(tooltip) = self.tooltip {
            button = button.tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx));
        }
        button
    }
}
