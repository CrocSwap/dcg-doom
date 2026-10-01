// SPDX-License-Identifier: GPL-3.0-only

use dcg_program::kernel::{Kernel, KernelError, StatefulKernel};
use doom_dcg::{
    validate_manifest, DoomKernel, DoomResourceBundle, DOOM_CONTEXT_ADDRESS,
    DOOM_DCG_APPLICATION_MANIFEST, DOOM_KERNEL_ID, ENGINE_CONTEXT_BYTES, MODE_CONSENSUS_V3,
    MP1_ACCOUNT_BYTES, RESOURCE_BYTES, RESOURCE_KEY, RESOURCE_SCHEMA, SNAPSHOT_BYTES,
    ST1_ACCOUNT_BYTES, STATE_BYTES, STATE_SCHEMA, STRIP_BYTES, V3_INITIALIZATION_PHASE_BYTES,
    V3_INITIALIZATION_PHASE_COMPUTE_UNITS, V3_SNAPSHOT_CACHE_BYTES, V3_SNAPSHOT_DATA_HEADER_BYTES,
    V3_SNAPSHOT_HEADER_BYTES, V3_SNAPSHOT_MAX_MOBJ_BYTES, V3_SNAPSHOT_MAX_MOBJ_COUNT,
    V3_SNAPSHOT_MOBJ_BYTES, V3_SNAPSHOT_PSPRITE_BYTES, V3_SNAPSHOT_PSPRITE_COUNT,
    V3_SNAPSHOT_SECTOR_BYTES, V3_SNAPSHOT_SIDE_BYTES, V3_SNAPSHOT_TRANSLATION_BYTES,
    V3_VIEW_PHASE_BYTES, V3_VIEW_PHASE_COMPUTE_CEILING, V3_VIEW_WORKSPACE_BYTES,
};

const RSN3_ENCODER_SOURCE: &str = include_str!("../../doom-sbf/src/doom_sbf_resource_ri2.c");

#[test]
fn application_manifest_pins_one_composite_v3_doom_kernel() {
    assert_eq!(validate_manifest(), Ok(()));
    assert_eq!(
        DOOM_DCG_APPLICATION_MANIFEST.application_id,
        b"basanos-doom/3"
    );
    assert_eq!(DOOM_DCG_APPLICATION_MANIFEST.version, 3);
    assert_eq!(DOOM_DCG_APPLICATION_MANIFEST.kernels.len(), 1);

    let kernel = DOOM_DCG_APPLICATION_MANIFEST.kernels[0];
    let spec = kernel.manifest();
    assert_eq!(spec.id, DOOM_KERNEL_ID);
    assert_eq!(spec.semantic_version, 3);
    assert_eq!(spec.abi_version, 1);
    assert_eq!(spec.modes, &[MODE_CONSENSUS_V3]);
    assert_eq!(spec.input.max_bytes, 4);
    assert_eq!(spec.resources.max_operations, 1);
    assert_eq!(spec.state.unwrap().id, STATE_SCHEMA);
    assert_eq!(spec.state.unwrap().max_bytes, STATE_BYTES);

    assert_eq!(
        ENGINE_CONTEXT_BYTES + ST1_ACCOUNT_BYTES + MP1_ACCOUNT_BYTES,
        STATE_BYTES
    );
    assert_eq!(SNAPSHOT_BYTES, 1_048_768);
    assert_eq!(STRIP_BYTES, 8_128);
    assert_eq!(RESOURCE_BYTES, 4_457_108);
    assert_eq!(RESOURCE_KEY, [0xd0; 32]);
    assert_eq!(RESOURCE_SCHEMA.version, 2);
    assert_eq!(DOOM_CONTEXT_ADDRESS, 0x400000060);
    assert_eq!(V3_INITIALIZATION_PHASE_BYTES, 16_384);
    assert_eq!(V3_INITIALIZATION_PHASE_COMPUTE_UNITS, 1_400_000);
    assert_eq!(V3_VIEW_PHASE_BYTES, STRIP_BYTES);
    assert_eq!(V3_VIEW_PHASE_COMPUTE_CEILING, 1_400_000);
    assert!(V3_VIEW_WORKSPACE_BYTES > SNAPSHOT_BYTES + 8 * STRIP_BYTES);
}

