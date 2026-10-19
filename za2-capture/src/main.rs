use std::io::{BufWriter, Write};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::time::{Duration, Instant};

const MSG_SEGMENT: u8 = 1;
const MSG_STATUS: u8 = 2;
const MSG_STATS: u8 = 3;
// Game traffic is a few KB/s; with immediate mode every packet takes a ~64 KB frame of this ring, so it stays resident.
const KERNEL_BUFFER_BYTES: i32 = 8 * 1024 * 1024;
const RESCAN_INTERVAL: Duration = Duration::from_secs(5);
const STATS_INTERVAL: Duration = Duration::from_secs(2);

struct Options {
    ports: Vec<u16>,
    device: Option<String>,
    list: bool,
}

fn parse_options() -> Result<Options, String> {
    let mut options = Options { ports: Vec::new(), device: None, list: false };
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--ports" => {
                let value = args.next().ok_or("--ports needs a value")?;
                for p in value.split(',') {
                    options.ports.push(p.trim().parse().map_err(|_| format!("bad port {p}"))?);
                }
            }
            "--device" => options.device = Some(args.next().ok_or("--device needs a value")?),
            "--list" => options.list = true,
            other => return Err(format!("unknown argument {other}")),
        }
    }
    if options.ports.is_empty() && !options.list {
        return Err("usage: za2-capture --ports 13328[,port...] [--device NAME] | --list".into());
    }
    Ok(options)
}

struct Output {
    out: BufWriter<std::io::Stdout>,
}

impl Output {
    fn status(&mut self, text: &str) -> std::io::Result<()> {
        let bytes = text.as_bytes();
        let len = bytes.len().min(u16::MAX as usize);
        self.out.write_all(&[MSG_STATUS])?;
        self.out.write_all(&(len as u16).to_le_bytes())?;
        self.out.write_all(&bytes[..len])?;
        self.out.flush()
    }

    fn stats(&mut self, received: u32, dropped: u32, if_dropped: u32, segments: u64) -> std::io::Result<()> {
        self.out.write_all(&[MSG_STATS])?;
        for v in [received, dropped, if_dropped] {
            self.out.write_all(&v.to_le_bytes())?;
        }
        self.out.write_all(&segments.to_le_bytes())?;
        self.out.flush()
    }

    fn segment(&mut self, ts_micros: i64, tcp: &TcpView, from_server: bool) -> std::io::Result<()> {
        self.out.write_all(&[MSG_SEGMENT])?;
        self.out.write_all(&ts_micros.to_le_bytes())?;
        write_ip(&mut self.out, tcp.source)?;
        self.out.write_all(&tcp.source_port.to_le_bytes())?;
        write_ip(&mut self.out, tcp.destination)?;
        self.out.write_all(&tcp.destination_port.to_le_bytes())?;
        self.out.write_all(&tcp.sequence.to_le_bytes())?;
        self.out.write_all(&[from_server as u8])?;
        self.out.write_all(&(tcp.payload.len() as u32).to_le_bytes())?;
        self.out.write_all(tcp.payload)?;
        self.out.flush()
    }
}

fn write_ip(out: &mut impl Write, ip: IpAddr) -> std::io::Result<()> {
    match ip {
        IpAddr::V4(v4) => {
            out.write_all(&[4])?;
            out.write_all(&v4.to_ipv6_mapped().octets())
        }
        IpAddr::V6(v6) => {
            out.write_all(&[6])?;
            out.write_all(&v6.octets())
        }
    }
}

struct TcpView<'a> {
    source: IpAddr,
    destination: IpAddr,
    source_port: u16,
    destination_port: u16,
    sequence: u32,
    payload: &'a [u8],
}

