//! Round trip to the game server, from the client's ping and the server's answer (as A2Tools measures it).
//!
//! The client sends a ping request every ten seconds; its body is encrypted, but nothing else it sends has the
//! same size. The server answers with frame `03 36 00 00`, which echoes the client's timestamp. That timestamp is
//! one of three clocks: the newer client's performance counter (ms since boot plus Unreal's 16,777,216 s offset),
//! KR/TW's wall-clock .NET milliseconds, or an arbitrary one. The round trip is read from the counter when this
//! machine's counter is the game's, else timed against the request that caused the answer, else from the
//! wall clock, else from the offset between the two clocks learned from earlier requests.

use std::collections::VecDeque;

use crate::segment::Micros;

/// .NET epoch offset: milliseconds between 0001-01-01 and 1970-01-01.
const DOTNET_EPOCH_OFFSET_MS: i64 = 62_135_596_800_000;
const MAX_PING_MS: i64 = 9999;
/// Opcode and padding of the server's answer, then the echoed i64 timestamp.
const PING_RS: [u8; 4] = [0x03, 0x36, 0x00, 0x00];
const PING_RS_BYTES: usize = 12;
/// Physical size of the client's ping request frame: 12 bytes while the timestamp was wall-clock, 13 since.
const PING_RQ_FRAME_BYTES: std::ops::RangeInclusive<usize> = 12..=13;
/// The same frames as a relay tunnel wraps them: `05 23 <len>` then the frame.
const RELAYED_PING_RQ: [[u8; 4]; 2] = [[0x05, 0x23, 0x0C, 0x0F], [0x05, 0x23, 0x0D, 0x10]];
/// How far a request's clock offset may move between pings and still be the same clock.
const OFFSET_TOLERANCE_MS: i64 = 15;
/// The newer client stamps its ping with the performance counter in ms since boot, plus Unreal's fixed offset.
const UNREAL_CLOCK_OFFSET_MS: i64 = 16_777_216 * 1000;
/// How far the counter's reading may differ from request timing before it counts as a disagreement.
const PERF_AGREEMENT_MS: i64 = 25;
/// Disagreements in a row before the counter stops being trusted.
const PERF_MISSES_TO_DISTRUST: u8 = 2;

/// Reads the performance counter (ms since boot) and the wall clock (Unix ms) together.
pub type PerfClock = fn() -> (i64, i64);

#[derive(Default)]
pub struct PingTracker {
    /// The machine's performance counter, so the newer client's timestamp can be read directly. None for replays,
    /// whose timestamps belong to the counter as it ran then.
    perf_clock: Option<PerfClock>,
    last: Option<(i64, Micros)>,
    /// Capture times (ms) of recent ping requests, oldest first.
    requests: VecDeque<i64>,
    /// Local capture clock minus the client's ping clock, once two pings agree on it.
    clock_offset: Option<i64>,
    /// Offsets the previous answer's candidate requests implied.
    prev_offsets: Vec<i64>,
    perf_misses: u8,
    /// The counter disagreed too often: it is not the clock the game stamps with, so it is not used again.
    perf_distrusted: bool,
}

impl PingTracker {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_perf_clock(&mut self, clock: Option<PerfClock>) {
        self.perf_clock = clock;
    }

    /// The last round trip (ms) and when it was measured.
    pub fn current(&self) -> Option<(i64, Micros)> {
        self.last
    }

    pub fn reset(&mut self) {
        let clock = self.perf_clock;
        *self = Self::default();
        self.perf_clock = clock;
    }

    /// A payload the client sent.
    pub fn on_client_payload(&mut self, payload: &[u8], at: Micros) {
        if !has_ping_rq(payload) {
            return;
        }
        let at_ms = at / 1000;
        self.requests.push_back(at_ms);
        while self.requests.front().is_some_and(|t| at_ms - t > MAX_PING_MS) {
            self.requests.pop_front();
        }
    }

