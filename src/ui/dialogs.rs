use adw::prelude::*;
use gtk::{ScrolledWindow, TextBuffer};

/// Creates a reusable read-only text scroller used by transcript and summary dialogs.
///
/// # Arguments
///
/// * `initial_text` - Initial text shown in the scroller buffer.
///
/// # Returns
///
/// A tuple containing the scroller widget and its backing [`TextBuffer`].
pub fn create_readonly_text_scroller(initial_text: &str) -> (ScrolledWindow, TextBuffer) {
    let builder = gtk::Builder::from_string(include_str!("dialogs.ui"));
    let scroller = builder
        .object::<ScrolledWindow>("text_scroller")
        .expect("text_scroller in dialogs.ui");
    let text_view = builder
        .object::<gtk::TextView>("text_view")
        .expect("text_view in dialogs.ui");

    let buffer = TextBuffer::new(None);
    buffer.set_text(initial_text);
    text_view.set_buffer(Some(&buffer));

    (scroller, buffer)
}

/// Creates a text dialog with a scrollable read-only text view.
///
/// # Arguments
///
/// * `parent` - Parent application window.
/// * `title` - Header bar title for the dialog.
/// * `initial_text` - Initial text shown in the read-only buffer.
/// * `configure_content` - Hook to insert additional widgets into the content area before the
///   text scroller.
///
/// # Returns
///
/// A tuple containing the presented dialog and its backing text buffer.
pub fn create_text_dialog<F>(
    parent: &adw::ApplicationWindow,
    title: &str,
    initial_text: &str,
    configure_content: F,
) -> (adw::Dialog, TextBuffer)
where
    F: FnOnce(&gtk::Box),
{
    let content_area = gtk::Box::new(gtk::Orientation::Vertical, 0);
    configure_content(&content_area);

    let (scroller, buffer) = create_readonly_text_scroller(initial_text);
    content_area.append(&scroller);

    let toolbar_view = adw::ToolbarView::new();
    toolbar_view.add_top_bar(&adw::HeaderBar::new());
    toolbar_view.set_content(Some(&content_area));

    let dialog = adw::Dialog::builder()
        .title(title)
        .content_width(700)
        .content_height(500)
        .child(&toolbar_view)
        .build();
    dialog.present(Some(parent));

    (dialog, buffer)
}

/// Show a scrollable text dialog (for transcripts/summaries)
pub fn show_text_dialog(parent: &adw::ApplicationWindow, title: &str, content: &str) {
    let _ = create_text_dialog(parent, title, content, |_| {});
}