fn parse_packet(linktype: i32, data: &[u8]) -> Option<TcpView<'_>> {
    let (ether_type, ip) = match linktype {
        1 => {
            let mut offset = 14;
            let mut ether_type = u16::from_be_bytes([*data.get(12)?, *data.get(13)?]);
            while ether_type == 0x8100 || ether_type == 0x88a8 {
                ether_type = u16::from_be_bytes([*data.get(offset + 2)?, *data.get(offset + 3)?]);
                offset += 4;
            }
            (ether_type, data.get(offset..)?)
        }
        113 => (u16::from_be_bytes([*data.get(14)?, *data.get(15)?]), data.get(16..)?),
        276 => (u16::from_be_bytes([*data.first()?, *data.get(1)?]), data.get(20..)?),
        0 | 108 => {
            let family = u32::from_ne_bytes(data.get(..4)?.try_into().ok()?);
            (if family == 2 { 0x0800 } else { 0x86dd }, data.get(4..)?)
        }
        12 | 101 => (if data.first()? >> 4 == 4 { 0x0800 } else { 0x86dd }, data),
        _ => return None,
    };

    let (source, destination, tcp) = match ether_type {
        0x0800 => {
            if ip.len() < 20 {
                return None;
            }
            let header = ((*ip.first()? & 0x0f) as usize) * 4;
            if *ip.get(9)? != 6 || header < 20 {
                return None;
            }
            let total = (u16::from_be_bytes([*ip.get(2)?, *ip.get(3)?]) as usize).min(ip.len());
            let src = Ipv4Addr::new(ip[12], ip[13], ip[14], ip[15]);
            let dst = Ipv4Addr::new(ip[16], ip[17], ip[18], ip[19]);
            (IpAddr::V4(src), IpAddr::V4(dst), ip.get(header..total)?)
        }
        0x86dd => {
            if *ip.get(6)? != 6 {
                return None;
            }
            let length = u16::from_be_bytes([ip[4], ip[5]]) as usize;
            let src: [u8; 16] = ip.get(8..24)?.try_into().ok()?;
            let dst: [u8; 16] = ip.get(24..40)?.try_into().ok()?;
            (IpAddr::V6(Ipv6Addr::from(src)), IpAddr::V6(Ipv6Addr::from(dst)), ip.get(40..(40 + length).min(ip.len()))?)
        }
        _ => return None,
    };

    let offset = ((*tcp.get(12)? >> 4) as usize) * 4;
    if offset < 20 {
        return None;
    }
    Some(TcpView {
        source,
        destination,
        source_port: u16::from_be_bytes([tcp[0], tcp[1]]),
        destination_port: u16::from_be_bytes([tcp[2], tcp[3]]),
        sequence: u32::from_be_bytes([tcp[4], tcp[5], tcp[6], tcp[7]]),
        payload: tcp.get(offset..).unwrap_or(&[]),
    })
}

fn parse_proc_ip(hex: &str) -> Option<IpAddr> {
    match hex.len() {
        8 => Some(IpAddr::V4(Ipv4Addr::from(u32::from_str_radix(hex, 16).ok()?.swap_bytes()))),
        32 => {
            let mut bytes = [0u8; 16];
            for (i, chunk) in bytes.chunks_mut(4).enumerate() {
                let word = u32::from_str_radix(&hex[i * 8..i * 8 + 8], 16).ok()?.swap_bytes();
                chunk.copy_from_slice(&word.to_be_bytes());
            }
            let v6 = Ipv6Addr::from(bytes);
            Some(v6.to_ipv4_mapped().map(IpAddr::V4).unwrap_or(IpAddr::V6(v6)))
        }
        _ => None,
    }
}

fn game_connections(ports: &[u16]) -> Vec<(IpAddr, IpAddr, u16)> {
    let mut found = Vec::new();
    for file in ["/proc/net/tcp", "/proc/net/tcp6"] {
        let Ok(text) = std::fs::read_to_string(file) else { continue };
        for line in text.lines().skip(1) {
            let fields: Vec<&str> = line.split_whitespace().collect();
            if fields.len() < 4 || fields[3] != "01" {
                continue;
            }
            let (Some((local, _)), Some((remote, rport))) = (fields[1].split_once(':'), fields[2].split_once(':')) else { continue };
            let Ok(rport) = u16::from_str_radix(rport, 16) else { continue };
            if !ports.contains(&rport) {
                continue;
            }
            if let (Some(l), Some(r)) = (parse_proc_ip(local), parse_proc_ip(remote)) {
                found.push((l, r, rport));
            }
        }
    }
    found
}

fn device_for(local: IpAddr) -> Option<pcap::Device> {
    pcap::Device::list().ok()?.into_iter().find(|d| d.addresses.iter().any(|a| a.addr == local))
}

fn list_devices() {
    match pcap::Device::list() {
        Ok(devices) => {
            for d in devices {
                let addrs: Vec<String> = d.addresses.iter().map(|a| a.addr.to_string()).collect();
                println!("{}\t{}", d.name, addrs.join(" "));
            }
        }
        Err(e) => eprintln!("za2-capture: cannot list devices: {e}"),
    }
}

enum CaptureEnd {
    Rescan,
    OutputClosed,
}

