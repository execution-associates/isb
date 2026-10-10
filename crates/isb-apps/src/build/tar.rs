use std::io::{Read, Write};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::Path;

use crate::error::Result;

fn octal(field: &mut [u8], v: u64) {
    // Sizes are capped well below 8 GiB, so 11 digits always suffice.
    let w = field.len() - 1;
    let s = format!("{v:0w$o}");
    let b = s.as_bytes();
    let start = b.len().saturating_sub(w);
    field[..w].copy_from_slice(&b[start..]);
    field[w] = 0;
}

fn header(name: &str, size: u64, mode: u32, mtime: u64, kind: u8, link: &str) -> [u8; 512] {
    let mut h = [0u8; 512];
    let n = name.as_bytes();
    h[..n.len().min(100)].copy_from_slice(&n[..n.len().min(100)]);
    octal(&mut h[100..108], (mode & 0o7777) as u64);
    octal(&mut h[108..116], 0);
    octal(&mut h[116..124], 0);
    octal(&mut h[124..136], size);
    octal(&mut h[136..148], mtime);
    h[156] = kind;
    let l = link.as_bytes();
    h[157..157 + l.len().min(100)].copy_from_slice(&l[..l.len().min(100)]);
    h[257..263].copy_from_slice(b"ustar\0");
    h[263..265].copy_from_slice(b"00");
    h[148..156].copy_from_slice(b"        ");
    let sum: u64 = h.iter().map(|b| *b as u64).sum();
    let s = format!("{sum:06o}\0 ");
    h[148..156].copy_from_slice(s.as_bytes());
    h
}

fn pad(w: &mut dyn Write, n: u64) -> std::io::Result<()> {
    let r = (512 - (n % 512) as usize) % 512;
    w.write_all(&vec![0u8; r])
}

fn pax_record(key: &str, value: &str) -> String {
    // "<len> key=value\n", where len counts itself.
    let body = format!(" {key}={value}\n");
    let mut len = body.len() + 1;
    while format!("{len}{body}").len() != len {
        len = format!("{len}{body}").len();
    }
    format!("{len}{body}")
}

fn entry(
    w: &mut dyn Write,
    name: &str,
    size: u64,
    mode: u32,
    mtime: u64,
    kind: u8,
    link: &str,
) -> std::io::Result<()> {
    if name.len() > 100 || link.len() > 100 || !name.is_ascii() || !link.is_ascii() {
        let mut pax = pax_record("path", name);
        if !link.is_empty() {
            pax.push_str(&pax_record("linkpath", link));
        }
        w.write_all(&header(
            "././@PaxHeader",
            pax.len() as u64,
            0o644,
            mtime,
            b'x',
            "",
        ))?;
        w.write_all(pax.as_bytes())?;
        pad(w, pax.len() as u64)?;
    }
    w.write_all(&header(name, size, mode, mtime, kind, link))
}

/// Write `dir`'s contents (not `dir` itself) as a tar.
pub fn write_dir(w: &mut dyn Write, dir: &Path) -> Result<()> {
    walk(w, dir, "")?;
    w.write_all(&[0u8; 1024])?;
    Ok(())
}

fn walk(w: &mut dyn Write, dir: &Path, prefix: &str) -> Result<()> {
    let mut names: Vec<_> = std::fs::read_dir(dir)?
        .filter_map(|e| e.ok())
        .map(|e| e.file_name())
        .collect();
    names.sort();
    for n in names {
        let path = dir.join(&n);
        let Some(n) = n.to_str() else {
            // Not UTF-8: leave it out rather than mangle it.
            continue;
        };
        let name = format!("{prefix}{n}");
        let md = std::fs::symlink_metadata(&path)?;
        let mtime = md.mtime().max(0) as u64;
        let mode = md.permissions().mode();
        let ft = md.file_type();
        if ft.is_symlink() {
            let target = std::fs::read_link(&path)?;
            let Some(t) = target.to_str() else { continue };
            entry(w, &name, 0, 0o777, mtime, b'2', t)?;
        } else if ft.is_dir() {
            entry(w, &format!("{name}/"), 0, mode, mtime, b'5', "")?;
            walk(w, &path, &format!("{name}/"))?;
        } else if ft.is_file() {
            let mut f = std::fs::File::open(&path)?;
            let size = md.len();
            entry(w, &name, size, mode, mtime, b'0', "")?;
            // Exactly `size` bytes, even if the file changes meanwhile.
            let copied = std::io::copy(&mut (&mut f).take(size), w)?;
            if copied < size {
                std::io::copy(&mut std::io::repeat(0).take(size - copied), w)?;
            }
            pad(w, size)?;
        }
        // Sockets, fifos and devices are not source.
    }
    Ok(())
}
