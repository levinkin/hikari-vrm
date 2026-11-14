use std::cell::RefCell;
#[cfg(target_os = "macos")]
use std::ffi::{c_char, c_void};

const HASH_SIZE: usize = 65536;
const HASH_MUL: u32 = 2654435761;
const MATCH_DEPTH: i32 = 64;
const JPEG_QUALITY: f64 = 0.5;
const SPLIT_QUALITY: f64 = 0.6;

#[cfg(target_os = "macos")]
type CFRef = *const c_void;
#[cfg(target_os = "macos")]
type CFMutable = *mut c_void;

#[cfg(target_os = "macos")]
#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFDataCreateWithBytesNoCopy(allocator: CFRef, bytes: *const u8, length: isize, deallocator: CFRef) -> CFRef;
    fn CFDataCreateMutable(allocator: CFRef, capacity: isize) -> CFMutable;
    fn CFDataGetLength(data: CFRef) -> isize;
    fn CFDataGetBytePtr(data: CFRef) -> *const u8;
    fn CFStringCreateWithCString(allocator: CFRef, cstr: *const c_char, encoding: u32) -> CFRef;
    fn CFNumberCreate(allocator: CFRef, kind: isize, value: *const c_void) -> CFRef;
    fn CFDictionaryCreate(
        allocator: CFRef,
        keys: *const CFRef,
        values: *const CFRef,
        count: isize,
        key_callbacks: *const c_void,
        value_callbacks: *const c_void,
    ) -> CFRef;
    fn CFRelease(cf: CFRef);
    static kCFAllocatorNull: CFRef;
}

#[cfg(target_os = "macos")]
#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGColorSpaceCreateDeviceRGB() -> CFRef;
    fn CGDataProviderCreateWithCFData(data: CFRef) -> CFRef;
    fn CGImageCreate(
        width: usize,
        height: usize,
        bits_per_component: usize,
        bits_per_pixel: usize,
        bytes_per_row: usize,
        space: CFRef,
        bitmap_info: u32,
        provider: CFRef,
        decode: *const c_void,
        should_interpolate: bool,
        intent: i32,
    ) -> CFRef;
}

#[cfg(target_os = "macos")]
#[link(name = "ImageIO", kind = "framework")]
extern "C" {
    fn CGImageDestinationCreateWithData(
        data: CFMutable,
        uti: CFRef,
        count: usize,
        options: CFRef,
    ) -> CFRef;
    fn CGImageDestinationAddImage(dest: CFRef, image: CFRef, properties: CFRef);
    fn CGImageDestinationFinalize(dest: CFRef) -> bool;
    static kCGImageDestinationLossyCompressionQuality: CFRef;
}

pub fn encode(rgba: &[u8], width: usize, height: usize, codec: &str) -> Vec<u8> {
    let px = width * height;
    if width == 0 || height == 0 || rgba.len() < px * 4 {
        return Vec::new();
    }
    match codec {
        "rgb565" => encode_565(rgba, px),
        "pal256" => encode_pal256(rgba, px),
        "split" => encode_split(rgba, width, height),
        "jpeg" | "mjpeg" => encode_jpeg(rgba, width, height, JPEG_QUALITY),
        "qoi" => encode_qoi(rgba, width, height),
        _ => panic!("unknown codec {}", codec),
    }
}

fn encode_qoi(rgba: &[u8], width: usize, height: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(width * height + 22);
    out.extend_from_slice(b"qoif");
    out.extend((width as u32).to_be_bytes());
    out.extend((height as u32).to_be_bytes());
    out.extend([4, 0]);
    let mut seen = [[0u8; 4]; 64];
    let mut prev = [0u8, 0, 0, 255];
    let mut run = 0u8;
    for px in rgba[..width * height * 4].chunks_exact(4) {
        let px = [px[0], px[1], px[2], px[3]];
        if px == prev {
            run += 1;
            if run == 62 {
                out.push(0xc0 | 61);
                run = 0;
            }
            continue;
        }
        if run > 0 {
            out.push(0xc0 | (run - 1));
            run = 0;
        }
        let slot = (px[0] as usize * 3 + px[1] as usize * 5 + px[2] as usize * 7 + px[3] as usize * 11) % 64;
        if seen[slot] == px {
            out.push(slot as u8);
        } else {
            seen[slot] = px;
            if px[3] != prev[3] {
                out.extend([0xff, px[0], px[1], px[2], px[3]]);
            } else {
                let dr = px[0].wrapping_sub(prev[0]) as i8 as i32;
                let dg = px[1].wrapping_sub(prev[1]) as i8 as i32;
                let db = px[2].wrapping_sub(prev[2]) as i8 as i32;
                let (dr_dg, db_dg) = (dr - dg, db - dg);
                if (-2..2).contains(&dr) && (-2..2).contains(&dg) && (-2..2).contains(&db) {
                    out.push(0x40 | ((dr + 2) << 4 | (dg + 2) << 2 | (db + 2)) as u8);
                } else if (-32..32).contains(&dg) && (-8..8).contains(&dr_dg) && (-8..8).contains(&db_dg) {
                    out.extend([0x80 | (dg + 32) as u8, ((dr_dg + 8) << 4 | (db_dg + 8)) as u8]);
                } else {
                    out.extend([0xfe, px[0], px[1], px[2]]);
                }
            }
        }
        prev = px;
    }
    if run > 0 {
        out.push(0xc0 | (run - 1));
    }
    out.extend([0, 0, 0, 0, 0, 0, 0, 1]);
    out
}

