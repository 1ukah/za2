use std::collections::{BTreeMap, HashMap, HashSet};
use std::net::SocketAddr;

use crate::protocol::FrameLayout;
use crate::segment::TcpSegment;

const MAX_PENDING_BYTES: usize = 256 * 1024;
const MAX_PENDING_SEGMENTS: usize = 64;
const MAX_KNOWN_FRAME_LENGTH: i64 = 1 << 18;
const SYNC_CHAIN_LENGTH: usize = 3;
const SYNC_SEARCH_WINDOW: usize = 4096;
const SYNC_GIVE_UP_BYTES: usize = 8192;
const MAX_STREAMS: usize = 64;

#[derive(Default)]
struct StreamBuffer {
    data: Vec<u8>,
    start: usize,
}

impl StreamBuffer {
    fn bytes(&self) -> &[u8] {
        &self.data[self.start..]
    }

    fn len(&self) -> usize {
        self.data.len() - self.start
    }

    fn extend(&mut self, bytes: &[u8]) {
        if self.start > 0 && self.start >= self.data.len() / 2 {
            self.data.drain(..self.start);
            self.start = 0;
        }
        self.data.extend_from_slice(bytes);
    }

    fn consume(&mut self, n: usize) {
        self.start += n.min(self.len());
        if self.start == self.data.len() {
            self.data.clear();
            self.start = 0;
        }
    }

    fn clear(&mut self) {
        self.data.clear();
        self.start = 0;
    }
}

#[derive(Default)]
struct StreamState {
    next_sequence: u32,
    synced: bool,
    buffer: StreamBuffer,
    pending: BTreeMap<u32, Vec<u8>>,
    pending_bytes: usize,
    last_used: u64,
}

#[derive(Default)]
pub struct TcpReassembler {
    clock: u64,
    streams: HashMap<(SocketAddr, SocketAddr), StreamState>,
    pub gaps: u64,
    pub retransmissions: u64,
    pub frames: u64,
    pub desyncs: u64,
}

fn read_varint(data: &[u8], pos: usize) -> Option<(i64, usize, bool)> {
    let mut length: i64 = 0;
    let mut prefix = 0;
    while prefix < 5 && pos + prefix < data.len() {
        let b = data[pos + prefix];
        length |= ((b & 0x7f) as i64) << (7 * prefix);
        prefix += 1;
        if b & 0x80 == 0 {
            return Some((length, prefix, true));
        }
    }
    if prefix == 0 {
        None
    } else {
        Some((length, prefix, false))
    }
}

impl TcpReassembler {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn reset(&mut self) {
        self.streams.clear();
    }

    pub fn push(&mut self, segment: &TcpSegment, layout: &FrameLayout, sync_opcodes: &HashSet<u32>, on_frame: &mut dyn FnMut(&[u8])) {
        if segment.payload.is_empty() {
            return;
        }

        let key = segment.stream_key();
        if !self.streams.contains_key(&key) && self.streams.len() >= MAX_STREAMS {
            let idle = self.streams.iter().min_by_key(|(_, s)| s.last_used).map(|(k, _)| *k);
            if let Some(k) = idle {
                self.streams.remove(&k);
            }
        }
        self.clock += 1;
        let stream = self.streams.entry(key).or_insert_with(|| StreamState { next_sequence: segment.sequence, ..Default::default() });
        stream.last_used = self.clock;

        let mut counters = Counters::default();
        append(stream, segment.sequence, &segment.payload, &mut counters);
        if layout.is_varint() {
            cut_varint_frames(stream, layout, sync_opcodes, &mut counters, on_frame);
        } else {
            cut_fixed_frames(stream, layout, &mut counters, on_frame);
        }
        self.gaps += counters.gaps;
        self.retransmissions += counters.retransmissions;
        self.frames += counters.frames;
        self.desyncs += counters.desyncs;
    }
}

#[derive(Default)]
struct Counters {
    gaps: u64,
    retransmissions: u64,
    frames: u64,
    desyncs: u64,
}

fn append(stream: &mut StreamState, sequence: u32, payload: &[u8], counters: &mut Counters) {
    let delta = sequence.wrapping_sub(stream.next_sequence) as i32 as i64;
    if delta == 0 {
        stream.buffer.extend(payload);
        stream.next_sequence = sequence.wrapping_add(payload.len() as u32);
        drain_pending(stream);
        return;
    }

    if delta < 0 {
        let overlap = (-delta) as usize;
        counters.retransmissions += 1;
        if overlap >= payload.len() {
            return;
        }
        stream.buffer.extend(&payload[overlap..]);
        stream.next_sequence = sequence.wrapping_add(payload.len() as u32);
        drain_pending(stream);
        return;
    }

    if let Some(previous) = stream.pending.insert(sequence, payload.to_vec()) {
        stream.pending_bytes -= previous.len();
    }
    stream.pending_bytes += payload.len();
    if stream.pending.len() > MAX_PENDING_SEGMENTS || stream.pending_bytes > MAX_PENDING_BYTES {
        counters.gaps += 1;
        stream.buffer.clear();
        stream.synced = false;
        stream.next_sequence = *stream.pending.keys().next().unwrap();
        drain_pending(stream);
    }
}

