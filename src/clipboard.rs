//! Cancellable, bounded streams: reject large clipboard data before decoding.
use gtk::{gio, glib, prelude::*};

struct Buffer {
    bytes: Vec<u8>,
    maximum: usize,
}

impl Buffer {
    fn append(&mut self, chunk: &[u8]) -> bool {
        if chunk.len() > self.maximum - self.bytes.len() {
            return false;
        }
        self.bytes.extend_from_slice(chunk);
        true
    }
}

pub fn read_limited(
    stream: gio::InputStream,
    cancel: gio::Cancellable,
    maximum: usize,
    finished: impl FnOnce(Option<Vec<u8>>) + 'static,
) {
    next(
        stream,
        cancel,
        Buffer {
            bytes: vec![],
            maximum,
        },
        Box::new(finished),
    );
}

fn next(
    stream: gio::InputStream,
    cancel: gio::Cancellable,
    mut buffer: Buffer,
    finished: Box<dyn FnOnce(Option<Vec<u8>>)>,
) {
    let count = (buffer.maximum - buffer.bytes.len() + 1).min(64 * 1024);
    let source = stream.clone();
    let cancellable = cancel.clone();
    stream.read_bytes_async(
        count,
        glib::Priority::DEFAULT,
        Some(&cancel),
        move |result| match result {
            Ok(chunk) if chunk.is_empty() => finished(Some(buffer.bytes)),
            Ok(chunk) if buffer.append(chunk.as_ref()) => {
                next(source, cancellable, buffer, finished)
            }
            _ => finished(None),
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::RefCell, rc::Rc};
    #[test]
    fn buffer_never_retains_more_than_the_limit() {
        let mut buffer = Buffer {
            bytes: vec![],
            maximum: 4,
        };
        assert!(buffer.append(b"ab"));
        assert!(buffer.append(b"cd"));
        assert!(!buffer.append(b"e"));
        assert_eq!(buffer.bytes, b"abcd");
    }

    fn transfer(data: &[u8], maximum: usize, cancelled: bool) -> Option<Vec<u8>> {
        let context = glib::MainContext::new();
        context
            .with_thread_default(|| {
                let stream = gio::MemoryInputStream::from_bytes(&glib::Bytes::from(data));
                let cancel = gio::Cancellable::new();
                if cancelled {
                    cancel.cancel();
                }
                let loop_ = glib::MainLoop::new(Some(&context), false);
                let result = Rc::new(RefCell::new(None));
                let output = result.clone();
                let finished_loop = loop_.clone();
                read_limited(stream.upcast(), cancel, maximum, move |bytes| {
                    *output.borrow_mut() = bytes;
                    finished_loop.quit();
                });
                loop_.run();
                result.borrow_mut().take()
            })
            .unwrap()
    }

    #[test]
    fn real_async_stream_enforces_bounds_and_cancellation() {
        assert_eq!(transfer(b"abcd", 4, false), Some(b"abcd".to_vec()));
        assert_eq!(transfer(b"abcde", 4, false), None);
        assert_eq!(transfer(b"abcd", 4, true), None);
        assert_eq!(transfer(b"", 4, false), Some(vec![]));
    }
}
