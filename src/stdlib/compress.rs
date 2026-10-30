//! Compression: DEFLATE (RFC 1951) with the gzip (RFC 1952) and zlib
//! (RFC 1950) wrappers, written from scratch — the zero-dependency rule means
//! no `flate2`/`miniz` here.
//!
//! The decoder handles every block type (stored, fixed Huffman, dynamic
//! Huffman), because that is what real `.gz` files use. The encoder emits
//! fixed-Huffman blocks over greedy LZ77 matches: valid, interoperable output
//! that any gzip/zlib implementation reads; it does not chase ratio (dynamic
//! Huffman would buy another ~10-20% and is not implemented).
//!
//! Natives: `deflate`/`inflate` (raw), `gzip_compress`/`gunzip`,
//! `zlib_compress`/`unzlib`, plus the checksums `crc32`/`adler32`.

use crate::value::{new_ref, NativeCtx, Value};

/// Refuse to expand a stream beyond this, so a hostile 1 KB input cannot
/// allocate gigabytes (the classic "zip bomb").
const MAX_OUTPUT: usize = 256 * 1024 * 1024;

const WINDOW: usize = 32768;
const MIN_MATCH: usize = 3;
const MAX_MATCH: usize = 258;
const HASH_BITS: usize = 15;
const HASH_SIZE: usize = 1 << HASH_BITS;
/// How many previous positions to test per hash bucket. 32 keeps the encoder
/// roughly O(n) while still finding the nearby repeats that matter.
const MAX_CHAIN: usize = 32;

// ------------------------------------------------------------------ checksums

fn crc_table() -> &'static [u32; 256] {
    use std::sync::OnceLock;
    static TABLE: OnceLock<[u32; 256]> = OnceLock::new();
    TABLE.get_or_init(|| {
        let mut t = [0u32; 256];
        let mut n = 0;
        while n < 256 {
            let mut c = n as u32;
            let mut k = 0;
            while k < 8 {
                c = if c & 1 != 0 {
                    0xEDB8_8320 ^ (c >> 1)
                } else {
                    c >> 1
                };
                k += 1;
            }
            t[n] = c;
            n += 1;
        }
        t
    })
}

pub fn crc32_bytes(data: &[u8]) -> u32 {
    let t = crc_table();
    let mut c = 0xFFFF_FFFFu32;
    for &b in data {
        c = t[((c ^ b as u32) & 0xFF) as usize] ^ (c >> 8);
    }
    c ^ 0xFFFF_FFFF
}

pub fn adler32_bytes(data: &[u8]) -> u32 {
    let mut a: u32 = 1;
    let mut b: u32 = 0;
    for &byte in data {
        a = (a + byte as u32) % 65521;
        b = (b + a) % 65521;
    }
    (b << 16) | a
}

// ------------------------------------------------------------------ bit reader

struct BitReader<'a> {
    data: &'a [u8],
    pos: usize,
    bit: u32,
}

impl<'a> BitReader<'a> {
    fn new(data: &'a [u8]) -> Self {
        BitReader {
            data,
            pos: 0,
            bit: 0,
        }
    }

    fn bits(&mut self, n: u32) -> Result<u32, String> {
        let mut out = 0u32;
        for i in 0..n {
            if self.pos >= self.data.len() {
                return Err("deflate: unexpected end of stream".to_string());
            }
            let b = (self.data[self.pos] >> self.bit) & 1;
            out |= (b as u32) << i;
            self.bit += 1;
            if self.bit == 8 {
                self.bit = 0;
                self.pos += 1;
            }
        }
        Ok(out)
    }

    fn align(&mut self) {
        if self.bit != 0 {
            self.bit = 0;
            self.pos += 1;
        }
    }

    fn byte_at(&mut self) -> Result<u8, String> {
        if self.pos >= self.data.len() {
            return Err("deflate: unexpected end of stream".to_string());
        }
        let b = self.data[self.pos];
        self.pos += 1;
        Ok(b)
    }

    fn u16_le(&mut self) -> Result<u16, String> {
        let lo = self.byte_at()? as u16;
        let hi = self.byte_at()? as u16;
        Ok(lo | (hi << 8))
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8], String> {
        if self.pos + n > self.data.len() {
            return Err("deflate: unexpected end of stream".to_string());
        }
        let s = &self.data[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }
}

// ----------------------------------------------------------------- bit writer

struct BitWriter {
    out: Vec<u8>,
    acc: u32,
    nbits: u32,
}

impl BitWriter {
    fn new() -> Self {
        BitWriter {
            out: Vec::new(),
            acc: 0,
            nbits: 0,
        }
    }

    /// LSB-first (the bit order DEFLATE uses for extra bits and headers).
    fn bits(&mut self, value: u32, n: u32) {
        for i in 0..n {
            let b = (value >> i) & 1;
            self.acc |= b << self.nbits;
            self.nbits += 1;
            if self.nbits == 8 {
                self.out.push(self.acc as u8);
                self.acc = 0;
                self.nbits = 0;
            }
        }
    }

    /// MSB-first: Huffman codes are packed starting from their most
    /// significant bit, which is the opposite of everything else here.
    fn code(&mut self, value: u32, n: u32) {
        for i in (0..n).rev() {
            let b = (value >> i) & 1;
            self.acc |= b << self.nbits;
            self.nbits += 1;
            if self.nbits == 8 {
                self.out.push(self.acc as u8);
                self.acc = 0;
                self.nbits = 0;
            }
        }
    }

