//! Handle shortcuts in the widget tree, without subscribing to the global
//! window-event bus (which also carries high-frequency scrolling events).

use iced::advanced::{
    Clipboard, Layout, Shell, Widget, layout, mouse, overlay, renderer,
    widget::{Operation, Tree},
};
use iced::{Element, Event, Length, Rectangle, Size, Vector, keyboard};

pub fn space<'a, Message: Clone + 'a, Theme: 'a, Renderer: renderer::Renderer + 'a>(
    content: impl Into<Element<'a, Message, Theme, Renderer>>,
    on_space: Message,
    on_escape: Option<Message>,
) -> Element<'a, Message, Theme, Renderer> {
    Element::new(Shortcut {
        content: content.into(),
        on_space,
        on_escape,
    })
}

struct Shortcut<'a, Message, Theme, Renderer> {
    content: Element<'a, Message, Theme, Renderer>,
    on_space: Message,
    on_escape: Option<Message>,
}

fn is_space_shortcut(event: &Event, captured: bool) -> bool {
    !captured
        && matches!(event, Event::Keyboard(keyboard::Event::KeyPressed {
        key, modifiers, repeat: false, ..
    }) if modifiers.is_empty() && matches!(key.as_ref(),
        keyboard::Key::Named(keyboard::key::Named::Space) | keyboard::Key::Character(" ")))
}

fn is_escape_shortcut(event: &Event) -> bool {
    matches!(event, Event::Keyboard(keyboard::Event::KeyPressed {
        key: keyboard::Key::Named(keyboard::key::Named::Escape),
        modifiers, repeat: false, ..
    }) if modifiers.is_empty())
}

