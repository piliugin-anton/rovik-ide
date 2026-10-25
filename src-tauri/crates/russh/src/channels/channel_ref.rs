use std::collections::VecDeque;

use tokio::sync::mpsc::Sender;
use tokio::sync::mpsc::error::TrySendError;

use super::{WindowSizeRef, data_len};
use crate::ChannelMsg;

/// A handle to the [`super::Channel`]'s to be able to transmit messages
/// to it and update it's `window_size`.
#[derive(Debug)]
pub struct ChannelRef {
    pub(super) sender: Sender<ChannelMsg>,
    pub(super) window_size: WindowSizeRef,
    /// What the channel's queue had no room for yet, in order (Rovik's
    /// fork, FORK.md: [`ChannelRef::deliver`]).
    pub(super) waiting: VecDeque<ChannelMsg>,
}

impl ChannelRef {
    pub fn new(sender: Sender<ChannelMsg>) -> Self {
        Self {
            sender,
            window_size: WindowSizeRef::new(0),
            waiting: VecDeque::new(),
        }
    }

    pub(crate) fn window_size(&self) -> &WindowSizeRef {
        &self.window_size
    }

    /// Hands `msg` to the channel's reader without waiting for room in its
    /// queue: what doesn't fit waits here, in order, until the reader takes
    /// from the queue ([`ChannelRef::flush`]). So a channel nobody reads
    /// holds up only itself, not the session and every other channel, and
    /// its data stays within the window the session gives as it's read.
    /// The data bytes dropped as the reader is gone.
    pub(crate) fn deliver(&mut self, msg: ChannelMsg) -> u32 {
        self.waiting.push_back(msg);
        self.flush()
    }

    /// Moves what waits here into the channel's queue, as far as it has
    /// room. The data bytes dropped as the reader is gone.
    pub(crate) fn flush(&mut self) -> u32 {
        while let Some(msg) = self.waiting.pop_front() {
            match self.sender.try_send(msg) {
                Ok(()) => {}
                Err(TrySendError::Full(msg)) => {
                    self.waiting.push_front(msg);
                    return 0;
                }
                Err(TrySendError::Closed(msg)) => {
                    let gone = std::iter::once(msg).chain(self.waiting.drain(..));
                    return gone.map(|msg| data_len(&msg)).fold(0, u32::saturating_add);
                }
            }
        }
        0
    }

    /// Gives `msg` to the reader if nothing waits and its queue has room,
    /// else drops it: for what only informs ([`ChannelMsg::WindowAdjusted`]).
    pub(crate) fn offer(&self, msg: ChannelMsg) {
        if self.waiting.is_empty() {
            let _ = self.sender.try_send(msg);
        }
    }

    /// The session lets the channel go: what still waits here goes to the
    /// reader on a task of its own, in order, as it reads.
    pub(crate) fn hand_over(self) {
        if self.waiting.is_empty() {
            return;
        }
        let (sender, waiting) = (self.sender, self.waiting);
        drop(russh_util::runtime::spawn(async move {
            for msg in waiting {
                if sender.send(msg).await.is_err() {
                    return;
                }
            }
        }));
    }
}

impl std::ops::Deref for ChannelRef {
    type Target = Sender<ChannelMsg>;

    fn deref(&self) -> &Self::Target {
        &self.sender
    }
}

#[cfg(test)]
mod tests {
    use bytes::Bytes;
    use tokio::sync::mpsc;

    use super::ChannelRef;
    use crate::ChannelMsg;

    fn data(bytes: &'static [u8]) -> ChannelMsg {
        ChannelMsg::Data {
            data: Bytes::from_static(bytes),
        }
    }

    /// What the reader can take now, data as text and the rest by name.
    fn taken(receiver: &mut mpsc::Receiver<ChannelMsg>) -> Vec<String> {
        std::iter::from_fn(|| receiver.try_recv().ok())
            .map(|msg| match msg {
                ChannelMsg::Data { data } => String::from_utf8_lossy(&data).into_owned(),
                other => format!("{other:?}"),
            })
            .collect()
    }

    // Rovik's fork (FORK.md): what a reader has no room for waits, and
    // holds up nothing else.

    #[test]
    fn what_a_full_queue_has_no_room_for_waits_its_turn_in_order() {
        let (sender, mut receiver) = mpsc::channel(1);
        let mut chan = ChannelRef::new(sender);
        assert_eq!(chan.deliver(data(b"one")), 0);
        assert_eq!(chan.deliver(data(b"two")), 0);
        assert_eq!(chan.deliver(ChannelMsg::Eof), 0);

        assert_eq!(taken(&mut receiver), ["one"]);
        assert_eq!(chan.flush(), 0);
        assert_eq!(taken(&mut receiver), ["two"]);
        assert_eq!(chan.deliver(data(b"three")), 0);
        assert_eq!(taken(&mut receiver), ["Eof"], "what waited goes first");
        assert_eq!(chan.flush(), 0);
        assert_eq!(taken(&mut receiver), ["three"]);
        assert!(chan.waiting.is_empty());
    }

    #[test]
    fn data_for_a_reader_gone_is_dropped_and_counted() {
        let (sender, receiver) = mpsc::channel(1);
        let mut chan = ChannelRef::new(sender);
        chan.deliver(data(b"queued"));
        chan.deliver(data(b"waits"));
        chan.deliver(ChannelMsg::ExtendedData {
            ext: 1,
            data: Bytes::from_static(b"err"),
        });
        chan.deliver(ChannelMsg::Eof);
        drop(receiver);

        assert_eq!(chan.flush(), 8, "the data that waited, 5 and 3 bytes");
        assert!(chan.waiting.is_empty());
        assert_eq!(chan.deliver(data(b"later")), 5);
    }

    #[test]
    fn a_window_notice_never_goes_ahead_of_what_waits() {
        let (sender, mut receiver) = mpsc::channel(1);
        let mut chan = ChannelRef::new(sender);
        chan.deliver(data(b"one"));
        chan.deliver(data(b"two"));
        assert_eq!(taken(&mut receiver), ["one"]);
        chan.offer(ChannelMsg::WindowAdjusted { new_size: 1 });
        assert_eq!(taken(&mut receiver), Vec::<String>::new(), "dropped");

        chan.flush();
        assert_eq!(taken(&mut receiver), ["two"]);
        chan.offer(ChannelMsg::WindowAdjusted { new_size: 2 });
        assert_eq!(taken(&mut receiver), ["WindowAdjusted { new_size: 2 }"]);
    }

    #[tokio::test]
    async fn what_waits_when_the_session_lets_a_channel_go_still_reaches_its_reader() {
        let (sender, mut receiver) = mpsc::channel(1);
        let mut chan = ChannelRef::new(sender);
        chan.deliver(data(b"one"));
        chan.deliver(data(b"two"));
        chan.deliver(ChannelMsg::Close);
        chan.hand_over();

        let mut all = Vec::new();
        while let Some(msg) = receiver.recv().await {
            all.push(match msg {
                ChannelMsg::Data { data } => String::from_utf8_lossy(&data).into_owned(),
                other => format!("{other:?}"),
            });
        }
        assert_eq!(all, ["one", "two", "Close"], "in order, then the end");
    }
}
