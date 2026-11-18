//! Physical bitmap frame allocator & fixed-region bump/free-list kernel heap (`R13.3`).

pub const PAGE_SIZE: usize = 4096;

/// Bitmap physical frame allocator tracking up to `WORDS * 64` 4 KiB frames.
pub struct BitmapFrameAllocator<const WORDS: usize> {
    bitmap: [u64; WORDS],
    total_frames: usize,
    used_frames: usize,
}

impl<const WORDS: usize> BitmapFrameAllocator<WORDS> {
    pub const fn new(total_frames: usize) -> Self {
        let cap = WORDS * 64;
        let clamped = if total_frames > cap { cap } else { total_frames };
        Self {
            bitmap: [0u64; WORDS],
            total_frames: clamped,
            used_frames: 0,
        }
    }

    pub fn alloc_frame(&mut self) -> Option<usize> {
        for word_idx in 0..WORDS {
            let word = self.bitmap[word_idx];
            if word != u64::MAX {
                let bit = word.trailing_ones() as usize;
                let frame = word_idx * 64 + bit;
                if frame >= self.total_frames {
                    return None;
                }
                self.bitmap[word_idx] |= 1u64 << bit;
                self.used_frames += 1;
                return Some(frame * PAGE_SIZE);
            }
        }
        None
    }

    pub fn free_frame(&mut self, phys_addr: usize) -> Result<(), &'static str> {
        if phys_addr % PAGE_SIZE != 0 {
            return Err("frame_free: unaligned physical address");
        }
        let frame = phys_addr / PAGE_SIZE;
        if frame >= self.total_frames {
            return Err("frame_free: frame out of range");
        }
        let word_idx = frame / 64;
        let bit = frame % 64;
        if (self.bitmap[word_idx] & (1u64 << bit)) == 0 {
            return Err("frame_free: double free detected");
        }
        self.bitmap[word_idx] &= !(1u64 << bit);
        self.used_frames -= 1;
        Ok(())
    }

    pub fn used_frames(&self) -> usize {
        self.used_frames
    }

    pub fn total_frames(&self) -> usize {
        self.total_frames
    }
}

/// Fixed-region kernel bump allocator (checked arithmetic, clean error on exhaustion).
pub struct KernelBumpHeap {
    start: usize,
    end: usize,
    next: usize,
}

impl KernelBumpHeap {
    pub const fn new(start: usize, size: usize) -> Self {
        Self {
            start,
            end: start.saturating_add(size),
            next: start,
        }
    }

    pub fn alloc(&mut self, size: usize, align: usize) -> Result<usize, &'static str> {
        if !align.is_power_of_two() {
            return Err("heap: alignment must be power of two");
        }
        let aligned = (self.next.checked_add(align - 1).ok_or("heap: overflow")?) & !(align - 1);
        let new_next = aligned.checked_add(size).ok_or("heap: overflow")?;
        if new_next > self.end {
            return Err("heap: out of memory");
        }
        self.next = new_next;
        Ok(aligned)
    }

    pub fn used_bytes(&self) -> usize {
        self.next.saturating_sub(self.start)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_allocator_prevents_double_free_and_accounts_every_frame() {
        let mut alloc = BitmapFrameAllocator::<2>::new(8);
        let mut addrs = [0usize; 8];
        for slot in &mut addrs {
            *slot = alloc.alloc_frame().expect("frame");
        }
        assert!(alloc.alloc_frame().is_none());
        assert_eq!(alloc.used_frames(), 8);

        alloc.free_frame(addrs[3]).unwrap();
        assert_eq!(alloc.used_frames(), 7);
        assert!(alloc.free_frame(addrs[3]).is_err());
        assert_eq!(alloc.alloc_frame(), Some(addrs[3]));
    }

    #[test]
    fn kernel_heap_exhaustion_is_clean_error_without_wrap() {
        let mut heap = KernelBumpHeap::new(0x10_0000, 128);
        assert_eq!(heap.alloc(64, 16).unwrap(), 0x10_0000);
        assert!(heap.alloc(128, 16).is_err());
    }
}
