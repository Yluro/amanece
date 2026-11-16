use crate::console::ConsoleEvent;
use crate::remote::RemoteEvent;
use crate::update::UpdateEvent;

/// Everything the UI loop reacts to arrives on one std channel.
#[derive(Debug)]
pub enum AppEvent {
    Input(ratatui::crossterm::event::Event),
    Remote(RemoteEvent),
    Console(ConsoleEvent),
    Update(UpdateEvent),
    /// A CellCheckCSD search finished (`key`: the solve's `Solve::key()`).
    Csd { key: String, id: u64, result: Result<crate::csd::Outcome, String> },}
