use gtk::prelude::*;
use gtk::{
    ApplicationWindow, Dialog, DialogFlags, Label, ResponseType, ScrolledWindow, TextBuffer,
    TextView,
};

/// Show a scrollable text dialog (for transcripts/summaries)
pub fn show_text_dialog(parent: &ApplicationWindow, title: &str, content: &str) {
    let dialog = Dialog::with_buttons(
        Some(title),
        Some(parent),
        DialogFlags::MODAL | DialogFlags::DESTROY_WITH_PARENT,
        &[("Close", ResponseType::Close)],
    );

    dialog.set_default_size(700, 500);

    let content_area = dialog.content_area();

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
    buffer.set_text(content);
    text_view.set_buffer(Some(&buffer));

    scrolled.add(&text_view);
    content_area.pack_start(&scrolled, true, true, 0);

    dialog.show_all();

    dialog.connect_response(|dialog, _| {
        dialog.close();
    });

    dialog.run();
    unsafe {
        dialog.destroy();
    }
}

/// Show a loading dialog that can be updated
#[allow(dead_code)]
pub fn create_loading_dialog(parent: &ApplicationWindow, title: &str) -> (Dialog, Label) {
    let dialog = Dialog::with_buttons(
        Some(title),
        Some(parent),
        DialogFlags::MODAL | DialogFlags::DESTROY_WITH_PARENT,
        &[("Cancel", ResponseType::Cancel)],
    );

    dialog.set_default_size(400, 100);

    let content_area = dialog.content_area();
    content_area.set_margin_start(20);
    content_area.set_margin_end(20);
    content_area.set_margin_top(20);
    content_area.set_margin_bottom(20);

    let label = Label::new(Some("Loading..."));
    label.set_line_wrap(true);
    content_area.pack_start(&label, true, true, 0);

    dialog.show_all();

    (dialog, label)
}

/// Show an error dialog
#[allow(dead_code)]
pub fn show_error_dialog(parent: &ApplicationWindow, title: &str, message: &str) {
    let dialog = gtk::MessageDialog::new(
        Some(parent),
        DialogFlags::MODAL | DialogFlags::DESTROY_WITH_PARENT,
        gtk::MessageType::Error,
        gtk::ButtonsType::Ok,
        message,
    );
    dialog.set_title(title);
    dialog.run();
    unsafe {
        dialog.destroy();
    }
}
