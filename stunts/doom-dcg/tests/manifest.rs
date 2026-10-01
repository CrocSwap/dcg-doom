// SPDX-License-Identifier: GPL-3.0-only

use dcg_program::kernel::{Kernel, KernelError, StatefulKernel};
use doom_dcg::{
    validate_manifest, DoomKernel, DoomResourceBundle, DOOM_CONTEXT_ADDRESS,
    DOOM_DCG_APPLICATION_MANIFEST, DOOM_KERNEL_ID, ENGINE_CONTEXT_BYTES, MODE_CONSENSUS_V3,
    MP1_ACCOUNT_BYTES, RESOURCE_BYTES, RESOURCE_KEY, RESOURCE_SCHEMA, SNAPSHOT_BYTES,
    ST1_ACCOUNT_BYTES, STATE_BYTES, STATE_SCHEMA, STRIP_BYTES, V3_INITIALIZATION_PHASE_BYTES,
    V3_INITIALIZATION_PHASE_COMPUTE_UNITS, V3_VIEW_PHASE_BYTES, V3_VIEW_PHASE_COMPUTE_CEILING,
    V3_VIEW_WORKSPACE_BYTES,
};

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
fn doom_opts_into_mutable_v3_views_and_declares_bounded_phases() {
    let kernel = DoomKernel::default();
    assert!(kernel.view_requires_mutable_state());
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
