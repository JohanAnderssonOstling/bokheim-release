//! Account section visuals: the identity block, the storage meter, and the
//! library usage rows that read against it — each of which reports what its
//! library is doing when it is doing something.
//!
//! The section opens with who is signed in, and the sync state is one line
//! inside that block rather than a banner above it. Only that line and the
//! activity dots carry a semantic colour, and only while something is actually
//! waiting; everything else is quiet.

use gpui::prelude::*;
use gpui::{AnyElement, Div, FontWeight, Hsla, SharedString, div, px, relative};
use gpui_component::button::{Button, ButtonVariants};

use crate::{BrowserTheme, SPACE_MD, SPACE_SM, SPACE_XS, SPACE_XXS, TEXT_SM, TEXT_XS, TITLE_LG};

/// Height of the account quota meter.
const METER_HEIGHT: f32 = 10.0;
/// The initial beside the address in the header chip.
const ACCOUNT_AVATAR_SIZE_REM: f32 = 1.5;
/// How much header an address may take before it truncates.
const ACCOUNT_CHIP_MAX_WIDTH_REM: f32 = 16.0;
const EVENT_DOT_SIZE: f32 = 8.0;
const PROGRESS_HEIGHT: f32 = 3.0;
/// Keeps a library row the height it had when its name was a button, so the
/// list reads at the same rhythm as the rest of the account section.
const LIBRARY_NAME_HEIGHT_REM: f32 = 2.5;
/// The label column of a library's facts. Wide enough for the longest of them —
/// "On this device" — so every value in the list starts at the same x and the
/// column can be read downwards.
const LIBRARY_FACT_LABEL_WIDTH_REM: f32 = 7.0;
const LIBRARY_FACT_LABEL_MAX_WIDTH: f32 = 160.0;
/// The height a fact's value keeps whatever it carries, so a row of text and a
/// row holding a control sit on the same rhythm.
const LIBRARY_FACT_HEIGHT_REM: f32 = 1.5;

/// Who is signed in, as the control in the account group's header.
///
/// The account was a block of its own — avatar, address, a status line and a
/// sign-out button — which is a lot of page for a thing you touch twice a year.
/// It is a chip now: the address is the only part worth a permanent line, and
/// what you can do to the account hangs off it in a menu.
///
/// The address truncates rather than wrapping; the caller gives the button the
/// full address as its tooltip, because a header cannot grow.
pub fn account_chip(id: impl Into<gpui::ElementId>, initial: impl Into<SharedString>, email: impl Into<SharedString>, theme: BrowserTheme) -> Button {
    crate::base_button(id).ghost().h(gpui::rems(crate::SETTINGS_CONTROL_SIZE_REM)).max_w(gpui::rems(ACCOUNT_CHIP_MAX_WIDTH_REM)).px(px(SPACE_XS)).rounded(px(crate::RADIUS_SM)).border_1().border_color(theme.border).child(
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(SPACE_SM))
            .min_w_0()
            .child(
                div()
                    .w(gpui::rems(ACCOUNT_AVATAR_SIZE_REM))
                    .h(gpui::rems(ACCOUNT_AVATAR_SIZE_REM))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .bg(theme.accent)
                    .text_color(theme.accent_text)
                    .text_size(gpui::rems(TEXT_XS))
                    .child(initial.into()),
            )
            .child(div().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().text_size(gpui::rems(TEXT_XS)).text_color(theme.text).child(email.into())),
    )
}

/// The quota as a figure rather than a sentence: the used amount is the largest
/// text on the page after the verdict, and what is left sits opposite it.
pub fn storage_figures(used: impl Into<SharedString>, of_quota: impl Into<SharedString>, available: impl Into<SharedString>, theme: BrowserTheme) -> Div {
    div()
        .w_full()
        .flex()
        .flex_row()
        .items_baseline()
        .justify_between()
        .gap(px(SPACE_MD))
        .child(div().flex().flex_row().items_baseline().gap(px(SPACE_XS)).child(div().text_size(gpui::rems(TITLE_LG)).child(used.into())).child(div().text_size(gpui::rems(TEXT_SM)).text_color(theme.text_muted).child(of_quota.into())))
        .child(div().text_size(gpui::rems(TEXT_SM)).text_color(theme.text_muted).child(available.into()))
}

