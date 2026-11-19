//! Entrances on top of Ely's motion: content that arrives when it is first
//! drawn under a given key, and again whenever the key changes. Lengths go
//! through Ely's `duration`, so reduced motion turns them off.

use std::{fmt::Display, time::Duration};

use ely_gpui_component::motion::{BASE, duration, ease_out_cubic};
use gpui::{Animation, AnimationExt, AnyElement, App, Div, ElementId, IntoElement, Styled, px};

/// How content arrives.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Enter {
    Fade,
    /// Fades in while rising from a little below.
    Rise,
    /// Fades in while sliding in from the left.
    Slide,
}

/// The gap between two staggered entrances.
const STEP: Duration = Duration::from_millis(45);
/// At most this many children wait their turn; later ones arrive with the last.
const MAX_STAGGER: u32 = 8;

fn place(element: Div, how: Enter, t: f32) -> Div {
    let e = ease_out_cubic(t.clamp(0., 1.));
    match how {
        Enter::Fade => element.opacity(e),
        Enter::Rise => element.opacity(e).relative().top(px(8. * (1. - e))),
        Enter::Slide => element.opacity(e).relative().left(px(-12. * (1. - e))),
    }
}

/// `element`, arriving when first drawn as `id` with `key`.
pub fn enter(element: Div, id: &str, key: impl Display, how: Enter, cx: &App) -> AnyElement {
    enter_nth(element, id, key, how, 0, cx)
}

/// The `nth` of a run of children: it arrives a little after the one before.
pub fn enter_nth(
    element: Div,
    id: &str,
    key: impl Display,
    how: Enter,
    nth: usize,
    cx: &App,
) -> AnyElement {
    let length = duration(BASE + Duration::from_millis(60), cx);
    if length < Duration::from_millis(5) {
        return element.into_any_element();
    }
    let lead = STEP * (nth as u32).min(MAX_STAGGER);
    let total = lead + length;
    let (lead, length_s) = (lead.as_secs_f32(), length.as_secs_f32());
    element
        .with_animation(
            ElementId::Name(format!("{id}:{key}:{nth}").into()),
            Animation::new(total),
            move |element, t| {
                let at = (t * total.as_secs_f32() - lead) / length_s;
                place(element, how, at)
            },
        )
        .into_any_element()
}