fn encode_565(rgba: &[u8], px: usize) -> Vec<u8> {
    let raw: Vec<u8> = rgba[..px * 4].chunks_exact(4).flat_map(|p| ((p[0] as u16 >> 3) << 11 | (p[1] as u16 >> 2) << 5 | (p[2] as u16 >> 3)).to_le_bytes()).collect();
    let mut out = Vec::with_capacity(raw.len() + raw.len() / 255 + 16);
    lz4_block(&raw, &mut out);
    out
}

fn encode_pal256(rgba: &[u8], px: usize) -> Vec<u8> {
    PAL_TABLES.with(|tables| {
        let (hist, key_to_idx) = &mut *tables.borrow_mut();
        let frame: Vec<u16> = rgba[..px * 4].chunks_exact(4).map(|p| (p[0] as u16 >> 3) << 11 | (p[1] as u16 >> 2) << 5 | (p[2] as u16 >> 3)).collect();
        let mut keys: Vec<u16> = Vec::new();
        for p in 0..px {
            let slot = &mut hist[frame[p] as usize];
            if *slot == 0 {
                keys.push(frame[p]);
            }
            *slot += 1;
        }
        let (count, pal_565, pal_rgb) = median_cut(&mut keys, hist);
        for key in &keys {
            hist[*key as usize] = 0;
        }
        let palette = &pal_rgb[..count * 3];
        for key in &keys {
            let k = *key;
            let r = channel(k, 0);
            let g = channel(k, 1);
            let b = channel(k, 2);
            let mut best = 0usize;
            let mut best_d = i32::MAX;
            for (c, rgb) in palette.chunks_exact(3).enumerate() {
                let dr = r - rgb[0];
                let dg = g - rgb[1];
                let db = b - rgb[2];
                let d = dr * dr + dg * dg + db * db;
                if d < best_d {
                    best_d = d;
                    best = c;
                }
            }
            key_to_idx[k as usize] = best as u8;
        }
        let mut idx = Vec::with_capacity(px);
        for p in 0..px {
            idx.push(key_to_idx[frame[p] as usize]);
        }
        let mut out = Vec::with_capacity(512 + px + px / 255 + 16);
        for v in &pal_565 {
            out.extend_from_slice(&v.to_le_bytes());
        }
        lz4_block(&idx, &mut out);
        out
    })
}

fn encode_split(rgba: &[u8], width: usize, height: usize) -> Vec<u8> {
    let half = height / 2;
    let split = width * half * 4;
    let top = encode_jpeg(&rgba[..split], width, half, SPLIT_QUALITY);
    let bottom = encode_jpeg(&rgba[split..split * 2], width, half, SPLIT_QUALITY);
    let mut out = Vec::with_capacity(4 + top.len() + bottom.len());
    out.extend_from_slice(&(top.len() as u32).to_be_bytes());
    out.extend_from_slice(&top);
    out.extend_from_slice(&bottom);
    out
}

#[cfg(not(target_os = "macos"))]
fn encode_jpeg(rgba: &[u8], width: usize, height: usize, quality: f64) -> Vec<u8> {
    let mut out = Vec::new();
    let encoder = jpeg_encoder::Encoder::new(&mut out, (25.0 + quality * 100.0).round().clamp(1.0, 100.0) as u8);
    if encoder.encode(rgba, width as u16, height as u16, jpeg_encoder::ColorType::Rgba).is_err() {
        return Vec::new();
    }
    out
}

