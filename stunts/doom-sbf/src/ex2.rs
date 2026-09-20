//! DOOM-EX2 host-native import/export envelope.
//!
//! EX2 is deliberately separate from the EX1 SBF instruction.  It binds the
//! retained ST1/MP1/CI2 payloads to an explicit image, ABI, and role map, then
//! lets the native C bridge perform one real engine entry.

#[cfg(feature = "ctx-engine")]
use sha2::{Digest, Sha256};

include!(concat!(env!("OUT_DIR"), "/doom_ex2_build_identity.rs"));

pub const MAGIC: &[u8; 8] = b"DOOM-EX2";
pub const VERSION: u32 = 2;
pub const HEADER_BYTES: usize = 256;
pub const SCHEMA: u32 = 2;
pub const OP_TIC: u32 = 2;
pub const ROLE_COUNT: usize = 12;
pub const ROLE_MAP_OFFSET: usize = 256;
pub const ROLE_MAP_BYTES: usize = 12;
pub const STATE_OFFSET: usize = 272;
pub const STATE_MAX_BYTES: usize = 126_976;
pub const MIRROR_BYTES: usize = 65_536;
pub const CONTROLLER_BYTES: usize = 64;
pub const FRAME_BYTES: usize = 320 * 200;

pub const ROLE_MAP: [u8; ROLE_MAP_BYTES] = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Envelope<'a> {
    pub state: &'a [u8],
    pub mirror: &'a [u8],
    pub controller: &'a [u8],
    pub frame: Option<&'a [u8]>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DecodeError {
    Short,
    Magic,
    Version,
    Header,
    Total,
    Operation,
    Schema,
    Identity,
    Roles,
    Reserved,
    Bounds,
    State,
    Mirror,
    Controller,
    Frame,
}

impl core::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "EX2 decode error: {self:?}")
    }
}

impl std::error::Error for DecodeError {}

impl<'a> Envelope<'a> {
    pub fn new(
        state: &'a [u8],
        mirror: &'a [u8],
        controller: &'a [u8],
    ) -> Result<Envelope<'a>, DecodeError> {
        let envelope = Envelope {
            state,
            mirror,
            controller,
            frame: None,
        };
        envelope.validate()
    }

    pub fn with_frame(self, frame: &'a [u8]) -> Result<Envelope<'a>, DecodeError> {
        let envelope = Envelope {
            state: self.state,
            mirror: self.mirror,
            controller: self.controller,
            frame: Some(frame),
        };
        envelope.validate()
    }

    fn validate(self) -> Result<Self, DecodeError> {
        if self.state.len() < 64
            || self.state.len() > STATE_MAX_BYTES
            || &self.state[..8] != b"DOOM-ST1"
            || u32_at(self.state, 8) != 0
            || u32_at(self.state, 12) != 64
            || u32_at(self.state, 16) as usize != self.state.len()
        {
            return Err(DecodeError::State);
        }
        if self.mirror.len() != MIRROR_BYTES
            || &self.mirror[..8] != b"DOOM-MP1"
            || u32_at(self.mirror, 8) != 1
            || u32_at(self.mirror, 12) != 4096
            || u32_at(self.mirror, 16) as usize != MIRROR_BYTES
        {
            return Err(DecodeError::Mirror);
        }
        if self.controller.len() != CONTROLLER_BYTES
            || &self.controller[..8] != b"DOOM-CI2"
            || u32_at(self.controller, 8) != 1
            || u32_at(self.controller, 12) != 28
            || u32_at(self.controller, 16) as usize != CONTROLLER_BYTES
            || u32_at(self.controller, 20) != u32_at(self.state, 20)
            || self.controller[28..].iter().any(|&b| b != 0)
        {
            return Err(DecodeError::Controller);
        }
        if let Some(frame) = self.frame {
            if frame.len() != FRAME_BYTES {
                return Err(DecodeError::Frame);
            }
        }
        Ok(self)
    }

    pub fn encoded_len(&self) -> usize {
        let mut cursor = align8(STATE_OFFSET + self.state.len());
        cursor = align8(cursor + self.mirror.len());
        cursor = align8(cursor + self.controller.len());
        if self.frame.is_some() {
            cursor = align8(cursor + FRAME_BYTES);
        }
        cursor
    }

    pub fn encode(&self) -> Result<Vec<u8>, DecodeError> {
        let envelope = self.validate()?;
        let total = envelope.encoded_len();
        let (state_offset, mirror_offset, controller_offset, frame_offset) = offsets(&envelope);
        let mut out = vec![0u8; total];
        out[..8].copy_from_slice(MAGIC);
        put32(&mut out, 8, VERSION);
        put32(&mut out, 12, HEADER_BYTES as u32);
        put32(&mut out, 16, total as u32);
        put32(&mut out, 20, OP_TIC);
        put32(&mut out, 24, SCHEMA);
        put32(&mut out, 28, ROLE_COUNT as u32);
        out[32..64].copy_from_slice(&IMAGE_ID);
        out[64..96].copy_from_slice(&ABI_ID);
        put32(&mut out, 96, state_offset as u32);
        put32(&mut out, 100, envelope.state.len() as u32);
        put32(&mut out, 104, mirror_offset as u32);
        put32(&mut out, 108, envelope.mirror.len() as u32);
        put32(&mut out, 112, controller_offset as u32);
        put32(&mut out, 116, envelope.controller.len() as u32);
        put32(&mut out, 120, frame_offset as u32);
        put32(
            &mut out,
            124,
            envelope.frame.map_or(0, |_| FRAME_BYTES as u32),
        );
        put32(&mut out, 128, ROLE_MAP_OFFSET as u32);
        put32(&mut out, 132, ROLE_MAP_BYTES as u32);
        out[ROLE_MAP_OFFSET..ROLE_MAP_OFFSET + ROLE_MAP_BYTES].copy_from_slice(&ROLE_MAP);
        out[state_offset..state_offset + envelope.state.len()].copy_from_slice(envelope.state);
        out[mirror_offset..mirror_offset + envelope.mirror.len()].copy_from_slice(envelope.mirror);
        out[controller_offset..controller_offset + envelope.controller.len()]
            .copy_from_slice(envelope.controller);
        if let Some(frame) = envelope.frame {
            out[frame_offset..frame_offset + frame.len()].copy_from_slice(frame);
        }
        Ok(out)
    }
}