fn capture(device: pcap::Device, ports: &[u16], expected_local: Option<IpAddr>, out: &mut Output, segments: &mut u64) -> Result<CaptureEnd, String> {
    let name = device.name.clone();
    let mut cap = pcap::Capture::from_device(device)
        .and_then(|c| c.promisc(false).snaplen(65535).timeout(200).immediate_mode(true).buffer_size(KERNEL_BUFFER_BYTES).open())
        .map_err(|e| format!("cannot open {name}: {e}"))?;
    let filter = format!("tcp and ({})", ports.iter().map(|p| format!("port {p}")).collect::<Vec<_>>().join(" or "));
    cap.filter(&filter, true).map_err(|e| format!("cannot set filter on {name}: {e}"))?;
    let linktype = cap.get_datalink().0;
    if out.status(&format!("capturing on {name}, filter \"{filter}\"")).is_err() {
        return Ok(CaptureEnd::OutputClosed);
    }

    let mut last_rescan = Instant::now();
    let mut last_stats = Instant::now();
    loop {
        match cap.next_packet() {
            Ok(packet) => {
                if let Some(tcp) = parse_packet(linktype, packet.data) {
                    if !tcp.payload.is_empty() {
                        let ts = packet.header.ts.tv_sec as i64 * 1_000_000 + packet.header.ts.tv_usec as i64;
                        let from_server = ports.contains(&tcp.source_port);
                        *segments += 1;
                        if out.segment(ts, &tcp, from_server).is_err() {
                            return Ok(CaptureEnd::OutputClosed);
                        }
                    }
                }
            }
            Err(pcap::Error::TimeoutExpired) => {}
            Err(e) => return Err(format!("capture on {name} ended: {e}")),
        }

        if last_stats.elapsed() >= STATS_INTERVAL {
            last_stats = Instant::now();
            let stat = cap.stats().unwrap_or(pcap::Stat { received: 0, dropped: 0, if_dropped: 0 });
            if out.stats(stat.received, stat.dropped, stat.if_dropped, *segments).is_err() {
                return Ok(CaptureEnd::OutputClosed);
            }
        }

        if let Some(local) = expected_local {
            if last_rescan.elapsed() >= RESCAN_INTERVAL {
                last_rescan = Instant::now();
                let connections = game_connections(ports);
                if !connections.is_empty() && !connections.iter().any(|c| c.0 == local) {
                    let _ = out.status("game connection moved to another address, switching adapter");
                    return Ok(CaptureEnd::Rescan);
                }
            }
        }
    }
}

fn main() {
    let options = match parse_options() {
        Ok(o) => o,
        Err(e) => {
            eprintln!("za2-capture: {e}");
            std::process::exit(2);
        }
    };
    if options.list {
        list_devices();
        return;
    }

    // stdin closes when za2 ends, even when it crashes or is killed; without this the capture outlives it while waiting.
    if std::env::var_os("ZA2_EXIT_WITH_PARENT").is_some() {
        std::thread::spawn(|| {
            let _ = std::io::copy(&mut std::io::stdin().lock(), &mut std::io::sink());
            std::process::exit(0);
        });
    }

    let mut out = Output { out: BufWriter::with_capacity(256 * 1024, std::io::stdout()) };
    let mut segments = 0u64;
    let mut waiting_reported = false;
    loop {
        let (device, local) = if let Some(name) = &options.device {
            match pcap::Device::list().ok().and_then(|list| list.into_iter().find(|d| &d.name == name)) {
                Some(d) => (d, None),
                None => {
                    let _ = out.status(&format!("device {name} not found"));
                    std::process::exit(1);
                }
            }
        } else {
            let connections = game_connections(&options.ports);
            match connections.iter().find_map(|c| device_for(c.0).map(|d| (d, c.0, c.1))) {
                Some((d, local, remote)) => {
                    waiting_reported = false;
                    if out.status(&format!("game connection {local} -> {remote} on {}", d.name)).is_err() {
                        return;
                    }
                    (d, Some(local))
                }
                None => {
                    if !waiting_reported {
                        waiting_reported = true;
                        let text = if connections.is_empty() { "waiting for the game connection" } else { "game connection found, but no capture device has its address" };
                        if out.status(text).is_err() {
                            return;
                        }
                    }
                    std::thread::sleep(Duration::from_secs(2));
                    continue;
                }
            }
        };

        match capture(device, &options.ports, local, &mut out, &mut segments) {
            Ok(CaptureEnd::OutputClosed) => return,
            Ok(CaptureEnd::Rescan) => {}
            Err(message) => {
                if out.status(&message).is_err() {
                    return;
                }
                if message.contains("Operation not permitted") || message.contains("permission") {
                    std::process::exit(3);
                }
                std::thread::sleep(Duration::from_secs(2));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proc_net_addresses() {
        assert_eq!(parse_proc_ip("090FA8C0"), Some("192.168.15.9".parse().unwrap()));
        assert_eq!(parse_proc_ip("0000000000000000FFFF0000090FA8C0"), Some("192.168.15.9".parse().unwrap()));
    }

    #[test]
    fn ethernet_ipv4_tcp_payload() {
        let mut frame = vec![0u8; 14];
        frame[12] = 0x08;
        let mut ip = vec![0x45, 0, 0, 0, 0, 0, 0, 0, 64, 6, 0, 0, 193, 202, 112, 173, 192, 168, 15, 9];
        let mut tcp = vec![0x34, 0x10, 0x8b, 0x26, 0, 0, 0x10, 0x00, 0, 0, 0, 0, 0x50, 0x18, 0, 0, 0, 0, 0, 0];
        tcp.extend_from_slice(b"hello");
        let total = (ip.len() + tcp.len()) as u16;
        ip[2..4].copy_from_slice(&total.to_be_bytes());
        frame.extend(ip);
        frame.extend(tcp);
        frame.extend([0, 0, 0, 0]);
        let view = parse_packet(1, &frame).unwrap();
        assert_eq!(view.source, "193.202.112.173".parse::<IpAddr>().unwrap());
        assert_eq!(view.source_port, 13328);
        assert_eq!(view.destination_port, 35622);
        assert_eq!(view.sequence, 0x1000);
        assert_eq!(view.payload, b"hello");
    }
}