#[cfg(target_os = "macos")]
fn encode_jpeg(rgba: &[u8], width: usize, height: usize, quality: f64) -> Vec<u8> {
    let mut out = Vec::new();
    unsafe {
        let data = CFDataCreateWithBytesNoCopy(std::ptr::null(), rgba.as_ptr(), rgba.len() as isize, kCFAllocatorNull);
        if data.is_null() {
            return out;
        }
        let provider = CGDataProviderCreateWithCFData(data);
        let space = CGColorSpaceCreateDeviceRGB();
        let image = CGImageCreate(
            width,
            height,
            8,
            32,
            width * 4,
            space,
            5,
            provider,
            std::ptr::null(),
            true,
            0,
        );
        let uti = CFStringCreateWithCString(std::ptr::null(), c"public.jpeg".as_ptr(), 0x0800_0100);
        let mutable = CFDataCreateMutable(std::ptr::null(), 0);
        let dest = CGImageDestinationCreateWithData(mutable, uti, 1, std::ptr::null());
        if !image.is_null() && !dest.is_null() {
            let key = kCGImageDestinationLossyCompressionQuality;
            let value = CFNumberCreate(std::ptr::null(), 6, &quality as *const f64 as *const c_void);
            let keys = [key];
            let values = [value];
            let props = CFDictionaryCreate(
                std::ptr::null(),
                keys.as_ptr(),
                values.as_ptr(),
                1,
                std::ptr::null(),
                std::ptr::null(),
            );
            CGImageDestinationAddImage(dest, image, props);
            if CGImageDestinationFinalize(dest) {
                let len = CFDataGetLength(mutable) as usize;
                let ptr = CFDataGetBytePtr(mutable);
                out.extend_from_slice(std::slice::from_raw_parts(ptr, len));
            }
            if !props.is_null() {
                CFRelease(props);
            }
            if !value.is_null() {
                CFRelease(value);
            }
        }
        for object in [dest, mutable as CFRef, uti, image, space, provider, data] {
            if !object.is_null() {
                CFRelease(object);
            }
        }
    }
    out
}

fn channel(key: u16, axis: usize) -> i32 {
    if axis == 0 {
        let c = ((key >> 11) & 31) as i32;
        c << 3 | c >> 2
    } else if axis == 1 {
        let c = ((key >> 5) & 63) as i32;
        c << 2 | c >> 4
    } else {
        let c = (key & 31) as i32;
        c << 3 | c >> 2
    }
}

fn box_span(keys: &[u16], lo: usize, hi: usize) -> (i32, usize) {
    if hi - lo <= 1 {
        return (-1, 0);
    }
    let mut span = -1i32;
    let mut axis = 0usize;
    for a in 0..3 {
        let mut low = 255i32;
        let mut high = 0i32;
        for i in lo..hi {
            let v = channel(keys[i], a);
            if v < low {
                low = v;
            }
            if v > high {
                high = v;
            }
        }
        if high - low > span {
            span = high - low;
            axis = a;
        }
    }
    (span, axis)
}

fn median_cut(
    keys: &mut [u16],
    hist: &[u32],
) -> (usize, [u16; 256], [i32; 768]) {
    let mut boxes: Vec<(usize, usize)> = vec![(0, keys.len())];
    let mut spans: Vec<(i32, usize)> = vec![box_span(keys, 0, keys.len())];
    let mut buckets: Vec<Vec<u16>> = vec![Vec::new(); 256];
    while boxes.len() < 256 {
        let mut pick = 0usize;
        let mut b = 1usize;
        while b < boxes.len() {
            if spans[b].0 > spans[pick].0 {
                pick = b;
            }
            b += 1;
        }
        if spans[pick].0 <= 0 {
            break;
        }
        let (lo, hi) = boxes.remove(pick);
        let axis = spans.remove(pick).1;

        for bucket in &mut buckets {
            bucket.clear();
        }
        for i in lo..hi {
            buckets[channel(keys[i], axis) as usize].push(keys[i]);
        }
        let mut w = lo;
        for bucket in &buckets {
            for key in bucket {
                keys[w] = *key;
                w += 1;
            }
        }

        let mut total: u64 = 0;
        for i in lo..hi {
            total += hist[keys[i] as usize] as u64;
        }
        let mut sum: u64 = 0;
        let mut cut = 0usize;
        while cut < hi - lo && 2 * (sum + hist[keys[lo + cut] as usize] as u64) < total {
            sum += hist[keys[lo + cut] as usize] as u64;
            cut += 1;
        }
        let cut = cut.clamp(1, hi - lo - 1);
        boxes.push((lo, lo + cut));
        boxes.push((lo + cut, hi));
        spans.push(box_span(keys, lo, lo + cut));
        spans.push(box_span(keys, lo + cut, hi));
    }

    let count = boxes.len();
    let mut pal_565 = [0u16; 256];
    let mut pal_rgb = [0i32; 768];
    for b in 0..count {
        let (lo, hi) = boxes[b];
        let mut wsum: u64 = 0;
        let mut r: f64 = 0.0;
        let mut g: f64 = 0.0;
        let mut bl: f64 = 0.0;
        for i in lo..hi {
            let key = keys[i];
            let wt = hist[key as usize] as u64;
            wsum += wt;
            r += channel(key, 0) as f64 * wt as f64;
            g += channel(key, 1) as f64 * wt as f64;
            bl += channel(key, 2) as f64 * wt as f64;
        }
        let ws = wsum as f64;
        let cr = (r / ws + 0.5).floor() as i32 & 255;
        let cg = (g / ws + 0.5).floor() as i32 & 255;
        let cb = (bl / ws + 0.5).floor() as i32 & 255;
        pal_rgb[b * 3] = cr;
        pal_rgb[b * 3 + 1] = cg;
        pal_rgb[b * 3 + 2] = cb;
        pal_565[b] = ((cr as u16 >> 3) << 11) | ((cg as u16 >> 2) << 5) | (cb as u16 >> 3);
    }
    (count, pal_565, pal_rgb)
}