pub fn decode(bytes: &[u8]) -> Result<Envelope<'_>, DecodeError> {
    if bytes.len() < STATE_OFFSET || &bytes[..8] != MAGIC {
        return Err(if bytes.len() < STATE_OFFSET {
            DecodeError::Short
        } else {
            DecodeError::Magic
        });
    }
    if u32_at(bytes, 8) != VERSION {
        return Err(DecodeError::Version);
    }
    if u32_at(bytes, 12) as usize != HEADER_BYTES || u32_at(bytes, 28) as usize != ROLE_COUNT {
        return Err(DecodeError::Header);
    }
    let total = u32_at(bytes, 16) as usize;
    if total != bytes.len() {
        return Err(DecodeError::Total);
    }
    if u32_at(bytes, 20) != OP_TIC {
        return Err(DecodeError::Operation);
    }
    if u32_at(bytes, 24) != SCHEMA {
        return Err(DecodeError::Schema);
    }
    if bytes[32..64] != IMAGE_ID || bytes[64..96] != ABI_ID {
        return Err(DecodeError::Identity);
    }
    if bytes[136..256].iter().any(|&b| b != 0)
        || u32_at(bytes, 128) as usize != ROLE_MAP_OFFSET
        || u32_at(bytes, 132) as usize != ROLE_MAP_BYTES
        || bytes[ROLE_MAP_OFFSET..ROLE_MAP_OFFSET + ROLE_MAP_BYTES] != ROLE_MAP
    {
        return Err(if bytes[136..256].iter().any(|&b| b != 0) {
            DecodeError::Reserved
        } else {
            DecodeError::Roles
        });
    }
    let state = range(
        bytes,
        u32_at(bytes, 96) as usize,
        u32_at(bytes, 100) as usize,
    )?;
    let mirror = range(
        bytes,
        u32_at(bytes, 104) as usize,
        u32_at(bytes, 108) as usize,
    )?;
    let controller = range(
        bytes,
        u32_at(bytes, 112) as usize,
        u32_at(bytes, 116) as usize,
    )?;
    let frame_len = u32_at(bytes, 124) as usize;
    let frame = if frame_len == 0 {
        if u32_at(bytes, 120) != 0 {
            return Err(DecodeError::Frame);
        }
        None
    } else {
        Some(range(bytes, u32_at(bytes, 120) as usize, frame_len)?)
    };
    let envelope = Envelope {
        state,
        mirror,
        controller,
        frame,
    };
    validate_ranges(bytes, &envelope)?;
    envelope.validate()
}