    fn finish(mut self) -> Vec<u8> {
        if self.nbits > 0 {
            self.out.push(self.acc as u8);
        }
        self.out
    }
}

// ------------------------------------------------------------------- huffman

/// Canonical Huffman decoder (the "puff" formulation): `counts[len]` is how
/// many symbols have that code length, `symbols` is sorted by (length, value).
struct Huffman {
    counts: [u16; 16],
    symbols: Vec<u16>,
}

impl Huffman {
    fn build(lengths: &[u8]) -> Result<Huffman, String> {
        let mut counts = [0u16; 16];
        for &l in lengths {
            if l as usize > 15 {
                return Err("deflate: code length exceeds 15".to_string());
            }
            counts[l as usize] += 1;
        }
        counts[0] = 0;
        let mut offs = [0u16; 16];
        for i in 1..16 {
            offs[i] = offs[i - 1] + counts[i - 1];
        }
        let mut symbols = vec![0u16; lengths.len()];
        for (sym, &l) in lengths.iter().enumerate() {
            if l != 0 {
                symbols[offs[l as usize] as usize] = sym as u16;
                offs[l as usize] += 1;
            }
        }
        Ok(Huffman { counts, symbols })
    }

    fn decode(&self, br: &mut BitReader) -> Result<u16, String> {
        let mut code: i32 = 0;
        let mut first: i32 = 0;
        let mut index: i32 = 0;
        for len in 1..16 {
            code |= br.bits(1)? as i32;
            let count = self.counts[len] as i32;
            if code - first < count {
                let at = (index + (code - first)) as usize;
                return self
                    .symbols
                    .get(at)
                    .copied()
                    .ok_or_else(|| "deflate: bad huffman code".to_string());
            }
            index += count;
            first = (first + count) << 1;
            code <<= 1;
        }
        Err("deflate: bad huffman code".to_string())
    }
}

const LENGTH_BASE: [u16; 29] = [
    3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131,
    163, 195, 227, 258,
];
const LENGTH_EXTRA: [u32; 29] = [
    0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0,
];
const DIST_BASE: [u16; 30] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537,
    2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577,
];
const DIST_EXTRA: [u32; 30] = [
    0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13,
    13,
];

fn fixed_tables() -> (Huffman, Huffman) {
    let mut lit = [0u8; 288];
    for (i, slot) in lit.iter_mut().enumerate() {
        *slot = match i {
            0..=143 => 8,
            144..=255 => 9,
            256..=279 => 7,
            _ => 8,
        };
    }
    let dist = [5u8; 30];
    (
        Huffman::build(&lit).expect("fixed literal table"),
        Huffman::build(&dist).expect("fixed distance table"),
    )
}

// ------------------------------------------------------------------- inflate

/// Decompress a raw DEFLATE stream (no gzip/zlib header).
pub fn inflate_raw(data: &[u8]) -> Result<Vec<u8>, String> {
    let mut br = BitReader::new(data);
    let mut out: Vec<u8> = Vec::new();
    loop {
        let last = br.bits(1)?;
        let kind = br.bits(2)?;
        match kind {
            0 => {
                br.align();
                let len = br.u16_le()?;
                let nlen = br.u16_le()?;
                if len ^ 0xFFFF != nlen {
                    return Err("deflate: stored block length check failed".to_string());
                }
                let chunk = br.take(len as usize)?;
                if out.len() + chunk.len() > MAX_OUTPUT {
                    return Err("deflate: output exceeds the 256 MB limit".to_string());
                }
                out.extend_from_slice(chunk);
            }
            1 => {
                let (lit, dist) = fixed_tables();
                inflate_block(&mut br, &mut out, &lit, &dist)?;
            }
            2 => {
                let (lit, dist) = dynamic_tables(&mut br)?;
                inflate_block(&mut br, &mut out, &lit, &dist)?;
            }
            _ => return Err("deflate: reserved block type".to_string()),
        }
        if last == 1 {
            return Ok(out);
        }
    }
}

fn dynamic_tables(br: &mut BitReader) -> Result<(Huffman, Huffman), String> {
    const ORDER: [usize; 19] = [
        16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15,
    ];
    let hlit = br.bits(5)? as usize + 257;
    let hdist = br.bits(5)? as usize + 1;
    let hclen = br.bits(4)? as usize + 4;
    if hlit > 286 || hdist > 30 {
        return Err("deflate: too many length/distance codes".to_string());
    }
    let mut cl = [0u8; 19];
    for &slot in ORDER.iter().take(hclen) {
        cl[slot] = br.bits(3)? as u8;
    }
    let cl_huff = Huffman::build(&cl)?;

    let total = hlit + hdist;
    let mut lengths: Vec<u8> = Vec::with_capacity(total);
    while lengths.len() < total {
        let sym = cl_huff.decode(br)?;
        match sym {
            0..=15 => lengths.push(sym as u8),
            16 => {
                let prev = *lengths
                    .last()
                    .ok_or_else(|| "deflate: repeat with no previous length".to_string())?;
                let rep = 3 + br.bits(2)? as usize;
                for _ in 0..rep {
                    lengths.push(prev);
                }
            }
            17 => {
                let rep = 3 + br.bits(3)? as usize;
                lengths.resize(lengths.len() + rep, 0);
            }
            18 => {
                let rep = 11 + br.bits(7)? as usize;
                lengths.resize(lengths.len() + rep, 0);
            }
            _ => return Err("deflate: bad code-length symbol".to_string()),
        }
    }
    if lengths.len() != total {
        return Err("deflate: code lengths overran their table".to_string());
    }
    let lit = Huffman::build(&lengths[..hlit])?;
    let dist = Huffman::build(&lengths[hlit..])?;
    Ok((lit, dist))
}

