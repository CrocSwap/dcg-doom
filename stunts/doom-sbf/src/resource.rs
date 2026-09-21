//! Host-only DOOM-RI1/RI2 resource-image bridge.
//!
//! This module is deliberately separate from the retained DOOM-E2S/1 entry.
//! It exposes the experimental native encoder/importer to the bounded driver;
//! no SBF instruction or public account ABI consumes these values.

pub const IMAGE_MAX_BYTES: usize = 16 * 1024 * 1024;
pub const STATE_MAX_BYTES: usize = 126_976;
pub const MIRROR_BYTES: usize = 65_536;
pub const FRAME_BYTES: usize = 320 * 200;

#[derive(Debug)]
pub struct EncodedImage {
    pub bytes: Vec<u8>,
    pub graph_anchor: usize,
}

#[derive(Debug)]
pub struct ImportedTic {
    pub state: Vec<u8>,
    pub mirror: Vec<u8>,
    pub frame: Vec<u8>,
    pub canonical_image: Vec<u8>,
    pub graph_anchor: usize,
}

#[derive(Debug)]
pub struct InitializedImage {
    pub bytes: Vec<u8>,
    pub graph_anchor: usize,
}

#[derive(Debug)]
pub struct InitializedTic {
    pub state: Vec<u8>,
    pub mirror: Vec<u8>,
    pub frame: Vec<u8>,
    pub graph_anchor: usize,
}

unsafe extern "C" {
    fn doom_resource_encode(
        wad_accounts: *const *const u8,
        wad_lens: *const usize,
        out: *mut u8,
        out_cap: usize,
        out_len: *mut usize,
        graph_anchor: *mut usize,
    ) -> i32;
    fn doom_resource_import_tic(
        wad_accounts: *const *const u8,
        wad_lens: *const usize,
        image: *const u8,
        image_len: usize,
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
        image_out: *mut u8,
        image_cap: usize,
        image_out_len: *mut usize,
        graph_anchor: *mut usize,
        mutation_count: *mut usize,
    ) -> i32;
    fn doom_resource_encode_initialized(
        wad_accounts: *const *const u8,
        wad_lens: *const usize,
        out: *mut u8,
        out_cap: usize,
        out_len: *mut usize,
        graph_anchor: *mut usize,
    ) -> i32;
    fn doom_resource_import_initialized_tic(
        wad_accounts: *const *const u8,
        wad_lens: *const usize,
        image: *const u8,
        image_len: usize,
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
        graph_anchor: *mut usize,
    ) -> i32;
}

fn wad_ffi(wad_accounts: [&[u8]; 5]) -> ([*const u8; 5], [usize; 5]) {
    (
        wad_accounts.map(|account| account.as_ptr()),
        wad_accounts.map(|account| account.len()),
    )
}

pub fn encode(wad_accounts: [&[u8]; 5]) -> Result<EncodedImage, i32> {
    let (pointers, lengths) = wad_ffi(wad_accounts);
    let mut bytes = vec![0xa5; IMAGE_MAX_BYTES];
    let mut image_len = 0usize;
    let mut graph_anchor = 0usize;
    let rc = unsafe {
        doom_resource_encode(
            pointers.as_ptr(),
            lengths.as_ptr(),
            bytes.as_mut_ptr(),
            bytes.len(),
            &mut image_len,
            &mut graph_anchor,
        )
    };
    if rc != 0 {
        return Err(rc);
    }
    if image_len == 0 || image_len > bytes.len() || bytes[image_len..].iter().any(|&b| b != 0xa5) {
        return Err(238);
    }
    bytes.truncate(image_len);
    Ok(EncodedImage {
        bytes,
        graph_anchor,
    })
}

