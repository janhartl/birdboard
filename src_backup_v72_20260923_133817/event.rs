use anyhow::Result;
use crossterm::event::{Event, poll, read};
use std::time::Duration;

/// Poll often enough to animate/background-refresh the TUI without busy-looping.
pub fn read_event() -> Result<Option<Event>> {
    if poll(Duration::from_millis(80))? {
        Ok(Some(read()?))
    } else {
        Ok(None)
    }
}
