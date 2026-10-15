//! Keep transcript layout across frames; scrolling paints only visible lines.
use crate::subtitle::{format_srt, SrtEntry};
use gpui::{
    point, relative, App, AvailableSpace, Bounds, Element, ElementId, Font, GlobalElementId, Hsla,
    InspectorElementId, IntoElement, LayoutId, Pixels, SharedString, Size, Style, TextAlign,
    TextRun, Window, WrappedLine,
};
use std::{cell::RefCell, ops::Range, rc::Rc};

#[derive(Clone, Default)]
pub(super) struct Preview(Rc<RefCell<Layout>>);

#[derive(Default)]
struct Layout {
    text: SharedString,
    key: Option<Key>,
    width: Option<Pixels>,
    measured: Vec<(Option<Pixels>, Size<Pixels>)>,
    lines: Vec<(Pixels, WrappedLine)>,
    size: Size<Pixels>,
    builds: usize,
    painted: Range<usize>,
}

#[derive(Clone, PartialEq)]
pub(super) struct Key {
    font: Font,
    size: Pixels,
    line_height: Pixels,
    color: Hsla,
}

impl Layout {
    fn shape(&mut self, key: &Key, width: Option<Pixels>, window: &mut Window) -> Size<Pixels> {
        if self.key.as_ref() == Some(key) && self.width == width {
            return self.size;
        }
        if self.key.as_ref() != Some(key) {
            self.measured.clear();
        }
        let run = TextRun {
            len: self.text.len(),
            font: key.font.clone(),
            color: key.color,
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        self.size = Size::default();
        match window
            .text_system()
            .shape_text(self.text.clone(), key.size, &[run], width, None)
        {
            Ok(lines) => {
                self.lines = lines
                    .into_iter()
                    .map(|line| {
                        let top = self.size.height;
                        let size = line.size(key.line_height);
                        self.size.height += size.height;
                        self.size.width = self.size.width.max(size.width).ceil();
                        (top, line)
                    })
                    .collect();
            }
            Err(error) => {
                log::error!("Cannot lay out subtitle preview: {error}");
                self.lines.clear();
            }
        }
        if let Some(width) = width {
            self.size.width = width;
        }
        self.key = Some(key.clone());
        self.width = width;
        self.builds += 1;
        self.size
    }

    fn measure(&mut self, key: &Key, width: Option<Pixels>, window: &mut Window) -> Size<Pixels> {
        if self.key.as_ref() == Some(key) {
            if let Some((_, size)) = self.measured.iter().find(|(w, _)| *w == width) {
                return *size;
            }
        }
        let size = self.shape(key, width, window);
        // Taffy probes intrinsic and intermediate widths before the final width.
        // Retain only their sizes, keeping one drawable layout in memory.
        if self.measured.len() == 8 {
            self.measured.remove(0);
        }
        self.measured.push((width, size));
        size
    }
}

impl Preview {
    pub fn set_entries(&mut self, entries: &[SrtEntry]) {
        let mut text = format_srt(entries);
        text.truncate(text.trim_end().len());
        *self.0.borrow_mut() = Layout {
            text: text.into(),
            ..Default::default()
        };
    }

    pub fn stats(&self) -> (usize, Range<usize>, usize) {
        let layout = self.0.borrow();
        (layout.builds, layout.painted.clone(), layout.lines.len())
    }
}

impl IntoElement for Preview {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}

impl Element for Preview {
    type RequestLayoutState = Key;
    type PrepaintState = Range<usize>;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        _cx: &mut App,
    ) -> (LayoutId, Key) {
        let text_style = window.text_style();
        let key = Key {
            font: text_style.font(),
            size: text_style.font_size.to_pixels(window.rem_size()),
            line_height: text_style
                .line_height
                .to_pixels(text_style.font_size, window.rem_size()),
            color: text_style.color,
        };
        let measure_key = key.clone();
        let cache = self.0.clone();
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        let id = window.request_measured_layout(style, move |known, available, window, _| {
            let width = known.width.or(match available.width {
                AvailableSpace::Definite(width) => Some(width),
                _ => None,
            });
            cache.borrow_mut().measure(&measure_key, width, window)
        });
        (id, key)
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        key: &mut Key,
        window: &mut Window,
        _: &mut App,
    ) -> Range<usize> {
        let mut layout = self.0.borrow_mut();
        layout.shape(key, Some(bounds.size.width), window);
        let clip = window.content_mask().bounds;
        let line_height = key.line_height;
        let start = layout.lines.partition_point(|(top, line)| {
            bounds.top() + *top + line.size(line_height).height <= clip.top()
        });
        let end = layout
            .lines
            .partition_point(|(top, _)| bounds.top() + *top < clip.bottom());
        start..end.max(start)
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        key: &mut Key,
        visible: &mut Range<usize>,
        window: &mut Window,
        cx: &mut App,
    ) {
        let mut layout = self.0.borrow_mut();
        for (top, line) in &layout.lines[visible.clone()] {
            let top = (*top * window.scale_factor()).round() / window.scale_factor();
            if let Err(error) = line.paint(
                point(bounds.left(), bounds.top() + top),
                key.line_height,
                TextAlign::Left,
                Some(bounds),
                window,
                cx,
            ) {
                log::error!("Cannot paint subtitle preview: {error}");
            }
        }
        layout.painted = visible.clone();
    }
}