fn inflate_block(
    br: &mut BitReader,
    out: &mut Vec<u8>,
    lit: &Huffman,
    dist: &Huffman,
) -> Result<(), String> {
    loop {
        let sym = lit.decode(br)?;
        if sym < 256 {
            if out.len() >= MAX_OUTPUT {
                return Err("deflate: output exceeds the 256 MB limit".to_string());
            }
            out.push(sym as u8);
            continue;
        }
        if sym == 256 {
            return Ok(());
        }
        let li = (sym - 257) as usize;
        if li >= LENGTH_BASE.len() {
            return Err("deflate: bad length symbol".to_string());
        }
        let len = LENGTH_BASE[li] as usize + br.bits(LENGTH_EXTRA[li])? as usize;
        let di = dist.decode(br)? as usize;
        if di >= DIST_BASE.len() {
            return Err("deflate: bad distance symbol".to_string());
        }
        let distance = DIST_BASE[di] as usize + br.bits(DIST_EXTRA[di])? as usize;
        if distance > out.len() {
            return Err("deflate: distance points before the start of output".to_string());
        }
        if out.len() + len > MAX_OUTPUT {
            return Err("deflate: output exceeds the 256 MB limit".to_string());
        }
        let start = out.len() - distance;
        for k in 0..len {
            let b = out[start + k];
            out.push(b);
        }
    }
}

// ------------------------------------------------------------------- deflate

#[derive(Clone, Copy)]
enum Token {
    Lit(u8),
    Match { len: usize, dist: usize },
}

fn hash3(data: &[u8], i: usize) -> usize {
    let a = data[i] as usize;
    let b = data[i + 1] as usize;
    let c = data[i + 2] as usize;
    ((a << 10) ^ (b << 5) ^ c) & (HASH_SIZE - 1)
}

fn length_index(len: usize) -> usize {
    let mut i = LENGTH_BASE.len() - 1;
    while i > 0 && LENGTH_BASE[i] as usize > len {
        i -= 1;
    }
    i
}

fn dist_index(d: usize) -> usize {
    let mut i = DIST_BASE.len() - 1;
    while i > 0 && DIST_BASE[i] as usize > d {
        i -= 1;
    }
    i
}

/// Greedy LZ77 over a hash chain: one token per literal or match.
fn lz77(data: &[u8]) -> Vec<Token> {
    let n = data.len();
    let mut tokens: Vec<Token> = Vec::with_capacity(n / 2 + 1);
    let mut head = vec![usize::MAX; HASH_SIZE];
    let mut prev = vec![usize::MAX; n];
    let mut i = 0usize;
    while i < n {
        let mut best_len = 0usize;
        let mut best_dist = 0usize;
        if i + MIN_MATCH <= n {
            let h = hash3(data, i);
            let mut cand = head[h];
            let mut chain = 0;
            let limit = i.saturating_sub(WINDOW);
            while cand != usize::MAX && cand >= limit && chain < MAX_CHAIN {
                let max_len = (n - i).min(MAX_MATCH);
                let mut l = 0usize;
                while l < max_len && data[cand + l] == data[i + l] {
                    l += 1;
                }
                if l > best_len {
                    best_len = l;
                    best_dist = i - cand;
                    if l == max_len {
                        break;
                    }
                }
                cand = prev[cand];
                chain += 1;
            }
        }
        if best_len >= MIN_MATCH {
            tokens.push(Token::Match {
                len: best_len,
                dist: best_dist,
            });
            for k in 0..best_len {
                let at = i + k;
                if at + MIN_MATCH <= n {
                    let h = hash3(data, at);
                    prev[at] = head[h];
                    head[h] = at;
                }
            }
            i += best_len;
        } else {
            tokens.push(Token::Lit(data[i]));
            if i + MIN_MATCH <= n {
                let h = hash3(data, i);
                prev[i] = head[h];
                head[h] = i;
            }
            i += 1;
        }
    }
    tokens
}

fn emit_literal(bw: &mut BitWriter, byte: u8) {
    let sym = byte as u32;
    match sym {
        0..=143 => bw.code(0x30 + sym, 8),
        144..=255 => bw.code(0x190 + (sym - 144), 9),
        _ => unreachable!(),
    }
}

fn emit_match(bw: &mut BitWriter, len: usize, distance: usize) {
    let li = length_index(len);
    let sym = 257 + li as u32;
    match sym {
        257..=279 => bw.code(sym - 256, 7),
        _ => bw.code(0xC0 + (sym - 280), 8),
    }
    if LENGTH_EXTRA[li] > 0 {
        bw.bits((len - LENGTH_BASE[li] as usize) as u32, LENGTH_EXTRA[li]);
    }
    let di = dist_index(distance);
    bw.code(di as u32, 5);
    if DIST_EXTRA[di] > 0 {
        bw.bits((distance - DIST_BASE[di] as usize) as u32, DIST_EXTRA[di]);
    }
}

fn emit_fixed(tokens: &[Token]) -> Vec<u8> {
    let mut bw = BitWriter::new();
    bw.bits(1, 1); // BFINAL
    bw.bits(1, 2); // BTYPE = 01
    for t in tokens {
        match *t {
            Token::Lit(b) => emit_literal(&mut bw, b),
            Token::Match { len, dist } => emit_match(&mut bw, len, dist),
        }
    }
    bw.code(0, 7); // end of block
    bw.finish()
}