impl<Message: Clone, Theme, Renderer: renderer::Renderer> Widget<Message, Theme, Renderer>
    for Shortcut<'_, Message, Theme, Renderer>
{
    fn size(&self) -> Size<Length> {
        self.content.as_widget().size()
    }

    fn children(&self) -> Vec<Tree> {
        vec![Tree::new(self.content.as_widget())]
    }

    fn diff(&self, tree: &mut Tree) {
        tree.diff_children(std::slice::from_ref(&self.content));
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        self.content
            .as_widget_mut()
            .layout(&mut tree.children[0], renderer, limits)
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
        operation: &mut dyn Operation,
    ) {
        self.content
            .as_widget_mut()
            .operate(&mut tree.children[0], layout, renderer, operation);
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        viewport: &Rectangle,
    ) {
        // Escape cancels even when a text field has focus. Process it before
        // the child can consume it to unfocus the input; Space remains child-first.
        if is_escape_shortcut(event) {
            if let Some(message) = &self.on_escape {
                shell.publish(message.clone());
                shell.capture_event();
                return;
            }
        }
        self.content.as_widget_mut().update(
            &mut tree.children[0],
            event,
            layout,
            cursor,
            renderer,
            clipboard,
            shell,
            viewport,
        );
        // A focused text input consumes Space first. Repeat and modified keys
        // must not repeatedly toggle the microphone or interfere with typing.
        if is_space_shortcut(event, shell.is_event_captured()) {
            shell.publish(self.on_space.clone());
            shell.capture_event();
        }
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut Renderer,
        theme: &Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        self.content.as_widget().draw(
            &tree.children[0],
            renderer,
            theme,
            style,
            layout,
            cursor,
            viewport,
        );
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &Renderer,
    ) -> mouse::Interaction {
        self.content.as_widget().mouse_interaction(
            &tree.children[0],
            layout,
            cursor,
            viewport,
            renderer,
        )
    }

    fn overlay<'a>(
        &'a mut self,
        tree: &'a mut Tree,
        layout: Layout<'a>,
        renderer: &Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'a, Message, Theme, Renderer>> {
        self.content.as_widget_mut().overlay(
            &mut tree.children[0],
            layout,
            renderer,
            viewport,
            translation,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn press(repeat: bool, modifiers: keyboard::Modifiers) -> Event {
        Event::Keyboard(keyboard::Event::KeyPressed {
            key: keyboard::Key::Named(keyboard::key::Named::Space),
            modified_key: keyboard::Key::Named(keyboard::key::Named::Space),
            physical_key: keyboard::key::Physical::Code(keyboard::key::Code::Space),
            location: keyboard::Location::Standard,
            modifiers,
            text: Some(" ".into()),
            repeat,
        })
    }

    #[test]
    fn space_respects_typing_modifiers_and_repeat() {
        let space = press(false, keyboard::Modifiers::empty());
        assert!(is_space_shortcut(&space, false));
        assert!(!is_space_shortcut(&space, true));
        assert!(!is_space_shortcut(
            &press(true, keyboard::Modifiers::empty()),
            false
        ));
        assert!(!is_space_shortcut(
            &press(false, keyboard::Modifiers::CTRL),
            false
        ));
        assert!(!is_space_shortcut(
            &press(false, keyboard::Modifiers::SHIFT),
            false
        ));
        assert!(!is_space_shortcut(
            &Event::Mouse(mouse::Event::WheelScrolled {
                delta: mouse::ScrollDelta::Lines { x: 0.0, y: 1.0 },
            }),
            false
        ));
    }

    #[test]
    #[cfg(debug_assertions)]
    fn focused_text_input_inserts_a_space_instead_of_toggling() {
        #[derive(Debug, Clone, PartialEq)]
        enum Message {
            Input(String),
            Toggle,
            Cancel,
        }

        let input = iced::widget::TextInput::<Message, iced::Theme, ()>::new("Command", "")
            .on_input(Message::Input);
        let mut element = space(input, Message::Toggle, Some(Message::Cancel));
        let widget = element.as_widget_mut();
        let mut tree = Tree::new(&*widget);
        let layout = widget.layout(
            &mut tree,
            &(),
            &layout::Limits::new(Size::ZERO, Size::new(400.0, 100.0)),
        );
        tree.children[0]
            .state
            .downcast_mut::<iced::widget::text_input::State<()>>()
            .focus();
        let mut messages = Vec::new();
        widget.update(
            &mut tree,
            &press(false, keyboard::Modifiers::empty()),
            Layout::new(&layout),
            mouse::Cursor::Unavailable,
            &(),
            &mut iced::advanced::clipboard::Null,
            &mut Shell::new(&mut messages),
            &Rectangle::with_size(Size::new(400.0, 100.0)),
        );
        assert_eq!(messages, vec![Message::Input(" ".into())]);

        messages.clear();
        let escape = Event::Keyboard(keyboard::Event::KeyPressed {
            key: keyboard::Key::Named(keyboard::key::Named::Escape),
            modified_key: keyboard::Key::Named(keyboard::key::Named::Escape),
            physical_key: keyboard::key::Physical::Code(keyboard::key::Code::Escape),
            location: keyboard::Location::Standard,
            modifiers: keyboard::Modifiers::empty(),
            text: None,
            repeat: false,
        });
        widget.update(
            &mut tree,
            &escape,
            Layout::new(&layout),
            mouse::Cursor::Unavailable,
            &(),
            &mut iced::advanced::clipboard::Null,
            &mut Shell::new(&mut messages),
            &Rectangle::with_size(Size::new(400.0, 100.0)),
        );
        assert_eq!(
            messages,
            vec![Message::Cancel],
            "Escape must cancel even while typing"
        );
        let mut repeated_escape = escape.clone();
        if let Event::Keyboard(keyboard::Event::KeyPressed { repeat, .. }) = &mut repeated_escape {
            *repeat = true;
        }
        assert!(!is_escape_shortcut(&repeated_escape));

        messages.clear();
        tree.children[0]
            .state
            .downcast_mut::<iced::widget::text_input::State<()>>()
            .unfocus();
        widget.update(
            &mut tree,
            &press(false, keyboard::Modifiers::empty()),
            Layout::new(&layout),
            mouse::Cursor::Unavailable,
            &(),
            &mut iced::advanced::clipboard::Null,
            &mut Shell::new(&mut messages),
            &Rectangle::with_size(Size::new(400.0, 100.0)),
        );
        assert_eq!(messages, vec![Message::Toggle]);
    }
}