pub fn import_tic(
    wad_accounts: [&[u8]; 5],
    image: &[u8],
    state: &[u8],
    mirror: &[u8],
    controller: &[u8],
) -> Result<ImportedTic, i32> {
    if image.is_empty() || image.len() > IMAGE_MAX_BYTES {
        return Err(240);
    }
    let (pointers, lengths) = wad_ffi(wad_accounts);
    let mut state_out = vec![0xa5; STATE_MAX_BYTES];
    let mut mirror_out = vec![0xa5; MIRROR_BYTES];
    let mut frame_out = vec![0xa5; FRAME_BYTES];
    let mut image_out = vec![0xa5; image.len()];
    let mut state_len = 0usize;
    let mut mirror_len = 0usize;
    let mut frame_len = 0usize;
    let mut image_len = 0usize;
    let mut graph_anchor = 0usize;
    let mut mutation_count = 0usize;
    let rc = unsafe {
        doom_resource_import_tic(
            pointers.as_ptr(),
            lengths.as_ptr(),
            image.as_ptr(),
            image.len(),
            state.as_ptr(),
            state.len(),
            mirror.as_ptr(),
            mirror.len(),
            controller.as_ptr(),
            controller.len(),
            state_out.as_mut_ptr(),
            state_out.len(),
            &mut state_len,
            mirror_out.as_mut_ptr(),
            mirror_out.len(),
            &mut mirror_len,
            frame_out.as_mut_ptr(),
            frame_out.len(),
            &mut frame_len,
            image_out.as_mut_ptr(),
            image_out.len(),
            &mut image_len,
            &mut graph_anchor,
            &mut mutation_count,
        )
    };
    if rc != 0 {
        if mutation_count != 0
            || state_out.iter().any(|&b| b != 0xa5)
            || mirror_out.iter().any(|&b| b != 0xa5)
            || frame_out.iter().any(|&b| b != 0xa5)
            || image_out.iter().any(|&b| b != 0xa5)
        {
            return Err(239);
        }
        return Err(rc);
    }
    if state_len == 0
        || state_len > state_out.len()
        || mirror_len != MIRROR_BYTES
        || frame_len != FRAME_BYTES
        || image_len != image.len()
    {
        return Err(238);
    }
    state_out.truncate(state_len);
    mirror_out.truncate(mirror_len);
    frame_out.truncate(frame_len);
    image_out.truncate(image_len);
    Ok(ImportedTic {
        state: state_out,
        mirror: mirror_out,
        frame: frame_out,
        canonical_image: image_out,
        graph_anchor,
    })
}

pub fn encode_initialized(wad_accounts: [&[u8]; 5]) -> Result<InitializedImage, i32> {
    let (pointers, lengths) = wad_ffi(wad_accounts);
    let mut bytes = vec![0xa5; IMAGE_MAX_BYTES];
    let mut image_len = 0usize;
    let mut graph_anchor = 0usize;
    let rc = unsafe {
        doom_resource_encode_initialized(
            pointers.as_ptr(),
            lengths.as_ptr(),
            bytes.as_mut_ptr(),
            bytes.len(),
            &mut image_len,
            &mut graph_anchor,
        )
    };
    if rc != 0 {
        return Err(rc);
    }
    if image_len == 0 || image_len > bytes.len() || bytes[image_len..].iter().any(|&b| b != 0xa5) {
        return Err(238);
    }
    bytes.truncate(image_len);
    Ok(InitializedImage {
        bytes,
        graph_anchor,
    })
}

pub fn import_initialized_tic(
    wad_accounts: [&[u8]; 5],
    image: &[u8],
    state: &[u8],
    mirror: &[u8],
    controller: &[u8],
) -> Result<InitializedTic, i32> {
    if image.is_empty() || image.len() > IMAGE_MAX_BYTES {
        return Err(240);
    }
    let (pointers, lengths) = wad_ffi(wad_accounts);
    let mut state_out = vec![0xa5; STATE_MAX_BYTES];
    let mut mirror_out = vec![0xa5; MIRROR_BYTES];
    let mut frame_out = vec![0xa5; FRAME_BYTES];
    let mut state_len = 0usize;
    let mut mirror_len = 0usize;
    let mut frame_len = 0usize;
    let mut graph_anchor = 0usize;
    let rc = unsafe {
        doom_resource_import_initialized_tic(
            pointers.as_ptr(),
            lengths.as_ptr(),
            image.as_ptr(),
            image.len(),
            state.as_ptr(),
            state.len(),
            mirror.as_ptr(),
            mirror.len(),
            controller.as_ptr(),
            controller.len(),
            state_out.as_mut_ptr(),
            state_out.len(),
            &mut state_len,
            mirror_out.as_mut_ptr(),
            mirror_out.len(),
            &mut mirror_len,
            frame_out.as_mut_ptr(),
            frame_out.len(),
            &mut frame_len,
            &mut graph_anchor,
        )
    };
    if rc != 0 {
        if state_out.iter().any(|&b| b != 0xa5)
            || mirror_out.iter().any(|&b| b != 0xa5)
            || frame_out.iter().any(|&b| b != 0xa5)
        {
            return Err(239);
        }
        return Err(rc);
    }
    if state_len == 0
        || state_len > state_out.len()
        || mirror_len != MIRROR_BYTES
        || frame_len != FRAME_BYTES
    {
        return Err(238);
    }
    state_out.truncate(state_len);
    mirror_out.truncate(mirror_len);
    frame_out.truncate(frame_len);
    Ok(InitializedTic {
        state: state_out,
        mirror: mirror_out,
        frame: frame_out,
        graph_anchor,
    })
}