/// The quota meter and its matching legend. Each tuple is `(fraction, color,
/// label)`; fractions are clamped to the track while labels retain the same
/// color association below it.
pub fn storage_meter(segments: impl IntoIterator<Item = (f32, Hsla, SharedString)>, theme: BrowserTheme) -> Div {
    let mut track = div().w_full().h(px(METER_HEIGHT)).flex().flex_row().overflow_hidden().border_1().border_color(theme.rule).bg(theme.page_bg);
    let mut legend = div().w_full().flex().flex_row().flex_wrap().gap_x(px(SPACE_MD)).gap_y(px(SPACE_XS));
    for (fraction, color, label) in segments {
        track = track.child(div().h_full().w(relative(fraction.clamp(0.0, 1.0))).bg(color));
        legend = legend.child(div().flex().flex_row().items_center().gap(px(SPACE_XS)).child(div().w(px(8.0)).h(px(8.0)).flex_none().bg(color)).child(div().text_size(gpui::rems(TEXT_XS)).text_color(theme.text_muted).child(label)));
    }
    div().w_full().flex().flex_col().gap(px(SPACE_SM)).child(track).child(legend)
}

/// Shades for the quota segments, walking the accent away from itself so
/// adjacent libraries stay distinguishable without introducing a second hue.
///
/// Past the shades that stay legible the sequence repeats rather than fading
/// into the track; a library with a colour it shares with another is a smaller
/// error than a library that appears to occupy nothing.
pub fn usage_segment_color(index: usize, theme: BrowserTheme) -> Hsla {
    const STEPS: usize = 5;
    let step = (index % STEPS) as f32 / STEPS as f32;
    let mut color = theme.accent;
    color.l = (color.l + step * 0.34).clamp(0.0, 0.92);
    color.s = (color.s - step * 0.18).clamp(0.0, 1.0);
    color
}

/// A complete library-usage list with the shared row rhythm.
///
/// A library is several lines tall — its facts, and whatever it is doing — so
/// the rows are ruled apart. Padding alone told two libraries apart while a row
/// was one line; it stopped doing so the moment a row became a block, and the
/// group's border cannot say where one library ends and the next begins.
///
/// The rule goes between rows rather than under every one of them: the last
/// library must not be followed by a hairline that reads as an empty row, and
/// the first must not be cut off from the heading that names the list.
pub fn library_usage_list(rows: impl IntoIterator<Item = Div>, theme: BrowserTheme) -> Div {
    let mut list = div().w_full().flex().flex_col();
    for (index, row) in rows.into_iter().enumerate() {
        list = list.child(if index == 0 { row } else { row.border_t_1().border_color(theme.rule) });
    }
    list
}

/// The list with no libraries in it. A line of its own rather than an empty
/// group, which would read as something that failed to load.
pub fn library_usage_empty(text: impl Into<SharedString>, theme: BrowserTheme) -> Div {
    div().w_full().px(px(SPACE_MD)).py(px(SPACE_SM)).min_h(gpui::rems(LIBRARY_NAME_HEIGHT_REM)).flex().items_center().text_size(gpui::rems(TEXT_SM)).text_color(theme.text_muted).child(text.into())
}

/// What a library is doing right now, drawn under its name in the usage list.
///
/// It carries the same dot-and-progress vocabulary the account activity list
/// used, so a job reads the same whether it is running, queued or has failed;
/// the colour is the caller's, because only the caller knows which of those a
/// line is reporting.
/// `count` is what the job has done of what it has to do — "42 / 128" — kept
/// out of the sentence and in a column of its own, so several jobs on one
/// library are read down rather than hunted for in the middle of each line.
///
/// The track is drawn whether or not there is a fraction to put in it, so the
/// lines stay a column; an empty track says the job has no denominator, which
/// is itself worth knowing.
pub fn library_activity_note(state_color: Hsla, text: impl Into<SharedString>, count: Option<SharedString>, progress: Option<f32>, theme: BrowserTheme) -> Div {
    let track = div().min_w(px(48.0)).flex_1().h(px(PROGRESS_HEIGHT)).flex().flex_row().overflow_hidden().bg(theme.rule);
    div()
        .w_full()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(SPACE_SM))
        .child(div().w(px(EVENT_DOT_SIZE)).h(px(EVENT_DOT_SIZE)).flex_none().rounded(px(EVENT_DOT_SIZE / 2.0)).bg(state_color))
        .child(div().min_w_0().text_size(gpui::rems(TEXT_XS)).text_color(theme.text).child(text.into()))
        .child(track.children(progress.map(|progress| div().h_full().w(relative(progress.clamp(0.0, 1.0))).bg(state_color))))
        .children(count.map(|count| div().flex_none().text_size(gpui::rems(TEXT_XS)).text_color(theme.text_muted).child(count)))
}

/// The name a library is read by in the server-storage list. Nothing is done to
/// a library from here — the switcher owns renaming, opening and deleting — so
/// it is a plain line of text rather than a control.
///
/// It carries no inset of its own: the row it sits in owns the inset, and a
/// name that padded itself was being unpadded again at the call site.
pub fn library_usage_name(name: impl Into<SharedString>, theme: BrowserTheme) -> Div {
    div().w_full().min_w_0().min_h(gpui::rems(LIBRARY_NAME_HEIGHT_REM)).flex().items_center().overflow_hidden().whitespace_nowrap().text_ellipsis().text_size(gpui::rems(TEXT_SM)).text_color(theme.text).child(name.into())
}

