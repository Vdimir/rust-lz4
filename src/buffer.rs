pub mod read_buf {
    use core::slice::SliceIndex;
    use std::convert::TryInto;
    use std::io::{self, Read};
    use std::ops::Index;

    /// Simple buffer to cache read data
    #[derive(Debug)]
    pub struct Lz4ReadBuf {
        buf: Box<[u8]>,
        n: usize,
        end: usize,
    }

    impl Lz4ReadBuf {
        /// Create new buffer
        pub fn with_capacity(cap: usize) -> Self {
            let cap_round = cap.next_power_of_two();
            Lz4ReadBuf {
                buf: vec![0u8; cap_round].into_boxed_slice(),
                n: 0,
                end: 0,
            }
        }

        /// Buffer capacity
        pub fn capacity(&self) -> usize {
            self.buf.len()
        }

        /// Read `amt` bytes from reader
        pub fn extend_read<R: Read>(&mut self, input: &mut R, amt: usize) -> io::Result<()> {
            input.read_exact(&mut self.buf[self.end..self.end + amt])?;
            self.end += amt;
            Ok(())
        }

        /// Get 4 bytes from buffer as u32
        pub fn get_u32(&self, index: usize) -> u32 {
            let offset = self.n + index + std::mem::size_of::<u32>();
            assert!(offset <= self.end);

            let (int_bytes, _) = self.buf.split_at(offset);
            u32::from_le_bytes(int_bytes.try_into().unwrap())
        }

        /// Drop read data and possible move rest to beginnig of buffer
        pub fn compact(&mut self) {
            if self.n < self.end {
                self.buf.copy_within(self.n..self.end, 0);
            }
            self.end -= self.n;
            self.n = 0;
        }

        /// mark `amt` bytes as read and move cursor
        pub fn consume(&mut self, amt: usize) {
            self.n += amt;
        }

        /// Amount of data in buffer in bytes
        pub fn len(&self) -> usize {
            self.end - self.n
        }

        /// Get fisrt byte from buffer and  move cursor
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

    /// Buffer writes data to underlying writer and keep last chunk in internal storage
    #[derive(Debug)]
    pub struct Lz4WriteBuf<W> {
        inner: W,
        buf: Box<[u8]>,
        end: usize,
        total_written: usize,
    }

    impl<W: Write> Lz4WriteBuf<W> {
        /// Create new buffer
        pub fn with_capacity(inner: W, cap: usize) -> Self {
            let cap_round = cap.next_power_of_two();
            Lz4WriteBuf {
                inner: inner,
                buf: vec![0u8; cap_round].into_boxed_slice(),
                end: 0,
                total_written: 0,
            }
        }

        /// Copy `amt` bytes from buffer  with `offset` from end to underlying writer
        pub fn copy_from_offset(&mut self, offset: usize, mut amt: usize) -> io::Result<()> {
            assert!(offset < self.buf.len());

            let mut idx = if self.end < offset {
                self.buf.len() + self.end - offset
            } else {
                self.end - offset
            };

            idx &= self.buf.len() - 1;

            while amt > offset {
                self.copy_non_overlap(idx, offset)?;
                idx = (idx + offset) & (self.buf.len() - 1);
                amt -= offset;
            }
            return self.copy_non_overlap(idx, amt);
        }

        fn copy_non_overlap(&mut self, index: usize, amt: usize) -> io::Result<()> {
            assert!(index < self.buf.len());
            assert!(amt < self.buf.len());

            let n = cmp::min(
                cmp::min(self.buf.len() - self.end, self.buf.len() - index),
                amt,
            );

            self.total_written += n;
            self.inner.write_all(&self.buf[index..index + n])?;
            self.buf.copy_within(index..index + n, self.end);
            self.end = (self.end + n) & (self.buf.len() - 1);

            if n < amt {
                return self.copy_non_overlap((index + n) & (self.buf.len() - 1), amt - n);
            }
            return Ok(());
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

            self.buf[..amt - n].copy_from_slice(&buf[n..amt]);
            self.end = (self.end + amt) & (self.buf.len() - 1);
            self.total_written += amt;
            return Ok(amt);
        }

        fn flush(&mut self) -> io::Result<()> {
            self.inner.flush()
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        struct TestWrite {
            data: Vec<u8>,
            max_write_size: usize,
        }

        impl TestWrite {
            fn new(data: Vec<u8>, max_write_size: usize) -> Self {
                TestWrite {
                    data: data,
                    max_write_size: max_write_size,
                }
            }

            fn completed(&self) -> bool {
                self.data.len() == 0
            }
        }

        impl Write for TestWrite {
            fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
                assert!(buf.len() <= self.data.len());
                let n = cmp::min(buf.len(), self.max_write_size);
                assert_eq!(&buf[..n], &self.data[..n]);
                self.data.drain(..n);
                Ok(n)
            }

            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }

        #[test]
        fn test_write_buf() {
            let data: Vec<u8> = (0..1024).map(|x| (x & 255) as u8).collect();
            let mut tw = TestWrite::new(data.clone(), 100);
            let mut w = Lz4WriteBuf::with_capacity(&mut tw, 512);

            let shift = 2;
            let res = w.write_all(&data[..shift]);
            assert!(res.is_ok());
            assert_eq!(w.end, shift);

            let res = w.write_all(&data[shift..shift + 256]);
            assert!(res.is_ok());
            assert_eq!(w.end, shift + 256);

            let res = w.copy_from_offset(256, 64);
            assert!(res.is_ok());
            assert_eq!(w.end, shift + 256 + 64);

            let res = w.copy_from_offset(256, 128);
            assert!(res.is_ok());
            assert_eq!(w.end, shift + 256 + 64 + 128);

            let res = w.copy_from_offset(256, 64);
            assert!(res.is_ok());
            assert_eq!(w.end, shift);

            let res = w.copy_from_offset(256, 256);
            assert!(res.is_ok());
            assert_eq!(w.end, shift + 256);

            let res = w.copy_from_offset(256, 256 - shift);
            assert!(res.is_ok());
            assert_eq!(w.end, 0);

            assert!(tw.completed());
        }

        #[test]
        fn test_write_buf_overlap() {
            // [1, 1, ..., 1, 2, 2, ..., 2, 3, 3, ..., 3, 4, 4, ..., 4]
            //  |<-  256  ->| |<-  256  ->| |<-  256  ->| |<-  256  ->|
            let data: Vec<u8> = (0..1024).map(|x| (x / 256) as u8).collect();

            let mut tw = TestWrite::new(data.clone(), 100);
            let mut w = Lz4WriteBuf::with_capacity(&mut tw, 128);

            // 0
            let res = w.write_all(&data[..2]);
            assert!(res.is_ok());
            assert_eq!(w.end, 2);

            // 1
            let res = w.copy_from_offset(2, 254);
            assert!(res.is_ok());

            let res = w.write_all(&data[256..256 + 1]);
            assert!(res.is_ok());

            // 2
            let res = w.copy_from_offset(1, 255);
            assert!(res.is_ok());

            // 3
            let res = w.write_all(&data[512..512 + 56]);
            assert!(res.is_ok());

            let res = w.copy_from_offset(56, 200);
            assert!(res.is_ok());

            // 4
            let res = w.write_all(&data[768..768 + 127]);
            assert!(res.is_ok());

            let res = w.copy_from_offset(127, 129);
            assert!(res.is_ok());

            assert!(tw.completed());
        }
    }
} // mod write_buf
