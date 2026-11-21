use memchr::{memchr, memchr_iter};
use memmap2::Mmap;
use std::fs::{self, File};
use std::path::Path;

pub const DAY: i64 = 86_400_000;
pub const WEEK: i64 = 7 * DAY;
const CHUNK: usize = 32 * 1024 * 1024;

pub fn maps(directory: &Path) -> Vec<Mmap> {
    fs::read_dir(directory)
        .into_iter()
        .flatten()
        .map(|entry| entry.unwrap().path())
        .filter(|file| file.extension().is_some_and(|ext| ext == "csv"))
        .filter_map(|file| {
            let file = File::open(file).unwrap();
            (file.metadata().unwrap().len() != 0).then(|| unsafe { Mmap::map(&file).unwrap() })
        })
        .collect()
}

pub fn chunks(files: &[Mmap]) -> Vec<&[u8]> {
    let mut chunks = Vec::new();
    for file in files {
        let Some(header) = memchr(b'\n', file) else {
            continue;
        };
        let mut start = header + 1;
        while start < file.len() {
            let end = start.saturating_add(CHUNK).min(file.len());
            let end = memchr(b'\n', &file[end..]).map_or(file.len(), |tail| end + tail + 1);
            chunks.push(&file[start..end]);
            start = end;
        }
    }
    chunks.sort_unstable_by_key(|chunk| std::cmp::Reverse(chunk.len()));
    chunks
}

pub fn lines(chunk: &[u8]) -> impl Iterator<Item = &[u8]> {
    let mut start = 0;
    memchr_iter(b'\n', chunk)
        .chain(std::iter::once(chunk.len()))
        .map(move |end| {
            let line = &chunk[start..end];
            start = end + 1;
            line
        })
}

pub fn fields<const N: usize>(line: &[u8]) -> Option<[&[u8]; N]> {
    let mut result = [b"".as_slice(); N];
    let mut commas = memchr_iter(b',', line);
    let mut start = 0;
    if memchr(b'"', line).is_none() {
        for field in &mut result[..N - 1] {
            let end = commas.next()?;
            *field = &line[start..end];
            start = end + 1;
        }
        if commas.next().is_some() {
            return None;
        }
        result[N - 1] = &line[start..];
        return Some(result);
    }
    for field in &mut result[..N - 1] {
        let end = commas.next()?;
        *field = unquote(&line[start..end])?;
        start = end + 1;
    }
    if commas.next().is_some() {
        return None;
    }
    result[N - 1] = unquote(&line[start..])?;
    Some(result)
}

#[inline]
fn unquote(field: &[u8]) -> Option<&[u8]> {
    if field.first() == Some(&b'"') {
        if field.len() < 2 || field.last() != Some(&b'"') {
            return None;
        }
        let inner = &field[1..field.len() - 1];
        memchr(b'"', inner).is_none().then_some(inner)
    } else {
        memchr(b'"', field).is_none().then_some(field)
    }
}

#[inline]
pub fn fast_timestamp_f1(bytes: &[u8]) -> Option<i64> {
    if bytes.len() != 20
        || bytes[4] != b'-'
        || bytes[7] != b'-'
        || bytes[10] != b'T'
        || bytes[13] != b':'
        || bytes[16] != b':'
        || bytes[19] != b'Z'
    {
        return None;
    }
    let year = pair(bytes, 0)? * 100 + pair(bytes, 2)?;
    let month = pair(bytes, 5)?;
    let date = pair(bytes, 8)?;
    let hour = pair(bytes, 11)?;
    let minute = pair(bytes, 14)?;
    let second = pair(bytes, 17)?;
    if year == 0 || hour > 23 || minute > 59 || second > 59 {
        return None;
    }
    let year = year - i64::from(month <= 2);
    let month = if month > 2 { month - 3 } else { month + 9 };
    let days = year * 365 + year / 4 - year / 100 + year / 400 + (153 * month + 2) / 5 + date
        - 1
        - 719_468;
    Some(days * DAY + (hour * 3600 + minute * 60 + second) * 1000)
}

pub fn digits(bytes: &[u8]) -> Option<i64> {
    if bytes.is_empty() {
        return None;
    }
    let mut value = 0_i64;
    for &byte in bytes {
        let digit = byte.wrapping_sub(b'0');
        if digit > 9 {
            return None;
        }
        value = value.checked_mul(10)?.checked_add(i64::from(digit))?;
    }
    Some(value)
}

#[inline]
fn pair(bytes: &[u8], pos: usize) -> Option<i64> {
    let a = bytes[pos].wrapping_sub(b'0');
    let b = bytes[pos + 1].wrapping_sub(b'0');
    (a <= 9 && b <= 9).then_some(i64::from(a) * 10 + i64::from(b))
}

pub fn timestamp(bytes: &[u8]) -> Option<i64> {
    if bytes.get(4) != Some(&b'-') {
        return digits(bytes);
    }
    if (bytes.len() != 20 && bytes.len() != 25)
        || bytes[7] != b'-'
        || bytes[10] != b'T'
        || bytes[13] != b':'
        || bytes[16] != b':'
    {
        return None;
    }
    let year = pair(bytes, 0)? * 100 + pair(bytes, 2)?;
    let month = pair(bytes, 5)?;
    let date = pair(bytes, 8)?;
    let hour = pair(bytes, 11)?;
    let minute = pair(bytes, 14)?;
    let second = pair(bytes, 17)?;
    if year == 0 || hour > 23 || minute > 59 || second > 59 {
        return None;
    }
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let days_in_month = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => 28 + i64::from(leap),
        _ => return None,
    };
    if date == 0 || date > days_in_month {
        return None;
    }
    let offset = match (bytes.len(), bytes[19]) {
        (20, b'Z') => 0,
        (25, sign @ (b'+' | b'-')) if bytes[22] == b':' => {
            let h = pair(bytes, 20)?;
            let m = pair(bytes, 23)?;
            if h > 23 || m > 59 {
                return None;
            }
            (h * 60 + m) * if sign == b'+' { 1 } else { -1 }
        }
        _ => return None,
    };
    let year = year - i64::from(month <= 2);
    let month = if month > 2 { month - 3 } else { month + 9 };
    let days = year * 365 + year / 4 - year / 100 + year / 400 + (153 * month + 2) / 5 + date
        - 1
        - 719_468;
    Some(days * DAY + ((hour * 60 + minute - offset) * 60 + second) * 1000)
}