/// Huffman code lengths bounded by `max_bits`, with the overflow repair zlib
/// uses: clamp the too-long codes, then rebalance the length histogram until
/// the Kraft sum is valid again, and hand the longest codes to the rarest
/// symbols.
fn huffman_lengths(freqs: &[u32], max_bits: u8) -> Vec<u8> {
    let n = freqs.len();
    let mut lengths = vec![0u8; n];
    let mut used: Vec<usize> = (0..n).filter(|&i| freqs[i] > 0).collect();
    if used.is_empty() {
        return lengths;
    }
    if used.len() == 1 {
        lengths[used[0]] = 1;
        return lengths;
    }
    // Unbounded Huffman lengths by repeated merging of the two smallest.
    // A node is either a leaf (index) or an internal node (weight).
    #[derive(Clone)]
    struct Node {
        left: Option<usize>,
        right: Option<usize>,
        sym: Option<usize>,
    }
    let mut nodes: Vec<Node> = Vec::new();
    let mut heap: Vec<(u64, usize)> = Vec::new();
    for &s in &used {
        nodes.push(Node {
            left: None,
            right: None,
            sym: Some(s),
        });
        heap.push((freqs[s] as u64, nodes.len() - 1));
    }
    heap.sort_by_key(|e| std::cmp::Reverse(e.0));
    while heap.len() > 1 {
        let (wa, ia) = heap.pop().unwrap();
        let (wb, ib) = heap.pop().unwrap();
        nodes.push(Node {
            left: Some(ia),
            right: Some(ib),
            sym: None,
        });
        let id = nodes.len() - 1;
        // Insert keeping the heap sorted descending.
        let pos = heap
            .iter()
            .position(|&(w, _)| w < wa + wb)
            .unwrap_or(heap.len());
        heap.insert(pos, (wa + wb, id));
    }
    let root = heap[0].1;
    // Depth-first walk to assign lengths.
    let mut stack = vec![(root, 0u32)];
    while let Some((id, depth)) = stack.pop() {
        let node = &nodes[id];
        match node.sym {
            Some(s) => lengths[s] = depth.max(1) as u8,
            None => {
                if let Some(l) = node.left {
                    stack.push((l, depth + 1));
                }
                if let Some(r) = node.right {
                    stack.push((r, depth + 1));
                }
            }
        }
    }
    let mut max_len = 0u8;
    for &l in &lengths {
        max_len = max_len.max(l);
    }
    if max_len <= max_bits {
        return lengths;
    }
    // Clamp and repair.
    let mut bl_count = [0u32; 16];
    for &l in &lengths {
        if l > 0 {
            bl_count[l.min(max_bits) as usize] += 1;
        }
    }
    let mut overflow: i32 = 0;
    for &l in &lengths {
        if l > max_bits {
            overflow += 1;
        }
    }
    while overflow > 0 {
        let mut bits = max_bits as usize - 1;
        while bl_count[bits] == 0 {
            bits -= 1;
        }
        bl_count[bits] -= 1;
        bl_count[bits + 1] += 2;
        bl_count[max_bits as usize] -= 1;
        overflow -= 2;
    }
    // Reassign: rarest symbols get the longest codes.
    used.sort_by_key(|&s| freqs[s]);
    let mut idx = 0usize;
    for bits in (1..=max_bits as usize).rev() {
        for _ in 0..bl_count[bits] {
            if idx < used.len() {
                lengths[used[idx]] = bits as u8;
                idx += 1;
            }
        }
    }
    for &s in &used[idx..] {
        lengths[s] = 0;
    }
    lengths
}

/// Canonical code assignment: symbol -> (code, length), codes MSB-first.
fn canonical_codes(lengths: &[u8]) -> Vec<(u32, u8)> {
    let mut bl_count = [0u16; 16];
    for &l in lengths {
        if l > 0 {
            bl_count[l as usize] += 1;
        }
    }
    let mut next_code = [0u32; 16];
    let mut code = 0u32;
    for bits in 1..16 {
        code = (code + bl_count[bits - 1] as u32) << 1;
        next_code[bits] = code;
    }
    let mut out = vec![(0u32, 0u8); lengths.len()];
    for (sym, &l) in lengths.iter().enumerate() {
        if l > 0 {
            out[sym] = (next_code[l as usize], l);
            next_code[l as usize] += 1;
        }
    }
    out
}

const CL_ORDER: [usize; 19] = [
    16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15,
];

/// Run-length encode the code lengths with symbols 16/17/18.
fn rle_lengths(all: &[u8]) -> Vec<(u8, u8, u8)> {
    let mut out: Vec<(u8, u8, u8)> = Vec::new();
    let mut i = 0usize;
    while i < all.len() {
        let v = all[i];
        let mut run = 1usize;
        while i + run < all.len() && all[i + run] == v {
            run += 1;
        }
        if v == 0 {
            let mut left = run;
            while left >= 11 {
                let take = left.min(138);
                out.push((18, 7, (take - 11) as u8));
                left -= take;
            }
            while left >= 3 {
                let take = left.min(10);
                out.push((17, 3, (take - 3) as u8));
                left -= take;
            }
            for _ in 0..left {
                out.push((0, 0, 0));
            }
        } else {
            out.push((v, 0, 0));
            let mut left = run - 1;
            while left >= 3 {
                let take = left.min(6);
                out.push((16, 2, (take - 3) as u8));
                left -= take;
            }
            for _ in 0..left {
                out.push((v, 0, 0));
            }
        }
        i += run;
    }
    out
}