fn drain_pending(stream: &mut StreamState) {
    while let Some((&first, _)) = stream.pending.iter().next() {
        let delta = first.wrapping_sub(stream.next_sequence) as i32 as i64;
        if delta > 0 {
            return;
        }
        let payload = stream.pending.remove(&first).unwrap();
        stream.pending_bytes -= payload.len();
        let overlap = (-delta) as usize;
        if overlap < payload.len() {
            stream.buffer.extend(&payload[overlap..]);
            stream.next_sequence = first.wrapping_add(payload.len() as u32);
        }
    }
}

fn cut_fixed_frames(stream: &mut StreamState, layout: &FrameLayout, counters: &mut Counters, on_frame: &mut dyn FnMut(&[u8])) {
    while stream.buffer.len() >= layout.header_size {
        let buf = stream.buffer.bytes();
        let declared = read_fixed_length(buf, layout);
        let total = if layout.length_includes_header { declared } else { declared + layout.header_size };
        if total < layout.header_size || total > layout.max_frame_length {
            counters.desyncs += 1;
            stream.buffer.consume(1);
            continue;
        }
        if buf.len() < total {
            break;
        }
        on_frame(&buf[..total]);
        stream.buffer.consume(total);
        counters.frames += 1;
    }
}

fn read_fixed_length(buf: &[u8], layout: &FrameLayout) -> usize {
    let b = &buf[layout.length_offset..layout.length_offset + layout.length_size];
    match layout.length_size {
        1 => b[0] as usize,
        2 => {
            if layout.little_endian {
                u16::from_le_bytes([b[0], b[1]]) as usize
            } else {
                u16::from_be_bytes([b[0], b[1]]) as usize
            }
        }
        _ => {
            if layout.little_endian {
                u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize
            } else {
                u32::from_be_bytes([b[0], b[1], b[2], b[3]]) as usize
            }
        }
    }
}

fn cut_varint_frames(
    stream: &mut StreamState,
    layout: &FrameLayout,
    sync_opcodes: &HashSet<u32>,
    counters: &mut Counters,
    on_frame: &mut dyn FnMut(&[u8]),
) {
    while stream.buffer.len() > 0 {
        if !stream.synced {
            match find_sync_offset(stream.buffer.bytes(), layout, sync_opcodes) {
                None => {
                    if stream.buffer.len() > SYNC_GIVE_UP_BYTES {
                        counters.desyncs += 1;
                        let drop = stream.buffer.len() - SYNC_SEARCH_WINDOW;
                        stream.buffer.consume(drop);
                    }
                    return;
                }
                Some(offset) => {
                    if offset > 0 {
                        counters.desyncs += 1;
                        stream.buffer.consume(offset);
                    }
                    stream.synced = true;
                }
            }
        }

        let buf = stream.buffer.bytes();
        let Some((length, prefix, complete)) = read_varint(buf, 0) else { return };
        if !complete {
            if prefix >= 5 {
                counters.desyncs += 1;
                stream.synced = false;
                stream.buffer.consume(1);
                continue;
            }
            return;
        }

        let total = length + prefix as i64 + layout.length_bias;
        let body_min = layout.body_min() as i64;
        let mut too_big = total > layout.max_frame_length as i64;
        if too_big && total <= MAX_KNOWN_FRAME_LENGTH && !sync_opcodes.is_empty() && total - prefix as i64 >= body_min {
            let opcode_end = prefix + layout.opcode_offset + layout.opcode_size;
            if buf.len() < opcode_end {
                return;
            }
            let opcode = layout.opcode_at(buf, prefix + layout.opcode_offset);
            too_big = !sync_opcodes.contains(&opcode);
        }

        if total - (prefix as i64) < body_min || too_big {
            counters.desyncs += 1;
            stream.synced = false;
            stream.buffer.consume(1);
            continue;
        }

        let total = total as usize;
        if buf.len() < total {
            return;
        }

        on_frame(&buf[prefix..total]);
        stream.buffer.consume(total);
        counters.frames += 1;
    }
}

fn find_sync_offset(buffer: &[u8], layout: &FrameLayout, sync_opcodes: &HashSet<u32>) -> Option<usize> {
    if buffer.is_empty() {
        return None;
    }
    let last = (buffer.len() - 1).min(SYNC_SEARCH_WINDOW);
    (0..=last).find(|&start| chain_fits(buffer, start, layout, sync_opcodes))
}

fn chain_fits(buffer: &[u8], start: usize, layout: &FrameLayout, sync_opcodes: &HashSet<u32>) -> bool {
    let mut pos = start;
    for _ in 0..SYNC_CHAIN_LENGTH {
        let Some((length, prefix, complete)) = read_varint(buffer, pos) else { return false };
        if !complete {
            return false;
        }
        let total = length + prefix as i64 + layout.length_bias;
        if total - (prefix as i64) < layout.body_min() as i64 || total > layout.max_frame_length as i64 || pos as i64 + total > buffer.len() as i64 {
            return false;
        }
        if !sync_opcodes.is_empty() {
            let opcode = layout.opcode_at(buffer, pos + prefix + layout.opcode_offset);
            if !sync_opcodes.contains(&opcode) {
                return false;
            }
        }
        pos += total as usize;
        if pos == buffer.len() {
            return true;
        }
    }
    true
}