    /// A frame (opcode first) the server sent.
    pub fn on_server_frame(&mut self, frame: &[u8], at: Micros) {
        if frame.len() < PING_RS_BYTES || frame[..4] != PING_RS {
            return;
        }
        let client_sent = i64::from_le_bytes(frame[4..12].try_into().unwrap());
        let arrival_ms = at / 1000;

        // Neither echoed clock follows the wall clock live, so a time sync moves our clock and not the game's: a
        // seen request wins over the wall-clock reading, and the counter is checked against it.
        let wall_clock_rtt = arrival_ms.wrapping_sub(client_sent.wrapping_sub(DOTNET_EPOCH_OFFSET_MS));
        let mut perf_rtt = if self.perf_distrusted { None } else { self.rtt_from_perf_clock(client_sent, arrival_ms) };
        let paired_rtt = self.rtt_from_request(client_sent, arrival_ms);
        if let (Some(perf), Some(paired)) = (perf_rtt, paired_rtt) {
            if (perf - paired).abs() > PERF_AGREEMENT_MS {
                perf_rtt = None;
                self.perf_misses += 1;
                if self.perf_misses >= PERF_MISSES_TO_DISTRUST {
                    self.perf_distrusted = true;
                }
            } else {
                self.perf_misses = 0;
            }
        }
        let rtt = perf_rtt
            .or(paired_rtt)
            .or_else(|| is_valid_rtt(wall_clock_rtt).then_some(wall_clock_rtt))
            .or_else(|| self.rtt_from_known_offset(client_sent, arrival_ms));
        if let Some(rtt) = rtt {
            self.last = Some((rtt, at));
        }
    }

    fn rtt_from_perf_clock(&self, client_sent: i64, arrival_ms: i64) -> Option<i64> {
        let (counter_ms, wall_ms) = (self.perf_clock?)();
        let sent_counter_ms = client_sent.checked_sub(UNREAL_CLOCK_OFFSET_MS)?;
        if !(0..=counter_ms).contains(&sent_counter_ms) {
            return None;
        }
        let rtt = arrival_ms - (sent_counter_ms + (wall_ms - counter_ms));
        is_valid_rtt(rtt).then_some(rtt)
    }

    /// Round trip timed against the request that caused the answer. Each recent request implies an offset between
    /// the echoed clock and ours; one request is the answer outright, with several the one agreeing with the known
    /// offset (or the previous ping) is. The offset is stored once two consecutive pings agree on it.
    fn rtt_from_request(&mut self, client_sent: i64, arrival_ms: i64) -> Option<i64> {
        let offsets: Vec<i64> = self
            .requests
            .iter()
            .filter(|&&t| (0..=MAX_PING_MS).contains(&(arrival_ms - t)))
            .map(|&t| t.wrapping_sub(client_sent))
            .collect();
        let near = |o: i64, others: &[i64]| others.iter().copied().find(|x| x.wrapping_sub(o).wrapping_abs() <= OFFSET_TOLERANCE_MS);

        let picked = match offsets.len() {
            0 => None,
            1 => Some(offsets[0]),
            _ => self
                .clock_offset
                .and_then(|known| near(known, &offsets))
                .or_else(|| offsets.iter().copied().find(|&o| near(o, &self.prev_offsets).is_some())),
        };

        if let Some(offset) = picked {
            let agrees = self.clock_offset.is_some_and(|known| offset.wrapping_sub(known).wrapping_abs() <= OFFSET_TOLERANCE_MS);
            if agrees || near(offset, &self.prev_offsets).is_some() {
                self.clock_offset = Some(offset);
            } else {
                // One of the clocks jumped: forget the offset until two pings agree on the new one.
                self.clock_offset = None;
            }
        }
        self.prev_offsets = offsets;
        picked.map(|o| arrival_ms.wrapping_sub(client_sent.wrapping_add(o))).filter(|&r| is_valid_rtt(r))
    }

    fn rtt_from_known_offset(&self, client_sent: i64, arrival_ms: i64) -> Option<i64> {
        self.clock_offset.map(|o| arrival_ms.wrapping_sub(client_sent.wrapping_add(o))).filter(|&r| is_valid_rtt(r))
    }
}

fn is_valid_rtt(rtt: i64) -> bool {
    (1..=MAX_PING_MS).contains(&rtt)
}

fn read_varint(data: &[u8], pos: usize) -> Option<(i64, usize)> {
    let mut value: i64 = 0;
    for i in 0..5 {
        let b = *data.get(pos + i)?;
        value |= ((b & 0x7f) as i64) << (7 * i);
        if b & 0x80 == 0 {
            return Some((value, i + 1));
        }
    }
    None
}

/// Whether a client payload carries a ping request. The client's stream is encrypted, so a payload only counts if
/// it frames cleanly from its first byte to its last.
fn has_ping_rq(data: &[u8]) -> bool {
    if data.windows(4).any(|w| RELAYED_PING_RQ.iter().any(|rq| w == rq)) {
        return true;
    }
    let mut offset = 0;
    let mut found = false;
    while offset < data.len() {
        let Some((value, _)) = read_varint(data, offset) else { return false };
        let size = value - 3;
        if size <= 0 || offset + size as usize > data.len() {
            return false;
        }
        found |= PING_RQ_FRAME_BYTES.contains(&(size as usize));
        offset += size as usize;
    }
    found
}