fn emit_dynamic(tokens: &[Token]) -> Vec<u8> {
    let mut lit_freq = [0u32; 286];
    let mut dist_freq = [0u32; 30];
    for t in tokens {
        match *t {
            Token::Lit(b) => lit_freq[b as usize] += 1,
            Token::Match { len, dist } => {
                lit_freq[257 + length_index(len)] += 1;
                dist_freq[dist_index(dist)] += 1;
            }
        }
    }
    lit_freq[256] += 1; // end of block
    let mut hlit = 286usize;
    while hlit > 257 && lit_freq[hlit - 1] == 0 {
        hlit -= 1;
    }
    let mut hdist = 30usize;
    while hdist > 1 && dist_freq[hdist - 1] == 0 {
        hdist -= 1;
    }
    let lit_len = huffman_lengths(&lit_freq[..hlit], 15);
    let mut dist_len = huffman_lengths(&dist_freq[..hdist], 15);
    // A block with no matches still needs a syntactically valid distance tree.
    if dist_len.iter().all(|&l| l == 0) {
        dist_len[0] = 1;
    }
    let mut all = Vec::with_capacity(hlit + hdist);
    all.extend_from_slice(&lit_len);
    all.extend_from_slice(&dist_len);

    let rle = rle_lengths(&all);
    let mut cl_freq = [0u32; 19];
    for &(sym, _, _) in &rle {
        cl_freq[sym as usize] += 1;
    }
    let cl_len = huffman_lengths(&cl_freq, 7);
    let mut hclen = 19usize;
    while hclen > 4 && cl_len[CL_ORDER[hclen - 1]] == 0 {
        hclen -= 1;
    }

    let mut bw = BitWriter::new();
    bw.bits(1, 1); // BFINAL
    bw.bits(2, 2); // BTYPE = 10, dynamic
    bw.bits((hlit - 257) as u32, 5);
    bw.bits((hdist - 1) as u32, 5);
    bw.bits((hclen - 4) as u32, 4);
    for i in 0..hclen {
        bw.bits(cl_len[CL_ORDER[i]] as u32, 3);
    }
    let cl_codes = canonical_codes(&cl_len);
    for &(sym, extra_bits, extra) in &rle {
        let (code, len) = cl_codes[sym as usize];
        bw.code(code, len as u32);
        if extra_bits > 0 {
            bw.bits(extra as u32, extra_bits as u32);
        }
    }
    let lit_codes = canonical_codes(&lit_len);
    let dist_codes = canonical_codes(&dist_len);
    for t in tokens {
        match *t {
            Token::Lit(b) => {
                let (code, len) = lit_codes[b as usize];
                bw.code(code, len as u32);
            }
            Token::Match { len, dist } => {
                let li = length_index(len);
                let (code, clen) = lit_codes[257 + li];
                bw.code(code, clen as u32);
                if LENGTH_EXTRA[li] > 0 {
                    bw.bits((len - LENGTH_BASE[li] as usize) as u32, LENGTH_EXTRA[li]);
                }
                let di = dist_index(dist);
                let (dcode, dlen) = dist_codes[di];
                bw.code(dcode, dlen as u32);
                if DIST_EXTRA[di] > 0 {
                    bw.bits((dist - DIST_BASE[di] as usize) as u32, DIST_EXTRA[di]);
                }
            }
        }
    }
    let (eob_code, eob_len) = lit_codes[256];
    bw.code(eob_code, eob_len as u32);
    bw.finish()
}

/// Compress to a raw DEFLATE stream: one final block, LZ77 tokens, then
/// whichever of fixed or dynamic Huffman is smaller.
pub fn deflate_raw(data: &[u8]) -> Vec<u8> {
    if data.is_empty() {
        // An empty dynamic block costs more than a fixed one; keep it simple.
        return emit_fixed(&[]);
    }
    let tokens = lz77(data);
    let fixed = emit_fixed(&tokens);
    let dynamic = emit_dynamic(&tokens);
    if dynamic.len() < fixed.len() {
        dynamic
    } else {
        fixed
    }
}

// ------------------------------------------------------------- gzip / zlib

pub fn gzip_compress_bytes(data: &[u8]) -> Vec<u8> {
    let mut out = vec![0x1f, 0x8b, 0x08, 0x00, 0, 0, 0, 0, 0x00, 0xff];
    out.extend_from_slice(&deflate_raw(data));
    out.extend_from_slice(&crc32_bytes(data).to_le_bytes());
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out
}

pub fn gunzip_bytes(data: &[u8]) -> Result<Vec<u8>, String> {
    if data.len() < 18 {
        return Err("gunzip: input is too short to be a gzip stream".to_string());
    }
    if data[0] != 0x1f || data[1] != 0x8b {
        return Err("gunzip: not a gzip stream (bad magic)".to_string());
    }
    if data[2] != 0x08 {
        return Err(format!(
            "gunzip: unsupported compression method {}",
            data[2]
        ));
    }
    let flags = data[3];
    let mut pos = 10usize;
    if flags & 0x04 != 0 {
        // FEXTRA
        if pos + 2 > data.len() {
            return Err("gunzip: truncated FEXTRA".to_string());
        }
        let xlen = u16::from_le_bytes([data[pos], data[pos + 1]]) as usize;
        pos += 2 + xlen;
    }
    for mask in [0x08u8, 0x10u8] {
        // FNAME, FCOMMENT: zero-terminated
        if flags & mask != 0 {
            while pos < data.len() && data[pos] != 0 {
                pos += 1;
            }
            pos += 1;
        }
    }
    if flags & 0x02 != 0 {
        pos += 2; // FHCRC
    }
    if pos > data.len() {
        return Err("gunzip: truncated header".to_string());
    }
    let body = &data[pos..];
    let out = inflate_raw(body)?;
    // Trailer: CRC32 + ISIZE, little endian, last 8 bytes.
    if data.len() >= 8 {
        let tail = &data[data.len() - 8..];
        let want_crc = u32::from_le_bytes([tail[0], tail[1], tail[2], tail[3]]);
        let want_len = u32::from_le_bytes([tail[4], tail[5], tail[6], tail[7]]);
        let got_crc = crc32_bytes(&out);
        if got_crc != want_crc {
            return Err(format!(
                "gunzip: CRC mismatch (stored {:08x}, computed {:08x})",
                want_crc, got_crc
            ));
        }
        if (out.len() as u32) != want_len {
            return Err(format!(
                "gunzip: length mismatch (stored {}, got {})",
                want_len,
                out.len()
            ));
        }
    }
    Ok(out)
}

pub fn zlib_compress_bytes(data: &[u8]) -> Vec<u8> {
    let mut out = vec![0x78, 0x9c];
    out.extend_from_slice(&deflate_raw(data));
    out.extend_from_slice(&adler32_bytes(data).to_be_bytes());
    out
}

