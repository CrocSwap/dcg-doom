// SPDX-License-Identifier: GPL-3.0-only

use dcg_program::kernel::{KernelError, StatefulKernel};
use doom_dcg::{
    validate_manifest, DoomKernel, DoomResourceBundle, DOOM_DCG_APPLICATION_MANIFEST,
    DOOM_KERNEL_ID, ENGINE_CONTEXT_BYTES, MODE_CONSENSUS_V2, MP1_ACCOUNT_BYTES, RESOURCE_BYTES,
    RESOURCE_KEY, RESOURCE_SCHEMA, SNAPSHOT_BYTES, ST1_ACCOUNT_BYTES, STATE_BYTES, STATE_SCHEMA,
    STRIP_BYTES,
};

#[test]
fn application_manifest_pins_one_composite_v2_doom_kernel() {
    assert_eq!(validate_manifest(), Ok(()));
    assert_eq!(
        DOOM_DCG_APPLICATION_MANIFEST.application_id,
        b"basanos-doom/2"
    );
    assert_eq!(DOOM_DCG_APPLICATION_MANIFEST.version, 2);
    assert_eq!(DOOM_DCG_APPLICATION_MANIFEST.kernels.len(), 1);

    let kernel = DOOM_DCG_APPLICATION_MANIFEST.kernels[0];
    let spec = kernel.manifest();
    assert_eq!(spec.id, DOOM_KERNEL_ID);
    assert_eq!(spec.semantic_version, 2);
    assert_eq!(spec.abi_version, 1);
    assert_eq!(spec.modes, &[MODE_CONSENSUS_V2]);
    assert_eq!(spec.input.max_bytes, 4);
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
    assert_eq!(RESOURCE_SCHEMA.version, 1);
}

#[test]
fn the_local_engine_binding_shim_refuses_the_v2_state_address() {
    let kernel = DoomKernel::default();
    let mut context = vec![0u8; ENGINE_CONTEXT_BYTES as usize];
    let mut st1 = vec![0u8; ST1_ACCOUNT_BYTES as usize];
    let mut mp1 = vec![0u8; MP1_ACCOUNT_BYTES as usize];
    let mut spans = [
        dcg_program::kernel::StateSpanMut {
            key: [1; 32],
            owner: [2; 32],
            schema: STATE_SCHEMA,
            offset: 0,
            data: context.as_mut_slice(),
        },
        dcg_program::kernel::StateSpanMut {
            key: [3; 32],
            owner: [2; 32],
            schema: STATE_SCHEMA,
            offset: ENGINE_CONTEXT_BYTES,
            data: st1.as_mut_slice(),
        },
        dcg_program::kernel::StateSpanMut {
            key: [4; 32],
            owner: [2; 32],
            schema: STATE_SCHEMA,
            offset: ENGINE_CONTEXT_BYTES + ST1_ACCOUNT_BYTES,
            data: mp1.as_mut_slice(),
        },
    ];
    kernel.bind_invocation_state(&mut spans).unwrap();
    let mut output = [0u8; 4];
    assert_eq!(
        kernel.transition_spans(&[0, 0, 0, 0], &mut spans, &mut output),
        Err(KernelError::Refused)
    );
    kernel.unbind_invocation_state();
    assert_eq!(
        kernel.transition_spans(&[0, 0, 0, 0], &mut spans, &mut output),
        Err(KernelError::Refused)
    );
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