#[test]
fn maximum_doom_snapshot_dynamic_payload_fits_workspace_cache() {
    assert_eq!(V3_SNAPSHOT_HEADER_BYTES, 192);
    assert_eq!(V3_SNAPSHOT_DATA_HEADER_BYTES, 128);
    assert_eq!(V3_SNAPSHOT_PSPRITE_COUNT, 2);
    assert_eq!(V3_SNAPSHOT_PSPRITE_BYTES, 16);
    assert_eq!(V3_SNAPSHOT_SECTOR_BYTES, 16);
    assert_eq!(V3_SNAPSHOT_SIDE_BYTES, 16);
    assert_eq!(V3_SNAPSHOT_TRANSLATION_BYTES, 4);
    assert_eq!(V3_SNAPSHOT_MAX_MOBJ_COUNT, 1_024);
    assert_eq!(V3_SNAPSHOT_MOBJ_BYTES, 52);
    assert_eq!(V3_SNAPSHOT_MAX_MOBJ_BYTES, 53_248);
    assert!(
        V3_SNAPSHOT_HEADER_BYTES
            + V3_SNAPSHOT_DATA_HEADER_BYTES
            + V3_SNAPSHOT_PSPRITE_COUNT * V3_SNAPSHOT_PSPRITE_BYTES
            + V3_SNAPSHOT_MAX_MOBJ_BYTES
            <= V3_SNAPSHOT_CACHE_BYTES
    );
}

#[test]
fn snapshot_projection_bound_matches_encoder_count_guards() {
    for guard in [
        "numsectors > 8192",
        "numsides > 32768",
        "numtextures > 8191 || numflats > 8191",
        "if (++count > 1024u) return -1;",
        "#define RSN3_DATA_HEADER_BYTES 128u",
        "#define RSN3_SECTOR_BYTES 16u",
        "#define RSN3_SIDE_BYTES 16u",
        "#define RSN3_MOBJ_BYTES 52u",
        "#define RSN3_PSPRITE_BYTES 16u",
        "(size_t)NUMPSPRITES * RSN3_PSPRITE_BYTES",
    ] {
        assert!(
            RSN3_ENCODER_SOURCE.contains(guard),
            "snapshot-bound assumption missing from the C encoder: {guard}"
        );
    }
}

#[test]
fn doom_keeps_v3_state_read_only_and_declares_bounded_workspace_phases() {
    let kernel = DoomKernel::default();
    assert!(kernel.view_workspace_at_account_base());
    assert!(!kernel.clear_view_workspace_on_begin());
    assert_eq!(kernel.max_view_phase_bytes(), V3_VIEW_PHASE_BYTES);
    assert_eq!(
        kernel.view_phase_compute_units(),
        V3_VIEW_PHASE_COMPUTE_CEILING
    );
    assert_eq!(
        kernel.max_initialization_phase_bytes(),
        V3_INITIALIZATION_PHASE_BYTES
    );
    assert_eq!(
        kernel.initialization_phase_compute_units(),
        V3_INITIALIZATION_PHASE_COMPUTE_UNITS
    );
    assert_eq!(kernel.execute(&[], &mut []), Err(KernelError::Refused));
}

#[test]
fn resource_bundle_rejects_wrong_shape_and_header() {
    assert_eq!(
        DoomResourceBundle::decode(&[]).err(),
        Some(KernelError::InvalidInput)
    );
    let mut bytes = vec![0; RESOURCE_BYTES];
    bytes[..8].copy_from_slice(b"WRONG000");
    assert_eq!(
        DoomResourceBundle::decode(&bytes).err(),
        Some(KernelError::InvalidInput)
    );
}
