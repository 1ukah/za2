use std::ffi::c_void;
use std::io;

#[repr(C)]
struct IoVec {
    base: *mut c_void,
    len: usize,
}

extern "C" {
    fn process_vm_readv(pid: i32, local: *const IoVec, local_count: u64, remote: *const IoVec, remote_count: u64, flags: u64) -> isize;
}

/// Most iovecs one `process_vm_readv` call takes (IOV_MAX on Linux).
const IOV_MAX: usize = 1024;
const EFAULT: i32 = 14;

#[derive(Clone, Debug)]
pub struct Region {
    pub start: u64,
    pub end: u64,
    pub perms: String,
    pub path: String,
}

impl Region {
    pub fn readable(&self) -> bool {
        self.perms.starts_with('r')
    }

    pub fn writable(&self) -> bool {
        self.perms.as_bytes().get(1) == Some(&b'w')
    }
}

/// Read-only access to another process's memory.
#[derive(Clone, Copy)]
pub struct Process {
    pub pid: i32,
}

impl Process {
    /// Reads `buf.len()` bytes at `addr`. Fails if any byte is unreadable.
    pub fn read(&self, addr: u64, buf: &mut [u8]) -> io::Result<()> {
        if buf.is_empty() {
            return Ok(());
        }
        let local = IoVec { base: buf.as_mut_ptr().cast(), len: buf.len() };
        let remote = IoVec { base: addr as *mut c_void, len: buf.len() };
        let n = unsafe { process_vm_readv(self.pid, &local, 1, &remote, 1, 0) };
        if n < 0 {
            return Err(io::Error::last_os_error());
        }
        if n as usize != buf.len() {
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "partial read"));
        }
        Ok(())
    }

    pub fn read_vec(&self, addr: u64, len: usize) -> io::Result<Vec<u8>> {
        let mut buf = vec![0u8; len];
        self.read(addr, &mut buf)?;
        Ok(buf)
    }

    pub fn u64(&self, addr: u64) -> Option<u64> {
        let mut b = [0u8; 8];
        self.read(addr, &mut b).ok().map(|_| u64::from_le_bytes(b))
    }

    pub fn u32(&self, addr: u64) -> Option<u32> {
        let mut b = [0u8; 4];
        self.read(addr, &mut b).ok().map(|_| u32::from_le_bytes(b))
    }

    pub fn f64s<const N: usize>(&self, addr: u64) -> Option<[f64; N]> {
        let mut b = vec![0u8; N * 8];
        self.read(addr, &mut b).ok()?;
        let mut out = [0f64; N];
        for (i, v) in out.iter_mut().enumerate() {
            *v = f64::from_le_bytes(b[i * 8..i * 8 + 8].try_into().unwrap());
        }
        Some(out)
    }

    /// Reads many small blocks with few system calls: `len` bytes at each address, back to back in the result.
    /// A block that cannot be read is `None`; the others are still read.
    pub fn read_many(&self, addrs: &[u64], len: usize) -> Vec<Option<Vec<u8>>> {
        let mut data = vec![0u8; addrs.len() * len];
        let mut ok = vec![false; addrs.len()];
        let mut start = 0;
        while start < addrs.len() {
            let end = (start + IOV_MAX).min(addrs.len());
            let base = data.as_mut_ptr();
            let local: Vec<IoVec> = (start..end).map(|i| IoVec { base: unsafe { base.add(i * len) }.cast(), len }).collect();
            let remote: Vec<IoVec> = addrs[start..end].iter().map(|&a| IoVec { base: a as *mut c_void, len }).collect();
            let n = unsafe { process_vm_readv(self.pid, local.as_ptr(), local.len() as u64, remote.as_ptr(), remote.len() as u64, 0) };
            if n < 0 && io::Error::last_os_error().raw_os_error() != Some(EFAULT) {
                break;
            }
            // The kernel stops at the first block it cannot read: everything before it is complete, that block is
            // skipped, and the call is repeated for the rest.
            let complete = if n < 0 { 0 } else { n as usize / len };
            for flag in &mut ok[start..start + complete] {
                *flag = true;
            }
            start += if complete == end - start { complete } else { complete + 1 };
        }
        data.chunks(len.max(1)).zip(ok).map(|(chunk, ok)| ok.then(|| chunk.to_vec())).take(addrs.len()).collect()
    }

    pub fn regions(&self) -> io::Result<Vec<Region>> {
        Ok(parse_maps(&std::fs::read_to_string(format!("/proc/{}/maps", self.pid))?))
    }

    pub fn alive(&self) -> bool {
        std::path::Path::new(&format!("/proc/{}", self.pid)).exists()
    }
}

