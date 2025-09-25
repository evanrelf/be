use std::{
    hash::Hasher,
    ops::Deref,
    pin::{Pin, pin},
    task::{Context, Poll},
};
use tokio::io::{AsyncRead, ReadBuf};
use twox_hash::XxHash3_64;

pub struct Hashing<R, H> {
    reader: R,
    hasher: H,
}

impl<R, H> Hashing<R, H> {
    #[expect(dead_code)]
    pub fn new(reader: R, hasher: H) -> Self {
        Self { reader, hasher }
    }

    pub fn hash(self) -> u64
    where
        H: Hasher,
    {
        self.hasher.finish()
    }
}

impl<R, H> AsyncRead for Hashing<R, H>
where
    R: AsyncRead + Unpin,
    H: Hasher + Unpin,
{
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let Self { reader, hasher } = self.get_mut();
        match pin!(reader).poll_read(cx, buf) {
            Poll::Ready(Ok(())) => {
                hasher.write(buf.filled());
                Poll::Ready(Ok(()))
            }
            poll => poll,
        }
    }
}

impl<R, H> Deref for Hashing<R, H> {
    type Target = R;
    fn deref(&self) -> &Self::Target {
        &self.reader
    }
}

pub trait WithHashingExt: Sized {
    fn with_hashing(self) -> Hashing<Self, XxHash3_64> {
        Hashing {
            reader: self,
            hasher: XxHash3_64::default(),
        }
    }
}

impl<R> WithHashingExt for R where R: AsyncRead + Unpin {}
