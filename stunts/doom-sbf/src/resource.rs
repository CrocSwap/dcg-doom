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

#[derive(Debug)]
pub struct InitializedNoRenderSequence {
    pub states: Vec<Vec<u8>>,
    pub mirrors: Vec<Vec<u8>>,
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
    fn doom_resource_import_initialized_no_tic(
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
    fn doom_resource_import_initialized_no_render_sequence(
        wad_accounts: *const *const u8,
        wad_lens: *const usize,
        image: *const u8,
        image_len: usize,
        state: *const u8,
        state_len: usize,
        commands: *const u8,
        command_count: usize,
        states_out: *mut u8,
        state_stride: usize,
        state_lens_out: *mut usize,
        mirrors_out: *mut u8,
        mirror_stride: usize,
        graph_anchor: *mut usize,
    ) -> i32;
    fn doom_resource_test_reset_setup_counter();
    fn doom_resource_test_setup_counter() -> usize;
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

pub fn reset_full_setup_counter() {
    unsafe { doom_resource_test_reset_setup_counter() }
}

pub fn full_setup_counter() -> usize {
    unsafe { doom_resource_test_setup_counter() }
}

pub fn import_initialized_tic(
    wad_accounts: [&[u8]; 5],
    image: &[u8],
    state: &[u8],
    mirror: &[u8],
    controller: &[u8],
) -> Result<InitializedTic, i32> {
    import_initialized_mode(wad_accounts, image, state, mirror, controller, true)
}

/// Import the initialized RI2 image through the same source-matched native
/// engine, stopping before the tic/render boundary used by E2R/4.
pub fn import_initialized_no_tic(
    wad_accounts: [&[u8]; 5],
    image: &[u8],
    state: &[u8],
    mirror: &[u8],
    controller: &[u8],
) -> Result<InitializedTic, i32> {
    import_initialized_mode(wad_accounts, image, state, mirror, controller, false)
}

/// Run a real captured command sequence through one initialized native Doom
/// context.  This is the no-render oracle for DOOM-SIM/1: setup and state
/// reconstruction happen once, while each command performs exactly one
/// `G_Ticker` and emits ST1/MP1.
pub fn import_initialized_no_render_sequence(
    wad_accounts: [&[u8]; 5],
    image: &[u8],
    state: &[u8],
    commands: &[u8],
) -> Result<InitializedNoRenderSequence, i32> {
    if image.is_empty()
        || image.len() > IMAGE_MAX_BYTES
        || state.len() < 64
        || state.len() > STATE_MAX_BYTES
        || commands.is_empty()
        || commands.len() % 4 != 0
    {
        return Err(240);
    }
    let (pointers, lengths) = wad_ffi(wad_accounts);
    let count = commands.len() / 4;
    let mut states = vec![0u8; count * STATE_MAX_BYTES];
    let mut state_lens = vec![0usize; count];
    let mut mirrors = vec![0u8; count * MIRROR_BYTES];
    let mut graph_anchor = 0usize;
    let rc = unsafe {
        doom_resource_import_initialized_no_render_sequence(
            pointers.as_ptr(),
            lengths.as_ptr(),
            image.as_ptr(),
            image.len(),
            state.as_ptr(),
            state.len(),
            commands.as_ptr(),
            count,
            states.as_mut_ptr(),
            STATE_MAX_BYTES,
            state_lens.as_mut_ptr(),
            mirrors.as_mut_ptr(),
            MIRROR_BYTES,
            &mut graph_anchor,
        )
    };
    if rc != 0 {
        return Err(rc);
    }
    let mut state_vecs = Vec::with_capacity(count);
    let mut mirror_vecs = Vec::with_capacity(count);
    for index in 0..count {
        let len = state_lens[index];
        if !(64..=STATE_MAX_BYTES).contains(&len) {
            return Err(238);
        }
        state_vecs.push(states[index * STATE_MAX_BYTES..index * STATE_MAX_BYTES + len].to_vec());
        mirror_vecs.push(
            mirrors[index * MIRROR_BYTES..(index + 1) * MIRROR_BYTES].to_vec(),
        );
    }
    let _ = graph_anchor;
    Ok(InitializedNoRenderSequence {
        states: state_vecs,
        mirrors: mirror_vecs,
    })
}

fn import_initialized_mode(
    wad_accounts: [&[u8]; 5],
    image: &[u8],
    state: &[u8],
    mirror: &[u8],
    controller: &[u8],
    run_tic: bool,
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
        let import = if run_tic {
            doom_resource_import_initialized_tic
        } else {
            doom_resource_import_initialized_no_tic
        };
        import(
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

unsafe extern "C" {
    fn doom_resource_sim_session_open(
        wad_accounts: *const *const u8,
        wad_lens: *const usize,
        image: *const u8,
        image_len: usize,
        state: *const u8,
        state_len: usize,
    ) -> i32;
    fn doom_resource_sim_session_step(command: *const u8) -> i32;
    fn doom_resource_sim_session_publish(
        state_out: *mut u8,
        state_cap: usize,
        state_len_out: *mut usize,
        mirror_out: *mut u8,
        mirror_cap: usize,
    ) -> i32;
    fn doom_resource_sim_session_probe(out: *mut i64) -> i32;
    fn doom_resource_sim_session_close();
}

/// Engine fields the host SIM/2 oracle records per tic.  They are
/// diagnostics, not part of any commitment.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SessionProbe {
    pub gametic: i64,
    pub gamestate: i64,
    pub gameaction: i64,
    pub playerstate: i64,
    pub health: i64,
    pub demo_offset: i64,
}

/// A resident host Doom engine for the DOOM-SIM/2 oracle: one no-setup
/// RI2/2 import, then any number of `step` calls, with ST1/MP1 derived only
/// when `publish` is called.  The host engine is the process-global
/// `DOOM_CTX`, so at most one session exists at a time; `open` replaces any
/// previous session.
pub struct NativeSimSession {
    _not_send: std::marker::PhantomData<*const ()>,
}

impl NativeSimSession {
    pub fn open(wad_accounts: [&[u8]; 5], image: &[u8], state: &[u8]) -> Result<Self, i32> {
        if image.is_empty()
            || image.len() > IMAGE_MAX_BYTES
            || state.len() < 64
            || state.len() > STATE_MAX_BYTES
        {
            return Err(240);
        }
        let (pointers, lengths) = wad_ffi(wad_accounts);
        let rc = unsafe {
            doom_resource_sim_session_open(
                pointers.as_ptr(),
                lengths.as_ptr(),
                image.as_ptr(),
                image.len(),
                state.as_ptr(),
                state.len(),
            )
        };
        if rc != 0 {
            return Err(rc);
        }
        Ok(Self {
            _not_send: std::marker::PhantomData,
        })
    }

    /// Returns the raw SIM/2 step code: 0 ran, 1..=5 halted before
    /// `G_Ticker`, otherwise a refusal.
    pub fn step(&mut self, command: &[u8; 4]) -> i32 {
        unsafe { doom_resource_sim_session_step(command.as_ptr()) }
    }

    /// Writes ST1 (zero tail to `state.len()`) and MP1, returning the ST1
    /// length.
    pub fn publish(&mut self, state: &mut [u8], mirror: &mut [u8]) -> Result<usize, i32> {
        let mut state_len = 0usize;
        let rc = unsafe {
            doom_resource_sim_session_publish(
                state.as_mut_ptr(),
                state.len(),
                &mut state_len,
                mirror.as_mut_ptr(),
                mirror.len(),
            )
        };
        if rc != 0 {
            return Err(rc);
        }
        Ok(state_len)
    }

    pub fn probe(&self) -> SessionProbe {
        let mut out = [0i64; 6];
        unsafe { doom_resource_sim_session_probe(out.as_mut_ptr()) };
        SessionProbe {
            gametic: out[0],
            gamestate: out[1],
            gameaction: out[2],
            playerstate: out[3],
            health: out[4],
            demo_offset: out[5],
        }
    }
}

impl Drop for NativeSimSession {
    fn drop(&mut self) {
        unsafe { doom_resource_sim_session_close() };
    }
}