pub fn unzlib_bytes(data: &[u8]) -> Result<Vec<u8>, String> {
    if data.len() < 6 {
        return Err("unzlib: input is too short to be a zlib stream".to_string());
    }
    let cmf = data[0];
    let flg = data[1];
    if cmf & 0x0f != 8 {
        return Err(format!(
            "unzlib: unsupported compression method {}",
            cmf & 0x0f
        ));
    }
    if !(((cmf as u16) << 8) | flg as u16).is_multiple_of(31) {
        return Err("unzlib: header checksum failed".to_string());
    }
    if flg & 0x20 != 0 {
        return Err("unzlib: preset dictionaries are not supported".to_string());
    }
    let out = inflate_raw(&data[2..])?;
    if data.len() >= 4 {
        let tail = &data[data.len() - 4..];
        let want = u32::from_be_bytes([tail[0], tail[1], tail[2], tail[3]]);
        let got = adler32_bytes(&out);
        if got != want {
            return Err(format!(
                "unzlib: Adler-32 mismatch (stored {:08x}, computed {:08x})",
                want, got
            ));
        }
    }
    Ok(out)
}

// ----------------------------------------------------------------------- zip

/// One archive member. `zip_create` takes a list of maps
/// `{name: string, data: string|bytes}` and returns the archive bytes;
/// `zip_read` reverses it into a name -> bytes map.
pub struct ZipEntry {
    pub name: String,
    pub data: Vec<u8>,
}

fn put_u16(out: &mut Vec<u8>, v: u16) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn put_u32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn rd_u16(data: &[u8], at: usize) -> Result<u16, String> {
    if at + 2 > data.len() {
        return Err("zip: truncated archive".to_string());
    }
    Ok(u16::from_le_bytes([data[at], data[at + 1]]))
}

fn rd_u32(data: &[u8], at: usize) -> Result<u32, String> {
    if at + 4 > data.len() {
        return Err("zip: truncated archive".to_string());
    }
    Ok(u32::from_le_bytes([
        data[at],
        data[at + 1],
        data[at + 2],
        data[at + 3],
    ]))
}

pub fn zip_create(entries: &[ZipEntry]) -> Result<Vec<u8>, String> {
    let mut out: Vec<u8> = Vec::new();
    let mut central: Vec<u8> = Vec::new();
    for e in entries {
        if e.name.is_empty() {
            return Err("zip: entry name must not be empty".to_string());
        }
        if e.name.len() > 0xFFFF {
            return Err(format!("zip: entry name is too long: {}", e.name));
        }
        let crc = crc32_bytes(&e.data);
        let packed = deflate_raw(&e.data);
        // Store when deflating does not help (tiny or incompressible entries).
        let (method, body): (u16, Vec<u8>) = if packed.len() < e.data.len() {
            (8, packed)
        } else {
            (0, e.data.clone())
        };
        let offset = out.len() as u32;
        put_u32(&mut out, 0x0403_4b50);
        put_u16(&mut out, 20); // version needed
        put_u16(&mut out, 0); // flags
        put_u16(&mut out, method);
        put_u16(&mut out, 0); // mod time
        put_u16(&mut out, 0x21); // mod date: 1980-01-01
        put_u32(&mut out, crc);
        put_u32(&mut out, body.len() as u32);
        put_u32(&mut out, e.data.len() as u32);
        put_u16(&mut out, e.name.len() as u16);
        put_u16(&mut out, 0); // extra length
        out.extend_from_slice(e.name.as_bytes());
        out.extend_from_slice(&body);

        put_u32(&mut central, 0x0201_4b50);
        put_u16(&mut central, 20); // version made by
        put_u16(&mut central, 20); // version needed
        put_u16(&mut central, 0); // flags
        put_u16(&mut central, method);
        put_u16(&mut central, 0);
        put_u16(&mut central, 0x21);
        put_u32(&mut central, crc);
        put_u32(&mut central, body.len() as u32);
        put_u32(&mut central, e.data.len() as u32);
        put_u16(&mut central, e.name.len() as u16);
        put_u16(&mut central, 0); // extra
        put_u16(&mut central, 0); // comment
        put_u16(&mut central, 0); // disk
        put_u16(&mut central, 0); // internal attrs
        put_u32(&mut central, 0); // external attrs
        put_u32(&mut central, offset);
        central.extend_from_slice(e.name.as_bytes());
    }
    let cd_offset = out.len() as u32;
    let cd_size = central.len() as u32;
    let count = entries.len() as u16;
    out.extend_from_slice(&central);
    put_u32(&mut out, 0x0605_4b50);
    put_u16(&mut out, 0);
    put_u16(&mut out, 0);
    put_u16(&mut out, count);
    put_u16(&mut out, count);
    put_u32(&mut out, cd_size);
    put_u32(&mut out, cd_offset);
    put_u16(&mut out, 0); // comment length
    Ok(out)
}