pub fn parse_maps(text: &str) -> Vec<Region> {
    text.lines()
        .filter_map(|line| {
            let mut it = line.split_whitespace();
            let (start, end) = it.next()?.split_once('-')?;
            let perms = it.next()?.to_string();
            let path = it.nth(3).map(str::to_string).unwrap_or_default();
            Some(Region { start: u64::from_str_radix(start, 16).ok()?, end: u64::from_str_radix(end, 16).ok()?, perms, path })
        })
        .collect()
}

pub struct Game {
    pub process: Process,
    /// Where the game executable is loaded.
    pub base: u64,
}

/// The running game: the process that has `AION2.exe` loaded and uses the most memory (Proton starts a small
/// second copy of the executable, and the launcher scripts only name it on their command line).
pub fn find_game() -> Option<Game> {
    let mut best: Option<(u64, i32, u64)> = None;
    for entry in std::fs::read_dir("/proc").ok()?.flatten() {
        let Some(pid) = entry.file_name().to_str().and_then(|n| n.parse::<i32>().ok()) else { continue };
        let Ok(maps) = std::fs::read_to_string(format!("/proc/{pid}/maps")) else { continue };
        let Some(base) = parse_maps(&maps).iter().find(|r| r.path.to_ascii_lowercase().ends_with("/aion2.exe")).map(|r| r.start) else { continue };
        let rss = std::fs::read_to_string(format!("/proc/{pid}/statm")).ok().and_then(|s| s.split_whitespace().nth(1)?.parse::<u64>().ok()).unwrap_or(0);
        if best.map_or(true, |b| rss > b.0) {
            best = Some((rss, pid, base));
        }
    }
    best.map(|(_, pid, base)| Game { process: Process { pid }, base })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_lines() {
        let text = "140000000-140001000 r--p 00000000 00:22 1012898                          /games/AION2/Aion2/Binaries/Win64/AION2.exe\n\
                    150237000-158d6e000 rw-p 00000000 00:00 0 \n";
        let r = parse_maps(text);
        assert_eq!(r.len(), 2);
        assert_eq!((r[0].start, r[0].end), (0x140000000, 0x140001000));
        assert!(r[0].path.ends_with("AION2.exe") && !r[0].writable());
        assert!(r[1].readable() && r[1].writable() && r[1].path.is_empty());
    }

    #[test]
    fn reads_own_memory_in_batches() {
        let me = Process { pid: std::process::id() as i32 };
        let values: Vec<u64> = (0..3000u64).map(|i| i * 7 + 1).collect();
        let mut addrs: Vec<u64> = values.iter().map(|v| v as *const u64 as u64).collect();
        addrs.insert(1500, 8);
        let got = me.read_many(&addrs, 8);
        assert_eq!(got.len(), 3001);
        assert!(got[1500].is_none(), "address 8 is never mapped");
        for (i, v) in values.iter().enumerate() {
            let at = if i < 1500 { i } else { i + 1 };
            assert_eq!(got[at].as_deref().map(|b| u64::from_le_bytes(b.try_into().unwrap())), Some(*v));
        }
    }
}