/// The clock the game stamps pings with, as Wine provides it: QueryPerformanceCounter is CLOCK_MONOTONIC_RAW. If
/// that is wrong, the tracker notices it disagreeing with request timing and stops using it.
#[cfg(target_os = "linux")]
pub fn perf_clock() -> Option<PerfClock> {
    #[repr(C)]
    struct Timespec {
        tv_sec: i64,
        tv_nsec: i64,
    }
    extern "C" {
        fn clock_gettime(clock_id: i32, tp: *mut Timespec) -> i32;
    }
    const CLOCK_MONOTONIC_RAW: i32 = 4;
    fn read() -> (i64, i64) {
        let mut ts = Timespec { tv_sec: 0, tv_nsec: 0 };
        // SAFETY: writes one timespec through a valid pointer.
        unsafe { clock_gettime(CLOCK_MONOTONIC_RAW, &mut ts) };
        (ts.tv_sec * 1000 + ts.tv_nsec / 1_000_000, crate::segment::now_micros() / 1000)
    }
    Some(read)
}

#[cfg(not(target_os = "linux"))]
pub fn perf_clock() -> Option<PerfClock> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_790_000_000_000;

    fn request(t: &mut PingTracker, at_ms: i64) {
        let data = [vec![0x10], vec![0xAB; 12]].concat();
        t.on_client_payload(&data, at_ms * 1000);
    }

    fn response(t: &mut PingTracker, at_ms: i64, client_sent: i64) {
        let mut frame = PING_RS.to_vec();
        frame.extend_from_slice(&client_sent.to_le_bytes());
        frame.extend_from_slice(&at_ms.to_le_bytes());
        t.on_server_frame(&frame, at_ms * 1000);
    }

    fn ping(t: &PingTracker) -> Option<i64> {
        t.current().map(|(ms, _)| ms)
    }

    #[test]
    fn wall_clock_timestamp_needs_no_request() {
        let mut t = PingTracker::new();
        response(&mut t, NOW + 80, NOW + DOTNET_EPOCH_OFFSET_MS);
        assert_eq!(ping(&t), Some(80));
    }

    #[test]
    fn arbitrary_epoch_is_timed_against_the_request() {
        let mut t = PingTracker::new();
        let clock = 17_552_452_660;
        request(&mut t, NOW);
        response(&mut t, NOW + 64, clock);
        assert_eq!(ping(&t), Some(64));
        request(&mut t, NOW + 10_000);
        response(&mut t, NOW + 10_090, clock + 10_000);
        assert_eq!(ping(&t), Some(90));
        // With the offset learned, an answer whose request was never seen still resolves.
        response(&mut t, NOW + 20_045, clock + 20_000);
        assert_eq!(ping(&t), Some(45));
    }

    #[test]
    fn arbitrary_epoch_without_a_request_reports_nothing() {
        let mut t = PingTracker::new();
        response(&mut t, NOW + 64, 17_552_452_660);
        assert_eq!(ping(&t), None);
    }

    #[test]
    fn requests_are_only_found_in_cleanly_framed_payloads() {
        let rq = [vec![0x10], vec![0xAB; 12]].concat();
        let beat = [vec![0x0E], vec![0xCD; 10]].concat();
        assert!(has_ping_rq(&rq));
        assert!(has_ping_rq(&[beat.clone(), rq].concat()));
        assert!(!has_ping_rq(&beat));
        let mut bulk = vec![0x7F; 4096];
        bulk[0] = 0xFF;
        bulk[100] = 0x10;
        assert!(!has_ping_rq(&bulk));
    }

    fn fake_perf_clock() -> (i64, i64) {
        (1_000_000, NOW)
    }

    #[test]
    fn perf_counter_timestamp_needs_no_request() {
        let mut t = PingTracker::new();
        t.set_perf_clock(Some(fake_perf_clock));
        response(&mut t, NOW - 10_000 + 64, 990_000 + UNREAL_CLOCK_OFFSET_MS);
        assert_eq!(ping(&t), Some(64));
    }

    #[test]
    fn a_counter_that_disagrees_with_requests_is_dropped() {
        let mut t = PingTracker::new();
        t.set_perf_clock(Some(fake_perf_clock));
        let base = NOW - 10_000;
        for n in 0..3 {
            let sent_wall = base + n * 10_000;
            request(&mut t, sent_wall);
            response(&mut t, sent_wall + 70, 990_000 + n * 10_000 - 500 + UNREAL_CLOCK_OFFSET_MS);
            assert_eq!(ping(&t), Some(70));
        }
        assert!(t.perf_distrusted);
    }
}