pub fn zip_read(data: &[u8]) -> Result<Vec<ZipEntry>, String> {
    if data.len() < 22 {
        return Err("zip: input is too short to be an archive".to_string());
    }
    // The EOCD is at the end, possibly followed by a comment.
    let mut eocd = None;
    let mut i = data.len() - 22;
    loop {
        if data[i..i + 4] == [0x50, 0x4b, 0x05, 0x06] {
            eocd = Some(i);
            break;
        }
        if i == 0 {
            break;
        }
        i -= 1;
    }
    let eocd = eocd.ok_or_else(|| "zip: no end-of-central-directory record".to_string())?;
    let count = rd_u16(data, eocd + 10)? as usize;
    let cd_size = rd_u32(data, eocd + 12)? as usize;
    let cd_off = rd_u32(data, eocd + 16)? as usize;
    if cd_off + cd_size > data.len() {
        return Err("zip: central directory runs past the end of the file".to_string());
    }
    let mut pos = cd_off;
    let mut out = Vec::with_capacity(count);
    for _ in 0..count {
        if rd_u32(data, pos)? != 0x0201_4b50 {
            return Err("zip: bad central-directory signature".to_string());
        }
        let method = rd_u16(data, pos + 10)?;
        let crc = rd_u32(data, pos + 16)?;
        let csize = rd_u32(data, pos + 20)? as usize;
        let usize_ = rd_u32(data, pos + 24)? as usize;
        let nlen = rd_u16(data, pos + 28)? as usize;
        let elen = rd_u16(data, pos + 30)? as usize;
        let clen = rd_u16(data, pos + 32)? as usize;
        let loff = rd_u32(data, pos + 42)? as usize;
        let name_start = pos + 46;
        if name_start + nlen > data.len() {
            return Err("zip: truncated entry name".to_string());
        }
        let name = String::from_utf8_lossy(&data[name_start..name_start + nlen]).to_string();

        // The local header repeats the name and may add an extra field; the
        // payload starts after both.
        let l_nlen = rd_u16(data, loff + 26)? as usize;
        let l_elen = rd_u16(data, loff + 28)? as usize;
        let start = loff + 30 + l_nlen + l_elen;
        if start + csize > data.len() {
            return Err(format!(
                "zip: entry '{}' runs past the end of the file",
                name
            ));
        }
        let body = &data[start..start + csize];
        let content = match method {
            0 => body.to_vec(),
            8 => inflate_raw(body)?,
            other => {
                return Err(format!(
                    "zip: entry '{}' uses unsupported method {}",
                    name, other
                ))
            }
        };
        if content.len() != usize_ {
            return Err(format!(
                "zip: entry '{}' size mismatch (stored {}, got {})",
                name,
                usize_,
                content.len()
            ));
        }
        if crc32_bytes(&content) != crc {
            return Err(format!("zip: entry '{}' failed its CRC check", name));
        }
        out.push(ZipEntry {
            name,
            data: content,
        });
        pos = name_start + nlen + elen + clen;
    }
    Ok(out)
}

// ------------------------------------------------------------------- natives

fn input_bytes(v: &Value, who: &str) -> Result<Vec<u8>, String> {
    match v {
        Value::Str(s) => Ok(s.as_bytes().to_vec()),
        Value::Bytes(b) => Ok(b.lock().unwrap().clone()),
        other => Err(format!(
            "{} expects a string or bytes, found {}",
            who,
            crate::value::type_name(other)
        )),
    }
}

fn ok_bytes(data: Vec<u8>) -> Value {
    Value::Bytes(new_ref(data))
}

fn ok_result(r: Result<Vec<u8>, String>) -> Value {
    match r {
        Ok(d) => Value::Result(Ok(Box::new(Value::Bytes(new_ref(d))))),
        Err(e) => Value::Result(Err(Box::new(Value::Str(e.into())))),
    }
}

fn n_crc32(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(format!("crc32 expects 1 argument, got {}", args.len()));
    }
    let data = input_bytes(&args[0], "crc32")?;
    Ok(Value::Int(crc32_bytes(&data) as i64))
}

fn n_adler32(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(format!("adler32 expects 1 argument, got {}", args.len()));
    }
    let data = input_bytes(&args[0], "adler32")?;
    Ok(Value::Int(adler32_bytes(&data) as i64))
}

fn n_deflate(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(format!("deflate expects 1 argument, got {}", args.len()));
    }
    let data = input_bytes(&args[0], "deflate")?;
    Ok(ok_bytes(deflate_raw(&data)))
}

fn n_inflate(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(format!("inflate expects 1 argument, got {}", args.len()));
    }
    let data = input_bytes(&args[0], "inflate")?;
    Ok(ok_result(inflate_raw(&data)))
}

fn n_gzip_compress(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(format!(
            "gzip_compress expects 1 argument, got {}",
            args.len()
        ));
    }
    let data = input_bytes(&args[0], "gzip_compress")?;
    Ok(ok_bytes(gzip_compress_bytes(&data)))
}

fn n_gunzip(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(format!("gunzip expects 1 argument, got {}", args.len()));
    }
    let data = input_bytes(&args[0], "gunzip")?;
    Ok(ok_result(gunzip_bytes(&data)))
}

fn n_zlib_compress(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(format!(
            "zlib_compress expects 1 argument, got {}",
            args.len()
        ));
    }
    let data = input_bytes(&args[0], "zlib_compress")?;
    Ok(ok_bytes(zlib_compress_bytes(&data)))
}

fn n_unzlib(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(format!("unzlib expects 1 argument, got {}", args.len()));
    }
    let data = input_bytes(&args[0], "unzlib")?;
    Ok(ok_result(unzlib_bytes(&data)))
}

fn map_field(
    m: &std::sync::Arc<std::sync::Mutex<crate::value::collections::seq::MapData>>,
    key: &str,
) -> Option<Value> {
    let guard = m.lock().unwrap();
    for (k, v) in guard.entries.iter() {
        if let crate::value::MapKey::Str(s) = k {
            if &**s == key {
                return Some(v.clone());
            }
        }
    }
    None
}

/// `zip_create([{name, data}, ...])` -> archive bytes.
fn n_zip_create(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(format!("zip_create expects 1 argument, got {}", args.len()));
    }
    let items = match &args[0] {
        Value::List(l) => l.lock().unwrap().items.clone(),
        other => {
            return Err(format!(
                "zip_create expects a list of {{name, data}} maps, found {}",
                crate::value::type_name(other)
            ))
        }
    };
    let mut entries = Vec::with_capacity(items.len());
    for item in items.iter() {
        let m = match item {
            Value::Map(m) => m.clone(),
            other => {
                return Err(format!(
                    "zip_create: each entry must be a map, found {}",
                    crate::value::type_name(other)
                ))
            }
        };
        let name = match map_field(&m, "name") {
            Some(Value::Str(s)) => s.to_string(),
            _ => return Err("zip_create: each entry needs a string 'name'".to_string()),
        };
        let data = match map_field(&m, "data") {
            Some(v) => input_bytes(&v, "zip_create")?,
            None => return Err("zip_create: each entry needs a 'data' value".to_string()),
        };
        entries.push(ZipEntry { name, data });
    }
    Ok(ok_bytes(zip_create(&entries)?))
}

