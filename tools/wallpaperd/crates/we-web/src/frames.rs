//! GPU frame descriptors; the default copy path reserves CEF's pool slot until acknowledged.
use std::{os::fd::OwnedFd, sync::mpsc::SyncSender};

pub const ARGB8888: u32 = 0x34325241;
pub const ABGR8888: u32 = 0x34324241;

pub struct Plane {
    pub stride: u32,
    pub offset: u64,
}

pub struct Info {
    pub sequence: u64,
    pub size: [u32; 2],
    pub visible: [u32; 4],
    pub format: u32,
    pub modifier: u64,
    pub planes: Vec<Plane>,
}

pub struct Frame {
    pub info: Info,
    pub fds: Vec<OwnedFd>,
    pub(crate) ack: Option<Ack>,
}

pub(crate) enum Ack {
    Local(SyncSender<bool>),
    Remote(Box<dyn FnOnce(bool) + Send>),
}

impl Frame {
    pub fn complete(mut self, accepted: bool) {
        if let Some(ack) = self.ack.take() {
            ack.complete(accepted);
        }
    }
}

impl Drop for Frame {
    fn drop(&mut self) {
        // Releasing content, a superseded candidate or an invalid frame must
        // also unblock CEF. Never leave the UI thread waiting for a dead consumer.
        if let Some(ack) = self.ack.take() {
            ack.complete(false);
        }
    }
}

impl Ack {
    fn complete(self, accepted: bool) {
        match self {
            Self::Local(sender) => {
                let _ = sender.send(accepted);
            }
            Self::Remote(send) => send(accepted),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    #[test]
    fn abandoned_frame_releases_the_cef_callback() {
        let (ack, receipt) = mpsc::sync_channel(1);
        let frame = Frame {
            info: Info {
                sequence: 1,
                size: [1, 1],
                visible: [0, 0, 1, 1],
                format: ARGB8888,
                modifier: 0,
                planes: vec![],
            },
            fds: vec![],
            ack: Some(Ack::Local(ack)),
        };
        drop(frame);
        assert!(!receipt.recv().unwrap());
    }

    #[test]
    fn abandoned_remote_frame_nacks_the_host_lease() {
        let (ack, receipt) = mpsc::sync_channel(1);
        let frame = Frame {
            info: Info {
                sequence: 2,
                size: [1, 1],
                visible: [0, 0, 1, 1],
                format: ARGB8888,
                modifier: 0,
                planes: vec![],
            },
            fds: vec![],
            ack: Some(Ack::Remote(Box::new(move |accepted| {
                ack.send(accepted).unwrap()
            }))),
        };
        drop(frame);
        assert!(!receipt.recv().unwrap());
    }
}
