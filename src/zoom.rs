use std::num::NonZeroU16;

use crate::config::IconSizes;
use crate::tab::View;

static DEFAULT_ZOOM: NonZeroU16 = NonZeroU16::new(100).unwrap();
pub(crate) static MIN_ZOOM: NonZeroU16 = NonZeroU16::new(50).unwrap();
pub(crate) static MAX_ZOOM: NonZeroU16 = NonZeroU16::new(500).unwrap();
pub(crate) const ZOOM_STEP: u16 = 25;

pub(crate) const fn zoom_to_default(view: View, icon_sizes: &mut IconSizes) {
    let icon_size = select_resized_icon(view, icon_sizes);
    *icon_size = DEFAULT_ZOOM;
}

pub(crate) fn zoom_in_view(view: View, icon_sizes: &mut IconSizes) {
    let icon_size = select_resized_icon(view, icon_sizes);

    let mut step = MIN_ZOOM;
    while step <= MAX_ZOOM {
        if *icon_size < step {
            *icon_size = step;
            break;
        }
        step = step.saturating_add(ZOOM_STEP);
    }
    if *icon_size > step {
        *icon_size = step;
    }
}

pub(crate) fn zoom_out_view(view: View, icon_sizes: &mut IconSizes) {
    let icon_size = select_resized_icon(view, icon_sizes);

    let mut step = MAX_ZOOM;
    while step >= MIN_ZOOM {
        if *icon_size > step {
            *icon_size = step;
            break;
        }
        step = NonZeroU16::new(step.get().saturating_sub(ZOOM_STEP)).unwrap();
    }
    if *icon_size < step {
        *icon_size = step;
    }
}

pub(crate) fn zoom_set(view: View, icon_sizes: &mut IconSizes, zoom: NonZeroU16) {
    let icon_size = select_resized_icon(view, icon_sizes);
    *icon_size = zoom.clamp(MIN_ZOOM, MAX_ZOOM);
}

const fn select_resized_icon(view: View, icon_sizes: &mut IconSizes) -> &mut NonZeroU16 {
    match view {
        View::Grid => &mut icon_sizes.grid,
        View::List => &mut icon_sizes.list,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn zoom(value: u16) -> NonZeroU16 {
        NonZeroU16::new(value).unwrap()
    }

    #[test]
    fn zoom_set_changes_only_the_given_view() {
        let mut icon_sizes = IconSizes::default();

        zoom_set(View::List, &mut icon_sizes, zoom(150));
        assert_eq!(icon_sizes.list, zoom(150));
        assert_eq!(icon_sizes.grid, zoom(100));

        zoom_set(View::Grid, &mut icon_sizes, zoom(250));
        assert_eq!(icon_sizes.list, zoom(150));
        assert_eq!(icon_sizes.grid, zoom(250));
    }

    #[test]
    fn zoom_set_clamps_to_zoom_range() {
        let mut icon_sizes = IconSizes::default();

        zoom_set(View::List, &mut icon_sizes, zoom(1));
        assert_eq!(icon_sizes.list, MIN_ZOOM);

        zoom_set(View::List, &mut icon_sizes, zoom(u16::MAX));
        assert_eq!(icon_sizes.list, MAX_ZOOM);
    }
}
