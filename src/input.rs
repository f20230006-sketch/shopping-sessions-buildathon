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
    #[cfg(target_arch = "x86_64")]
    if (32..=128).contains(&line.len()) && std::is_x86_feature_detected!("avx2") {
        // The scanner loads only within this bounded line.
        let (mut commas, quotes) = unsafe { separator_masks(line) };
        if commas.count_ones() as usize != N - 1 {
            return None;
        }
        let mut start = 0;
        let mut endpoints = 0;
        let mut result = [b"".as_slice(); N];
        for field in &mut result[..N - 1] {
            let end = commas.trailing_zeros() as usize;
            commas &= commas - 1;
            *field = strip_ends(&line[start..end], &mut endpoints);
            start = end + 1;
        }
        result[N - 1] = strip_ends(&line[start..], &mut endpoints);
        return (endpoints == quotes).then_some(result);
    }

    let mut commas = memchr_iter(b',', line);
    let mut result = [b"".as_slice(); N];
    let mut start = 0;
    let mut endpoints = 0;
    for field in &mut result[..N - 1] {
        let end = commas.next()?;
        *field = strip_ends(&line[start..end], &mut endpoints);
        start = end + 1;
    }
    if commas.next().is_some() {
        return None;
    }
    result[N - 1] = strip_ends(&line[start..], &mut endpoints);
    (endpoints as usize == memchr_iter(b'"', line).count()).then_some(result)
}

#[inline]
fn strip_ends<'a>(raw: &'a [u8], endpoints: &mut u32) -> &'a [u8] {
    // As every recognized boundary consumes exactly two distinct quotes,
    // comparing to the line's total also rejects every internal quote.
    if let [b'"', content @ .., b'"'] = raw {
        *endpoints += 2;
        content
    } else {
        raw
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn separator_masks(line: &[u8]) -> (u128, u32) {
    use std::arch::x86_64::{
        __m256i, _mm256_cmpeq_epi8, _mm256_loadu_si256, _mm256_movemask_epi8, _mm256_set1_epi8,
    };
    let comma = _mm256_set1_epi8(b',' as i8);
    let quote = _mm256_set1_epi8(b'"' as i8);
    let scan = |index: usize| {
        // Each caller ensures index..index+32 lies entirely inside line.
        let bytes = unsafe { _mm256_loadu_si256(line.as_ptr().add(index).cast::<__m256i>()) };
        let commas = _mm256_movemask_epi8(_mm256_cmpeq_epi8(bytes, comma)) as u32;
        let quotes = _mm256_movemask_epi8(_mm256_cmpeq_epi8(bytes, quote)) as u32;
        (commas, quotes)
    };
    let mut offset = 0;
    let mut commas = 0;
    let mut quotes = 0;
    while offset + 32 <= line.len() {
        let (current, quoted) = scan(offset);
        commas |= u128::from(current) << offset;
        quotes += quoted.count_ones();
        offset += 32;
    }
    if offset < line.len() {
        let tail = line.len() - 32;
        let overlap = offset - tail;
        let (current, quoted) = scan(tail);
        commas |= u128::from(current >> overlap) << offset;
        quotes += (quoted >> overlap).count_ones();
    }
    (commas, quotes)
}

#[inline]
pub fn link_fields(line: &[u8]) -> Option<[&[u8]; 2]> {
    let comma = memchr(b',', line)?;
    let a = unquote(&line[..comma])?;
    let b = unquote(&line[comma + 1..])?;
    if a.is_empty() || b.is_empty() || memchr(b',', &line[comma + 1..]).is_some() {
        return None;
    }
    Some([a, b])
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

#[inline]
pub fn digits(bytes: &[u8]) -> Option<i64> {
    if bytes.is_empty() {
        return None;
    }
    let mut value = 0_i64;
    if bytes.len() <= 18 {
        for &byte in bytes {
            let digit = byte.wrapping_sub(b'0');
            if digit > 9 {
                return None;
            }
            value = value * 10 + i64::from(digit);
        }
    } else {
        for &byte in bytes {
            let digit = byte.wrapping_sub(b'0');
            if digit > 9 {
                return None;
            }
            value = value.checked_mul(10)?.checked_add(i64::from(digit))?;
        }
    }
    Some(value)
}

#[cfg(test)]
mod tests {
    use super::digits;

    #[test]
    fn decimal_does_not_wrap() {
        assert_eq!(digits(b"18446744073709551617"), None);
        assert_eq!(digits(b"9223372036854775807"), Some(i64::MAX));
        assert_eq!(digits(b"000000000000000000001"), Some(1));
    }
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
