//! Tries every 4-byte aligned 32-byte window of the game's memory as the pak AES-256 key. A window is a key when it
//! decrypts the first block of the pak index to a mount point ("../"). find_aes_key.py looks for the code that builds
//! the key; this finds the key where the game keeps it after building it.
//!
//! Usage: keyscan <pid> <pak with an encrypted index>      (needs ptrace_scope 0 or CAP_SYS_PTRACE)

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::os::unix::fs::FileExt;

use aes::cipher::{generic_array::GenericArray, BlockDecrypt, KeyInit};
use aes::Aes256;

fn index_block(pak: &str) -> [u8; 16] {
    let mut f = File::open(pak).expect("cannot open the pak");
    let size = f.seek(SeekFrom::End(0)).unwrap();
    let start = size.saturating_sub(4096);
    let mut tail = Vec::new();
    f.seek(SeekFrom::Start(start)).unwrap();
    f.read_to_end(&mut tail).unwrap();
    let magic = 0x5A6F12E1u32.to_le_bytes();
    let at = tail.windows(4).rposition(|w| w == magic).expect("no pak footer");
    assert!(tail[at - 1] != 0, "the pak index is not encrypted");
    let offset = u64::from_le_bytes(tail[at + 8..at + 16].try_into().unwrap());
    let mut block = [0u8; 16];
    f.read_exact_at(&mut block, offset).unwrap();
    block
}

/// Keys are random bytes: many distinct values, no long runs.
fn plausible(key: &[u8]) -> bool {
    let mut seen = [false; 256];
    let mut distinct = 0;
    for &b in key {
        if !seen[b as usize] {
            seen[b as usize] = true;
            distinct += 1;
        }
    }
    distinct >= 22 && !key.windows(4).any(|w| w[0] == w[1] && w[1] == w[2] && w[2] == w[3])
}

fn opens(key: &[u8], block: &[u8; 16]) -> bool {
    let cipher = Aes256::new(GenericArray::from_slice(key));
    let mut b = GenericArray::clone_from_slice(block);
    cipher.decrypt_block(&mut b);
    let length = i32::from_le_bytes([b[0], b[1], b[2], b[3]]);
    length > 0 && length < 1024 && &b[4..7] == b"../"
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let (pid, pak) = match args.as_slice() {
        [_, pid, pak] => (pid.clone(), pak.clone()),
        _ => {
            eprintln!("usage: keyscan <pid> <pak>");
            std::process::exit(2);
        }
    };
    let block = index_block(&pak);
    let maps = std::fs::read_to_string(format!("/proc/{pid}/maps")).expect("cannot read the process maps");
    let mem = File::open(format!("/proc/{pid}/mem")).expect("cannot open the process memory (ptrace_scope?)");
    let regions: Vec<(u64, u64)> = maps
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let range = fields.next()?;
            let perms = fields.next()?;
            let (lo, hi) = range.split_once('-')?;
            let (lo, hi) = (u64::from_str_radix(lo, 16).ok()?, u64::from_str_radix(hi, 16).ok()?);
            perms.starts_with('r').then_some((lo, hi))
        })
        .collect();
    let total: u64 = regions.iter().map(|(lo, hi)| hi - lo).sum();
    eprintln!("{} regions, {} MB", regions.len(), total >> 20);

    const CHUNK: u64 = 64 << 20;
    let mut tested: u64 = 0;
    let mut buffer = vec![0u8; (CHUNK + 32) as usize];
    for &(lo, hi) in &regions {
        let mut at = lo;
        while at < hi {
            let len = (hi - at).min(CHUNK + 32) as usize;
            let Ok(n) = mem.read_at(&mut buffer[..len], at) else { break };
            let data = &buffer[..n];
            let mut i = 0;
            while i + 32 <= data.len() {
                let key = &data[i..i + 32];
                if plausible(key) {
                    tested += 1;
                    if opens(key, &block) {
                        let hex: String = key.iter().map(|b| format!("{b:02X}")).collect();
                        println!("0x{hex}");
                        eprintln!("found at {:#x} after {tested} plausible windows", at + i as u64);
                        return;
                    }
                }
                i += 4;
            }
            at += CHUNK;
        }
    }
    eprintln!("no key found ({tested} plausible windows tried)");
    std::process::exit(1);
}