/// `zip_read(archive)` -> ok(map name -> bytes) | err(message).
fn n_zip_read(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(format!("zip_read expects 1 argument, got {}", args.len()));
    }
    let data = input_bytes(&args[0], "zip_read")?;
    match zip_read(&data) {
        Ok(entries) => {
            let pairs: Vec<(crate::value::MapKey, Value)> = entries
                .into_iter()
                .map(|e| {
                    (
                        crate::value::MapKey::Str(e.name.into()),
                        Value::Bytes(new_ref(e.data)),
                    )
                })
                .collect();
            Ok(Value::Result(Ok(Box::new(Value::Map(new_ref(
                crate::value::collections::seq::MapData::from_entries(pairs),
            ))))))
        }
        Err(e) => Ok(Value::Result(Err(Box::new(Value::Str(e.into()))))),
    }
}

/// `zip_list(archive)` -> ok([names]) | err(message).
fn n_zip_list(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(format!("zip_list expects 1 argument, got {}", args.len()));
    }
    let data = input_bytes(&args[0], "zip_list")?;
    match zip_read(&data) {
        Ok(entries) => {
            let items: Vec<Value> = entries
                .into_iter()
                .map(|e| Value::Str(e.name.into()))
                .collect();
            Ok(Value::Result(Ok(Box::new(
                crate::value::collections::seq::make_list(items),
            ))))
        }
        Err(e) => Ok(Value::Result(Err(Box::new(Value::Str(e.into()))))),
    }
}

pub fn native_defs() -> Vec<crate::stdlib::NativeDef> {
    vec![
        ("crc32", n_crc32),
        ("adler32", n_adler32),
        ("deflate", n_deflate),
        ("inflate", n_inflate),
        ("gzip_compress", n_gzip_compress),
        ("gunzip", n_gunzip),
        ("zlib_compress", n_zlib_compress),
        ("unzlib", n_unzlib),
        ("zip_create", n_zip_create),
        ("zip_read", n_zip_read),
        ("zip_list", n_zip_list),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc32_matches_the_known_value() {
        // The canonical CRC-32 of "123456789".
        assert_eq!(crc32_bytes(b"123456789"), 0xCBF4_3926);
    }

    #[test]
    fn adler32_matches_the_known_value() {
        assert_eq!(adler32_bytes(b"Wikipedia"), 0x11E6_0398);
    }

    #[test]
    fn roundtrip_short() {
        for input in [
            &b""[..],
            b"a",
            b"hello hello hello hello",
            b"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        ] {
            let packed = deflate_raw(input);
            assert_eq!(inflate_raw(&packed).unwrap(), input);
        }
    }

    #[test]
    fn roundtrip_repetitive_is_smaller() {
        let input: Vec<u8> = b"the quick brown fox jumps over the lazy dog. "
            .iter()
            .cycle()
            .take(4000)
            .copied()
            .collect();
        let packed = deflate_raw(&input);
        assert!(packed.len() < input.len() / 2, "packed {}", packed.len());
        assert_eq!(inflate_raw(&packed).unwrap(), input);
    }

    #[test]
    fn gzip_roundtrip_and_checksum() {
        let input = b"spid3r compression test payload";
        let packed = gzip_compress_bytes(input);
        assert_eq!(&packed[..2], &[0x1f, 0x8b]);
        assert_eq!(gunzip_bytes(&packed).unwrap(), input);
    }

    #[test]
    fn zlib_roundtrip_and_checksum() {
        let input = b"spid3r zlib payload";
        let packed = zlib_compress_bytes(input);
        assert_eq!(&packed[..2], &[0x78, 0x9c]);
        assert_eq!(unzlib_bytes(&packed).unwrap(), input);
    }

    /// Deterministic pseudo-random payloads across sizes and alphabets: the
    /// dynamic-block path has many edge cases (single symbol, all 256 symbols,
    /// skewed frequencies) that a fixed-Huffman test would never reach.
    #[test]
    fn roundtrip_pseudo_random() {
        let mut state: u64 = 0x2545F4914F6CDD1D;
        let mut next = || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        for size in [0usize, 1, 2, 3, 17, 64, 255, 256, 1000, 5000, 40000] {
            // Alphabet of 2: long runs -> dynamic codes with few symbols.
            let two: Vec<u8> = (0..size)
                .map(|_| if next() % 2 == 0 { b'a' } else { b'b' })
                .collect();
            let packed = deflate_raw(&two);
            assert_eq!(
                inflate_raw(&packed).unwrap(),
                two,
                "binary alphabet, size {}",
                size
            );

            // Full alphabet: every literal and distance code gets exercised.
            let all: Vec<u8> = (0..size).map(|_| (next() & 0xFF) as u8).collect();
            let packed = deflate_raw(&all);
            assert_eq!(
                inflate_raw(&packed).unwrap(),
                all,
                "full alphabet, size {}",
                size
            );
        }
    }

    /// The encoder must pick dynamic blocks when they win (BTYPE = 10) and
    /// still produce a stream the decoder accepts.
    #[test]
    fn skewed_text_uses_a_dynamic_block() {
        let mut text = String::new();
        for i in 0..2000 {
            text.push_str(["alpha", "beta", "gamma", "delta"][i % 4]);
            text.push(' ');
        }
        let packed = deflate_raw(text.as_bytes());
        assert_eq!(packed[0] & 1, 1, "BFINAL");
        assert_eq!((packed[0] >> 1) & 3, 2, "BTYPE should be dynamic");
        assert_eq!(inflate_raw(&packed).unwrap(), text.as_bytes());
    }

    #[test]
    fn corrupt_input_is_rejected_not_panicked() {
        let mut packed = gzip_compress_bytes(b"hello world hello world");
        let last = packed.len() - 5;
        packed[last] ^= 0xFF;
        assert!(gunzip_bytes(&packed).is_err());
    }
}
