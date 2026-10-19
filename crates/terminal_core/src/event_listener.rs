use crate::event::Event;
use async_channel::Sender;

/// Events produced by a terminal grid; the host owns both receiving queues.
#[derive(Clone)]
pub struct ChannelEventListener {
    wakeups: Sender<()>,
    events: Sender<Event>,
}

impl ChannelEventListener {
    pub fn new(wakeups: Sender<()>, events: Sender<Event>) -> Self {
        Self { wakeups, events }
    }

    pub fn send_wakeup_event(&self) {
        if let Err(error) = self.wakeups.try_send(()) {
            log::debug!("Terminal wakeup receiver unavailable: {error}");
        }
    }
    pub fn send_terminal_event(&self, event: Event) {
        if let Err(error) = self.events.try_send(event) {
            log::warn!("Terminal event receiver unavailable: {error}");
        }
    }
}
