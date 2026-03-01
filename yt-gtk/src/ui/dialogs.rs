use gtk::prelude::*;
use gtk::{
    ApplicationWindow, Dialog, DialogFlags, ResponseType, ScrolledWindow, TextBuffer, TextView,
};

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
    let scrolled = ScrolledWindow::new(gtk::Adjustment::NONE, gtk::Adjustment::NONE);
    scrolled.set_policy(gtk::PolicyType::Automatic, gtk::PolicyType::Automatic);
    scrolled.set_vexpand(true);
    scrolled.set_hexpand(true);

    let text_view = TextView::new();
    text_view.set_editable(false);
    text_view.set_wrap_mode(gtk::WrapMode::Word);
    text_view.set_left_margin(12);
    text_view.set_right_margin(12);
    text_view.set_top_margin(12);
    text_view.set_bottom_margin(12);

    let buffer = TextBuffer::new(None::<&gtk::TextTagTable>);
    buffer.set_text(initial_text);
    text_view.set_buffer(Some(&buffer));

    scrolled.add(&text_view);
    (scrolled, buffer)
}

/// Creates a modal text dialog with a scrollable read-only text view.
///
/// # Arguments
///
/// * `parent` - Parent application window.
/// * `title` - Window title for the dialog.
/// * `initial_text` - Initial text shown in the read-only buffer.
/// * `configure_content` - Hook to insert additional widgets into the content area before the
///   text scroller.
///
/// # Returns
///
/// A tuple containing the created dialog and its backing text buffer.
pub fn create_text_dialog<F>(
    parent: &ApplicationWindow,
    title: &str,
    initial_text: &str,
    configure_content: F,
) -> (Dialog, TextBuffer)
where
    F: FnOnce(&gtk::Box),
{
    let dialog = Dialog::with_buttons(
        Some(title),
        Some(parent),
        DialogFlags::MODAL | DialogFlags::DESTROY_WITH_PARENT,
        &[("Close", ResponseType::Close)],
    );

    dialog.set_default_size(700, 500);

    let content_area = dialog.content_area();
    configure_content(&content_area);

    let (scrolled, buffer) = create_readonly_text_scroller(initial_text);
    content_area.pack_start(&scrolled, true, true, 0);

    dialog.show_all();

    dialog.connect_response(|dialog, _| {
        dialog.close();
    });

    (dialog, buffer)
}

/// Show a scrollable text dialog (for transcripts/summaries)
pub fn show_text_dialog(parent: &ApplicationWindow, title: &str, content: &str) {
    let (dialog, _buffer) = create_text_dialog(parent, title, content, |_| {});

    dialog.run();
}
