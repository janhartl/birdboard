use anyhow::Result;
use crossterm::event::Event;
use crossterm::event::read;

pub fn read_event() -> Result<Event> {
    Ok(read()?)
}
