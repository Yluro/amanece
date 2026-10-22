use crate::console::ConsoleEvent;
use crate::remote::RemoteEvent;

/// Everything the UI loop reacts to arrives on one std channel.
#[derive(Debug)]
pub enum AppEvent {
    Input(ratatui::crossterm::event::Event),
    Remote(RemoteEvent),
    Console(ConsoleEvent),
}