fn lz4_len(dst: &mut Vec<u8>, mut n: usize) {
    while n >= 255 {
        dst.push(255);
        n -= 255;
    }
    dst.push(n as u8);
}

fn hash4(src: &[u8], i: usize) -> usize {
    let v = u32::from_le_bytes(src[i..i + 4].try_into().unwrap());
    (v.wrapping_mul(HASH_MUL) as usize) >> 16 & (HASH_SIZE - 1)
}

fn insert(head: &mut [i32], chain: &mut [i32], src: &[u8], pos: usize) {
    let h = hash4(src, pos);
    chain[pos & 0xffff] = head[h];
    head[h] = pos as i32;
}

thread_local! {
    static PAL_TABLES: RefCell<(Vec<u32>, Vec<u8>)> = RefCell::new((vec![0u32; HASH_SIZE], vec![0u8; HASH_SIZE]));
    static LZ4_TABLES: RefCell<(Vec<i32>, Vec<i32>)> = RefCell::new((vec![-1i32; HASH_SIZE], vec![-1i32; HASH_SIZE]));
}

fn lz4_block(src: &[u8], dst: &mut Vec<u8>) {
    LZ4_TABLES.with(|tables| {
        let (head, chain) = &mut *tables.borrow_mut();
        head.fill(-1);
        chain.fill(-1);
        lz4_compress(src, dst, head, chain);
    });
}

fn lz4_compress(src: &[u8], dst: &mut Vec<u8>, head: &mut [i32], chain: &mut [i32]) {
    let n = src.len();
    let match_limit = n.saturating_sub(5);
    let start_limit = n.saturating_sub(12);
    let mut anchor = 0usize;
    let mut i = 0usize;
    while n > 12 && i <= start_limit {
        let h = hash4(src, i);
        let mut best_len = 0usize;
        let mut best_ref = 0usize;
        let mut depth = MATCH_DEPTH;
        let mut at = head[h];
        while at >= 0 && i - at as usize <= 65535 && depth > 0 {
            let r = at as usize;
            if src[r + best_len] == src[i + best_len]
                && src[r] == src[i]
                && src[r + 1] == src[i + 1]
                && src[r + 2] == src[i + 2]
                && src[r + 3] == src[i + 3]
            {
                let mut len = 4usize;
                while i + len < match_limit && src[r + len] == src[i + len] {
                    len += 1;
                }
                if len > best_len {
                    best_len = len;
                    best_ref = r;
                }
            }
            depth -= 1;
            at = chain[r & 0xffff];
        }
        insert(head, chain, src, i);
        if best_len < 4 {
            i += 1;
            continue;
        }
        let mut j = i + 1;
        while j < i + best_len && j <= start_limit {
            insert(head, chain, src, j);
            j += 1;
        }
        let lit = i - anchor;
        dst.push((lit.min(15) as u8) << 4 | (best_len - 4).min(15) as u8);
        if lit >= 15 {
            lz4_len(dst, lit - 15);
        }
        dst.extend_from_slice(&src[anchor..i]);
        let offset = i - best_ref;
        dst.push((offset & 255) as u8);
        dst.push((offset >> 8) as u8);
        if best_len - 4 >= 15 {
            lz4_len(dst, best_len - 19);
        }
        i += best_len;
        anchor = i;
    }
    let tail = n - anchor;
    if tail > 0 {
        dst.push((tail.min(15) as u8) << 4);
        if tail >= 15 {
            lz4_len(dst, tail - 15);
        }
        dst.extend_from_slice(&src[anchor..n]);
    }
}