/// One library: its name, the facts about it, and whatever it is doing.
///
/// The facts are a label/value list rather than a line of values, because every
/// library states the same things — what it occupies here, where it stands with
/// the server, which folder it is, whether it is stored in the cloud — and a
/// reader comparing two libraries is reading down a column, not along a line.
/// Unlabelled values only worked while there were two of them; a size, a tag, a
/// path and a checkbox strung together say nothing about which is which.
///
/// Every control the row carries is a value in that list, so it shares the
/// row's one inset. They were previously appended to the row inside padding of
/// their own, which put a checkbox 32px from the group edge while the name it
/// belonged to sat at 16px.
pub fn library_usage_row(name: impl Into<SharedString>, facts: impl IntoIterator<Item = (SharedString, AnyElement)>, activity: impl IntoIterator<Item = Div>, compact: bool, theme: BrowserTheme) -> Div {
    let title = div()
        .w_full()
        .min_w_0()
        .min_h(gpui::rems(LIBRARY_FACT_HEIGHT_REM))
        .flex()
        .items_center()
        .overflow_hidden()
        .whitespace_nowrap()
        .text_ellipsis()
        .text_size(gpui::rems(TEXT_SM))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(theme.text)
        .child(name.into());
    let mut list = div().w_full().flex().flex_col().gap(px(SPACE_XS));
    for (label, value) in facts {
        list = list.child(library_fact(label, value, compact, theme));
    }
    div().w_full().px(px(SPACE_MD)).py(px(SPACE_MD)).flex().flex_col().gap(px(SPACE_SM)).child(title).child(list).children(activity)
}

/// The controls and less frequently needed facts revealed from a summary row.
pub fn library_usage_details(facts: impl IntoIterator<Item = (SharedString, AnyElement)>, compact: bool, theme: BrowserTheme) -> Div {
    let mut list = div().w_full().flex().flex_col().gap(px(SPACE_XS));
    for (label, value) in facts {
        list = list.child(library_fact(label, value, compact, theme));
    }
    list
}

/// One labelled fact about a library.
///
/// The label column is fixed rather than sized to its contents, because the
/// point of it is that the values line up; a column that fits each row's own
/// label is four columns, not one.
///
/// A compact window stacks the label over its value instead. The row does not
/// simply wrap: a wrapped value returns to the left edge and lands under the
/// label column, which reads as a third column appearing. Below the medium
/// width there is not enough left of the row to be worth 112px of label anyway.
fn library_fact(label: SharedString, value: AnyElement, compact: bool, theme: BrowserTheme) -> Div {
    let unlabelled = label.is_empty();
    let label = div().flex_none().text_size(gpui::rems(TEXT_XS)).text_color(theme.text_muted).child(label);
    let value = div().flex_1().min_w_0().min_h(gpui::rems(LIBRARY_FACT_HEIGHT_REM)).flex().flex_row().items_center().overflow_hidden().text_size(gpui::rems(TEXT_XS)).text_color(theme.text).child(value);
    // A control that names itself keeps the value column but takes no label.
    // Stacked, an empty label is an empty line, so only the value is drawn.
    if compact && unlabelled {
        return div().w_full().flex().child(value);
    }
    if compact {
        div().w_full().flex().flex_col().gap(px(SPACE_XXS)).child(label).child(value)
    } else {
        div().w_full().flex().flex_row().items_center().gap(px(SPACE_MD)).child(label.w(gpui::rems(LIBRARY_FACT_LABEL_WIDTH_REM)).max_w(px(LIBRARY_FACT_LABEL_MAX_WIDTH))).child(value)
    }
}

/// Vertical rhythm for the storage block/// The signed-out form: a title, the inputs, the actions, and whatever the last
/// attempt had to say, in one measure.
///
/// The inputs inside it carry their own placeholders and are never given a
/// label as well — a field named twice reads as two fields.
pub fn access_form() -> Div {
    div().w_full().flex().flex_col().gap(px(SPACE_SM))
}

/// The quiet line under an input that names a constraint the user cannot guess,
/// such as a minimum password length. Not a label: it says something the
/// placeholder does not.
pub fn form_hint(text: impl Into<SharedString>, theme: BrowserTheme) -> Div {
    div().text_size(gpui::rems(TEXT_XS)).text_color(theme.text_muted).child(text.into())
}

/// Vertical rhythm for the storage block: figures, meter, legend.
pub fn storage_block() -> Div {
    div().w_full().flex().flex_col().gap(px(SPACE_SM)).pt(px(SPACE_XS))
}
