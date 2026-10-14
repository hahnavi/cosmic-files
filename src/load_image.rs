use cosmic::iced::{core as iced_core, widget as iced_widget};
use iced_core::event::Event;
use iced_core::widget::{Operation, Tree};
use iced_core::{
    Element, Layout, Length, Rectangle, Shell, Size, Vector, Widget, layout, mouse, overlay,
    renderer,
};

pub fn loaded_image<'a, Message: 'static, Theme>(
    handle: <cosmic::Renderer as iced_core::image::Renderer>::Handle,
) -> LoadedImage<'a, Message, Theme, cosmic::Renderer>
where
    Theme: iced_widget::container::Catalog,
    <Theme as iced_widget::container::Catalog>::Class<'a>: From<cosmic::theme::Container<'a>>,
{
    LoadedImage::new(handle)
}

/// Forces the wrapped image to be loaded before drawing.
///
/// May cause a dropped frame if the image is not already in the cache.
/// This is useful when you want to ensure an image is loaded before it is drawn, for example when swapping out a placeholder.
/// Otherwise, the image may be blank until the next redraw.
#[allow(missing_debug_implementations)]
pub struct LoadedImage<'a, Message, Theme, Renderer>
where
    Renderer: iced_core::Renderer + iced_core::image::Renderer,
{
    handle: <Renderer as iced_core::image::Renderer>::Handle,
    content: cosmic::iced::Element<'a, Message, Theme, Renderer>,
}

impl<'a, Message, Theme, Renderer> LoadedImage<'a, Message, Theme, Renderer>
where
    Renderer: iced_core::Renderer + iced_core::image::Renderer,
    <Renderer as iced_core::image::Renderer>::Handle: 'a,
{
    /// Creates an empty [`LoadedImage`].
    pub(crate) fn new(handle: <Renderer as iced_core::image::Renderer>::Handle) -> Self {
        LoadedImage {
            handle: handle.clone(),
            content: cosmic::widget::Image::new(handle).into(),
        }
    }
}

impl<Message, Theme, Renderer> Widget<Message, Theme, Renderer>
    for LoadedImage<'_, Message, Theme, Renderer>
where
    Renderer: iced_core::Renderer + iced_core::image::Renderer,
{
    fn children(&self) -> Vec<Tree> {
        vec![Tree::new(&self.content)]
    }

    fn diff(&mut self, tree: &mut Tree) {
        tree.diff_children(std::slice::from_mut(&mut self.content));
    }

    fn size(&self) -> iced_core::Size<Length> {
        self.content.as_widget().size()
    }

    fn layout(&mut self, tree: &mut Tree, renderer: &Renderer, limits: &layout::Limits) {
        self.content
            .as_widget_mut()
            .layout(&mut tree.children[0], renderer, limits);
        tree.size = tree.children[0].size;
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout,
        viewport: &Rectangle,
        renderer: &Renderer,
        operation: &mut dyn Operation,
    ) {
        operation.container(None, layout.bounds(), viewport);
        operation.traverse(&mut |operation| {
            let child_layout = layout.child(&tree.children[0]);
            self.content.as_widget_mut().operate(
                &mut tree.children[0],
                child_layout,
                viewport,
                renderer,
                operation,
            );
        });
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout,
        cursor_position: mouse::Cursor,
        renderer: &Renderer,
        shell: &mut Shell<'_, Message>,
        viewport: &Rectangle,
    ) {
        self.content.as_widget_mut().update(
            &mut tree.children[0],
            event,
            layout
                .children()
                .next()
                .unwrap()
                .with_virtual_offset(layout.virtual_offset()),
            cursor_position,
            renderer,
            shell,
            viewport,
        );
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout,
        cursor_position: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &Renderer,
    ) -> mouse::Interaction {
        let content_layout = layout.child(&tree.children[0]);
        self.content.as_widget().mouse_interaction(
            &tree.children[0],
            content_layout.with_virtual_offset(layout.virtual_offset()),
            cursor_position,
            viewport,
            renderer,
        )
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut Renderer,
        theme: &Theme,
        renderer_style: &renderer::Style,
        layout: Layout,
        cursor_position: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        let content_layout = layout.child(&tree.children[0]);

        // forces image to be loaded before drawing
        _ = renderer.load_image(&self.handle);
        self.content.as_widget().draw(
            &tree.children[0],
            renderer,
            theme,
            renderer_style,
            content_layout.with_virtual_offset(layout.virtual_offset()),
            cursor_position,
            viewport,
        );
    }

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: Layout,
        renderer: &Renderer,
        viewport: &Rectangle,
        translation: Vector,
        window: Size,
    ) -> Vec<overlay::Element<'b, Message, Theme, Renderer>> {
        let child_layout = layout.child(&tree.children[0]);
        self.content.as_widget_mut().overlay(
            &mut tree.children[0],
            child_layout,
            renderer,
            viewport,
            translation,
            window,
        )
    }

    fn drag_destinations(
        &self,
        state: &Tree,
        layout: Layout,
        renderer: &Renderer,
        dnd_rectangles: &mut iced_core::clipboard::DndDestinationRectangles,
    ) {
        let content_layout = layout.child(&state.children[0]);
        self.content.as_widget().drag_destinations(
            &state.children[0],
            content_layout.with_virtual_offset(layout.virtual_offset()),
            renderer,
            dnd_rectangles,
        );
    }
}

impl<'a, Message, Theme, Renderer> From<LoadedImage<'a, Message, Theme, Renderer>>
    for Element<'a, Message, Theme, Renderer>
where
    Message: 'a,
    Renderer: 'a + iced_core::Renderer + iced_core::image::Renderer,
    Theme: 'a,
{
    fn from(c: LoadedImage<'a, Message, Theme, Renderer>) -> Element<'a, Message, Theme, Renderer> {
        Element::new(c)
    }
}