fn validate_ranges(bytes: &[u8], envelope: &Envelope<'_>) -> Result<(), DecodeError> {
    let (state_offset, mirror_offset, controller_offset, frame_offset) = offsets_from_header(bytes);
    let mut ranges = [
        (ROLE_MAP_OFFSET, ROLE_MAP_OFFSET + ROLE_MAP_BYTES),
        (state_offset, state_offset + envelope.state.len()),
        (mirror_offset, mirror_offset + envelope.mirror.len()),
        (
            controller_offset,
            controller_offset + envelope.controller.len(),
        ),
        (0, 0),
    ];
    let count = if let Some(frame) = envelope.frame {
        if frame.len() != FRAME_BYTES {
            return Err(DecodeError::Frame);
        }
        if frame_offset == 0 {
            return Err(DecodeError::Frame);
        }
        ranges[4] = (frame_offset, frame_offset + frame.len());
        5
    } else {
        4
    };
    for &(start, end) in &ranges[..count] {
        if start < HEADER_BYTES
            || end < start
            || end > bytes.len()
            || start % 8 != 0 && start != ROLE_MAP_OFFSET
        {
            return Err(DecodeError::Bounds);
        }
    }
    for i in 0..count {
        for j in i + 1..count {
            if ranges[i].0 < ranges[j].1 && ranges[j].0 < ranges[i].1 {
                return Err(DecodeError::Bounds);
            }
        }
    }
    Ok(())
}

fn offsets(envelope: &Envelope<'_>) -> (usize, usize, usize, usize) {
    let state = STATE_OFFSET;
    let mirror = align8(state + envelope.state.len());
    let controller = align8(mirror + envelope.mirror.len());
    let frame = if envelope.frame.is_some() {
        align8(controller + envelope.controller.len())
    } else {
        0
    };
    (state, mirror, controller, frame)
}

fn offsets_from_header(bytes: &[u8]) -> (usize, usize, usize, usize) {
    (
        u32_at(bytes, 96) as usize,
        u32_at(bytes, 104) as usize,
        u32_at(bytes, 112) as usize,
        u32_at(bytes, 120) as usize,
    )
}

fn range(bytes: &[u8], start: usize, len: usize) -> Result<&[u8], DecodeError> {
    let end = start.checked_add(len).ok_or(DecodeError::Bounds)?;
    if start < HEADER_BYTES || end > bytes.len() {
        return Err(DecodeError::Bounds);
    }
    Ok(&bytes[start..end])
}

fn align8(value: usize) -> usize {
    (value + 7) & !7
}

fn u32_at(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
}

