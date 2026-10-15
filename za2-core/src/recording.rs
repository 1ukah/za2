use std::io::{BufRead, BufReader, Write};
use std::net::{IpAddr, SocketAddr};
use std::path::Path;

use base64::Engine;
use chrono::{DateTime, Local, NaiveDateTime, TimeZone};
use serde::{Deserialize, Serialize};

use crate::segment::{Micros, TcpSegment};

#[derive(Serialize, Deserialize)]
struct Line {
    #[serde(rename = "T")]
    t: String,
    #[serde(rename = "From")]
    from: String,
    #[serde(rename = "To")]
    to: String,
    #[serde(rename = "Seq")]
    seq: u32,
    #[serde(rename = "Server")]
    server: bool,
    #[serde(rename = "Data")]
    data: String,
}

pub fn parse_endpoint(text: &str) -> Option<SocketAddr> {
    if let Ok(addr) = text.parse() {
        return Some(addr);
    }
    let (host, port) = text.rsplit_once(':')?;
    let ip: IpAddr = host.trim_matches(['[', ']']).parse().ok()?;
    Some(SocketAddr::new(ip, port.parse().ok()?))
}

pub fn parse_time(text: &str) -> Option<Micros> {
    if let Ok(t) = DateTime::parse_from_rfc3339(text) {
        return Some(t.timestamp_micros());
    }
    let naive = NaiveDateTime::parse_from_str(text, "%Y-%m-%dT%H:%M:%S%.f").ok()?;
    Local.from_local_datetime(&naive).earliest().map(|t| t.timestamp_micros())
}

pub fn format_time(micros: Micros) -> String {
    Local
        .timestamp_micros(micros)
        .single()
        .map(|t| t.format("%Y-%m-%dT%H:%M:%S%.6f%:z").to_string())
        .unwrap_or_default()
}

pub fn parse_line(line: &str) -> Option<TcpSegment> {
    let parsed: Line = serde_json::from_str(line).ok()?;
    Some(TcpSegment {
        timestamp: parse_time(&parsed.t)?,
        source: parse_endpoint(&parsed.from)?,
        destination: parse_endpoint(&parsed.to)?,
        sequence: parsed.seq,
        payload: base64::engine::general_purpose::STANDARD.decode(parsed.data).ok()?,
        from_server: parsed.server,
    })
}

pub fn read(path: &Path) -> std::io::Result<impl Iterator<Item = TcpSegment>> {
    let file = std::fs::File::open(path)?;
    Ok(BufReader::new(file).lines().map_while(Result::ok).filter(|l| !l.trim().is_empty()).filter_map(|l| parse_line(&l)))
}

pub struct Writer {
    out: std::io::BufWriter<std::fs::File>,
    pub count: u64,
}

impl Writer {
    pub fn create(path: &Path) -> std::io::Result<Writer> {
        Ok(Writer { out: std::io::BufWriter::with_capacity(64 * 1024, std::fs::File::create(path)?), count: 0 })
    }

    pub fn write(&mut self, segment: &TcpSegment) -> std::io::Result<()> {
        let line = Line {
            t: format_time(segment.timestamp),
            from: segment.source.to_string(),
            to: segment.destination.to_string(),
            seq: segment.sequence,
            server: segment.from_server,
            data: base64::engine::general_purpose::STANDARD.encode(&segment.payload),
        };
        serde_json::to_writer(&mut self.out, &line)?;
        self.out.write_all(b"\n")?;
        self.count += 1;
        Ok(())
    }

    pub fn flush(&mut self) -> std::io::Result<()> {
        self.out.flush()
    }
}
