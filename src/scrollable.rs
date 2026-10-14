use cosmic::iced::widget::Id;
use cosmic::iced::widget::scrollable::AbsoluteOffset;

pub(crate) fn scroll_to<Message>(
    id: impl Into<Id>,
    offset: AbsoluteOffset<Option<f32>>,
) -> cosmic::iced::Task<Message> {
    cosmic::iced::widget::operation::scrollable::scroll_to(
        id,
        offset,
        cosmic::iced::widget::operation::Animation::Auto,
    )
}

pub(crate) fn scroll_by<Message>(
    id: impl Into<Id>,
    offset: AbsoluteOffset,
) -> cosmic::iced::Task<Message> {
    cosmic::iced::widget::operation::scrollable::scroll_by(
        id,
        offset,
        cosmic::iced::widget::operation::Animation::Auto,
    )
}
