pub mod read_buf {
    use core::slice::SliceIndex;
    use std::convert::TryInto;
    use std::io::{self, Read};
    use std::ops::Index;

    #[derive(Debug)]
    pub struct Lz4ReadBuf {
        buf: Box<[u8]>,
        n: usize,
        end: usize,
    }

    impl Lz4ReadBuf {
        pub fn with_capacity(cap: usize) -> Self {
            let cap_round = cap.next_power_of_two();
            Lz4ReadBuf {
                buf: vec![0u8; cap_round].into_boxed_slice(),
                n: 0,
                end: 0,
            }
        }

        pub fn capacity(&self) -> usize {
            self.buf.len()
        }

        pub fn extend_read<R: Read>(&mut self, input: &mut R, amt: usize) -> io::Result<()> {
            input.read_exact(&mut self.buf[self.end..self.end + amt])?;
            self.end += amt;
            Ok(())
        }

        pub fn get_u32(&self, index: usize) -> u32 {
            let offset = self.n + index + std::mem::size_of::<u32>();
            assert!(offset <= self.end);

            let (int_bytes, _) = self.buf.split_at(offset);
            u32::from_le_bytes(int_bytes.try_into().unwrap())
        }

        pub fn compact(&mut self) {
            if self.n < self.end {
                self.buf.copy_within(self.n..self.end, 0);
            }
            self.end -= self.n;
            self.n = 0;
        }

        pub fn drop_beg(&mut self, amt: usize) {
            self.n += amt;
        }

        pub fn len(&self) -> usize {
            self.end - self.n
        }

        pub fn pop_byte(&mut self) -> Option<u8> {
            if self.len() == 0 {
                return None;
            }
            let v = self.buf[self.n];
            self.n += 1;
            return Some(v);
        }
    }

    impl<I: SliceIndex<[u8]>> Index<I> for Lz4ReadBuf {
        type Output = I::Output;

        #[inline]
        fn index(&self, index: I) -> &Self::Output {
            Index::index(&self.buf[self.n..], index)
        }
    }
} // mod read_buf

pub mod write_buf {
    use std::cmp;
    use std::io::{self, Write};

    #[derive(Debug)]
    pub struct Lz4WriteBuf<W> {
        inner: W,
        buf: Box<[u8]>,
        end: usize,
    }

    impl<W: Write> Lz4WriteBuf<W> {
        pub fn with_capacity(inner: W, cap: usize) -> Self {
            let cap_round = cap.next_power_of_two();
            Lz4WriteBuf {
                inner: inner,
                buf: vec![0u8; cap_round].into_boxed_slice(),
                end: 0,
            }
        }

        fn copy_non_overlap(&mut self, index: usize, amt: usize) -> io::Result<()> {
            assert!(index < self.buf.len());
            assert!(amt < self.buf.len());

            let n = index + amt;
            if index + amt < self.buf.len() {
                self.inner.write_all(&self.buf[index..n])?;
            } else {
                self.inner.write_all(&self.buf[index..])?;
                let rest_size = n & (self.buf.len() - 1);
                self.inner.write_all(&self.buf[..rest_size])?;
            }
            Ok(())
        }

        pub fn copy_from_offset(&mut self, offset: usize, mut amt: usize) -> io::Result<()> {
            assert!(offset < self.buf.len());

            let mut idx = self.buf.len() - self.end - offset;
            idx &= self.buf.len();

            while amt > offset {
                self.copy_non_overlap(idx, offset)?;
                idx = (idx + offset) & self.buf.len();
                amt -= offset;
            }
            return self.copy_non_overlap(idx, amt);
        }
    }

    impl<W: Write> Write for Lz4WriteBuf<W> {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            let amt = self.inner.write(buf)?;
            if amt > self.buf.len() {
                let n = amt - self.buf.len();
                self.buf.copy_from_slice(&buf[n..]);
                self.end = 0;
                return Ok(amt);
            }

            let n = cmp::min(self.buf.len() - self.end, amt);
            self.buf[self.end..self.end + n].copy_from_slice(&buf[..n]);

            self.buf[..amt - n].copy_from_slice(&buf[n..]);
            self.end += amt;
            return Ok(amt);
        }

        fn flush(&mut self) -> io::Result<()> {
            self.inner.flush()
        }
    }
} // mod write_buf
