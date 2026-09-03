//! Keyboard and mouse event dispatching for the TUI.

use anyhow::Result;
use crossterm::event::{
    Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use tokio::sync::mpsc::UnboundedSender;

use super::{
    app::{App, Entry, UserCommand},
    editor::{line_end, line_start, next_grapheme_boundary, previous_grapheme_boundary},
    selection::{copy_to_clipboard, selected_text, transcript_point, transcript_point_clamped},
};

pub fn handle_event(app: &mut App, event: Event, tx: &UnboundedSender<UserCommand>) -> Result<()> {
    match event {
        Event::Key(key) if key.kind == KeyEventKind::Press => handle_key(app, key, tx),
        Event::Paste(text) if !app.busy => {
            app.detach_history();
            let text = text.replace("\r\n", "\n");
            app.input.insert_str(app.cursor, &text);
            app.cursor += text.len();
        }
        Event::Mouse(mouse) => handle_mouse(app, mouse),
        _ => {}
    }
    Ok(())
}

pub fn handle_mouse(app: &mut App, mouse: MouseEvent) {
    match mouse.kind {
        MouseEventKind::ScrollUp => app.transcript_scroll.scroll_by(-3),
        MouseEventKind::ScrollDown => app.transcript_scroll.scroll_by(3),
        MouseEventKind::Down(MouseButton::Left) => {
            if let Some(point) = transcript_point(app, mouse.column, mouse.row) {
                app.selection_anchor = Some(point);
                app.selection_focus = Some(point);
                app.selecting = true;
            } else {
                app.selection_anchor = None;
                app.selection_focus = None;
                app.selecting = false;
            }
        }
        MouseEventKind::Drag(MouseButton::Left) if app.selecting => {
            if let Some(point) = transcript_point_clamped(app, mouse.column, mouse.row) {
                app.selection_focus = Some(point);
            }
        }
        MouseEventKind::Up(MouseButton::Left) if app.selecting => {
            if let Some(point) = transcript_point_clamped(app, mouse.column, mouse.row) {
                app.selection_focus = Some(point);
            }
            app.selecting = false;
            if let Some(text) = selected_text(app) {
                let _ = copy_to_clipboard(&text);
            }
        }
        _ => {}
    }
}

pub fn handle_key(app: &mut App, key: KeyEvent, tx: &UnboundedSender<UserCommand>) {
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
        app.should_quit = true;
        return;
    }
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('l') {
        if !app.busy {
            app.mark_transcript_changed();
            app.entries.clear();
            app.streaming.clear();
            app.transcript_scroll.scroll_to_end();
            app.tokens = 0;
            app.entries.push(Entry::Info("已清空上下文".into()));
            let _ = tx.send(UserCommand::Reset);
        }
        return;
    }
    if app.busy {
        match key.code {
            KeyCode::Esc => app.request_cancel(),
            KeyCode::PageUp => app.transcript_scroll.scroll_by(-8),
            KeyCode::PageDown => app.transcript_scroll.scroll_by(8),
            _ => {}
        }
        return;
    }

    match key.code {
        KeyCode::Enter
            if key
                .modifiers
                .intersects(KeyModifiers::SHIFT | KeyModifiers::ALT) =>
        {
            app.insert_text("\n");
        }
        KeyCode::Enter => app.submit(tx),
        KeyCode::Backspace if app.cursor > 0 => {
            app.detach_history();
            let previous = previous_grapheme_boundary(&app.input, app.cursor);
            app.input.drain(previous..app.cursor);
            app.cursor = previous;
        }
        KeyCode::Delete if app.cursor < app.input.len() => {
            app.detach_history();
            let next = next_grapheme_boundary(&app.input, app.cursor);
            app.input.drain(app.cursor..next);
        }
        KeyCode::Left if app.cursor > 0 => {
            app.cursor = previous_grapheme_boundary(&app.input, app.cursor);
        }
        KeyCode::Right if app.cursor < app.input.len() => {
            app.cursor = next_grapheme_boundary(&app.input, app.cursor);
        }
        KeyCode::Home => app.cursor = line_start(&app.input, app.cursor),
        KeyCode::End => app.cursor = line_end(&app.input, app.cursor),
        KeyCode::Up => app.recall(true),
        KeyCode::Down => app.recall(false),
        KeyCode::PageUp => app.transcript_scroll.scroll_by(-8),
        KeyCode::PageDown => app.transcript_scroll.scroll_by(8),
        KeyCode::Tab => app.insert_text("  "),
        KeyCode::Char('d')
            if key.modifiers.contains(KeyModifiers::CONTROL) && app.input.is_empty() =>
        {
            app.should_quit = true;
        }
        KeyCode::Esc if app.login_provider.is_some() => app.cancel_login(),
        KeyCode::Esc if app.input.is_empty() => app.should_quit = true,
        KeyCode::Esc => {
            app.input.clear();
            app.cursor = 0;
            app.detach_history();
        }
        KeyCode::Char(ch)
            if !key
                .modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
        {
            let mut encoded = [0; 4];
            app.insert_text(ch.encode_utf8(&mut encoded));
        }
        _ => {}
    }
}