fn put32(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

#[cfg(feature = "ctx-engine")]
unsafe extern "C" {
    fn doom_engine_ex2_entry(
        wad_accounts: *const *const u8,
        wad_lens: *const usize,
        state: *const u8,
        state_len: usize,
        mirror: *const u8,
        mirror_len: usize,
        controller: *const u8,
        controller_len: usize,
        state_out: *mut u8,
        state_cap: usize,
        state_out_len: *mut usize,
        mirror_out: *mut u8,
        mirror_cap: usize,
        mirror_out_len: *mut usize,
        frame_out: *mut u8,
        frame_cap: usize,
        frame_out_len: *mut usize,
    ) -> i32;
}

#[cfg(feature = "ctx-engine")]
#[derive(Debug)]
pub struct NativeOutput {
    pub state: Vec<u8>,
    pub mirror: Vec<u8>,
    pub frame: Vec<u8>,
}

#[cfg(feature = "ctx-engine")]
pub fn native_entry(
    envelope: &Envelope<'_>,
    wad_accounts: [&[u8]; 5],
) -> Result<NativeOutput, i32> {
    envelope.validate().map_err(|_| 232)?;
    let wad_ptrs = wad_accounts.map(|account| account.as_ptr());
    let wad_lens = wad_accounts.map(|account| account.len());
    let mut state = vec![0xa5; STATE_MAX_BYTES];
    let mut mirror = vec![0xa5; MIRROR_BYTES];
    let mut frame = vec![0xa5; FRAME_BYTES];
    let mut state_len = 0usize;
    let mut mirror_len = 0usize;
    let mut frame_len = 0usize;
    let rc = unsafe {
        doom_engine_ex2_entry(
            wad_ptrs.as_ptr(),
            wad_lens.as_ptr(),
            envelope.state.as_ptr(),
            envelope.state.len(),
            envelope.mirror.as_ptr(),
            envelope.mirror.len(),
            envelope.controller.as_ptr(),
            envelope.controller.len(),
            state.as_mut_ptr(),
            state.len(),
            &mut state_len,
            mirror.as_mut_ptr(),
            mirror.len(),
            &mut mirror_len,
            frame.as_mut_ptr(),
            frame.len(),
            &mut frame_len,
        )
    };
    if rc != 0 {
        if state.iter().any(|&b| b != 0xa5)
            || mirror.iter().any(|&b| b != 0xa5)
            || frame.iter().any(|&b| b != 0xa5)
        {
            return Err(239);
        }
        return Err(rc);
    }
    if state_len > state.len() || mirror_len != MIRROR_BYTES || frame_len != FRAME_BYTES {
        return Err(238);
    }
    state.truncate(state_len);
    mirror.truncate(mirror_len);
    frame.truncate(frame_len);
    Ok(NativeOutput {
        state,
        mirror,
        frame,
    })
}

#[cfg(feature = "ctx-engine")]
pub fn native_state_hash(wad_hash: &[u8], state: &[u8]) -> Result<String, DecodeError> {
    if wad_hash.len() != 32 || state.is_empty() {
        return Err(DecodeError::Bounds);
    }
    let version = 0u32.to_le_bytes();
    Ok(hex_digest(&[b"DOOM-ST1", &version, wad_hash, state]))
}

#[cfg(feature = "ctx-engine")]
pub fn native_frame_hash(
    wad_hash: &[u8],
    gametic: u32,
    frame: &[u8],
) -> Result<String, DecodeError> {
    if wad_hash.len() != 32 || frame.len() != FRAME_BYTES {
        return Err(DecodeError::Bounds);
    }
    let version = 0u32.to_le_bytes();
    let tic = gametic.to_le_bytes();
    let strip = 0u32.to_le_bytes();
    Ok(hex_digest(&[
        b"DOOM-FB1-FRAME",
        &version,
        wad_hash,
        &tic,
        &strip,
        frame,
    ]))
}

#[cfg(feature = "ctx-engine")]
fn hex_digest(parts: &[&[u8]]) -> String {
    let mut hasher = Sha256::new();
    for part in parts {
        hasher.update(part);
    }
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (Vec<u8>, Vec<u8>, Vec<u8>) {
        let mut state = vec![0u8; 64];
        state[..8].copy_from_slice(b"DOOM-ST1");
        state[16..20].copy_from_slice(&64u32.to_le_bytes());
        state[12..16].copy_from_slice(&64u32.to_le_bytes());
        let mut mirror = vec![0u8; MIRROR_BYTES];
        mirror[..8].copy_from_slice(b"DOOM-MP1");
        mirror[8..12].copy_from_slice(&1u32.to_le_bytes());
        mirror[12..16].copy_from_slice(&4096u32.to_le_bytes());
        mirror[16..20].copy_from_slice(&(MIRROR_BYTES as u32).to_le_bytes());
        let mut controller = vec![0u8; CONTROLLER_BYTES];
        controller[..8].copy_from_slice(b"DOOM-CI2");
        controller[8..12].copy_from_slice(&1u32.to_le_bytes());
        controller[12..16].copy_from_slice(&28u32.to_le_bytes());
        controller[16..20].copy_from_slice(&(CONTROLLER_BYTES as u32).to_le_bytes());
        (state, mirror, controller)
    }

    #[test]
    fn canonical_roundtrip_and_explicit_frame() {
        let (state, mirror, controller) = fixture();
        let envelope = Envelope::new(&state, &mirror, &controller).unwrap();
        let bytes = envelope
            .with_frame(&[0u8; FRAME_BYTES])
            .unwrap()
            .encode()
            .unwrap();
        let decoded = decode(&bytes).unwrap();
        assert_eq!(decoded.state, state);
        assert_eq!(decoded.mirror, mirror);
        assert_eq!(decoded.controller, controller);
        assert_eq!(decoded.frame.unwrap().len(), FRAME_BYTES);
        assert_eq!(u32_at(&bytes, 96) as usize, STATE_OFFSET);
    }

    #[test]
    fn rejects_identity_roles_and_truncated_ranges() {
        let (state, mirror, controller) = fixture();
        let base = Envelope::new(&state, &mirror, &controller)
            .unwrap()
            .encode()
            .unwrap();
        let mut bad = base.clone();
        bad[32] ^= 1;
        assert_eq!(decode(&bad), Err(DecodeError::Identity));
        let mut bad = base.clone();
        bad[256] ^= 1;
        assert_eq!(decode(&bad), Err(DecodeError::Roles));
        let mut bad = base;
        bad.truncate(bad.len() - 1);
        assert!(matches!(
            decode(&bad),
            Err(DecodeError::Total | DecodeError::Bounds)
        ));
    }
}
