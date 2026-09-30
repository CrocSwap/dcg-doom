// SPDX-License-Identifier: GPL-3.0-only

use dcg_program::kernel::{Kernel, KernelError, ManifestError, ManifestRunError, StatefulKernel};
use doom_dcg::{
    validate_manifest, DoomRenderStripKernel, DoomSimStepKernel, DoomSnapshotKernel,
    DOOM_DCG_APPLICATION_MANIFEST, MODE_CONSENSUS_V1, RENDER_STRIP, SIM_ENGINE_STATE_BUNDLE_BYTES,
    SIM_KERNEL_ID, SIM_STEP, SNAPSHOT, SNAPSHOT_ACCOUNT_BYTES, SNAPSHOT_KERNEL_ID,
    STRIP_ACCOUNT_BYTES, STRIP_KERNEL_ID,
};

#[test]
fn application_manifest_registers_the_three_doom_kernels_but_rejects_unbounded_compute() {
    assert_eq!(
        validate_manifest(),
        Err(ManifestError::InvalidComputeLimit(SIM_KERNEL_ID))
    );
    assert_eq!(
        DOOM_DCG_APPLICATION_MANIFEST.application_id,
        b"basanos-doom/1"
    );
    assert_eq!(DOOM_DCG_APPLICATION_MANIFEST.version, 1);
    assert_eq!(DOOM_DCG_APPLICATION_MANIFEST.kernels.len(), 3);

    for (id, kernel) in [
        (SIM_KERNEL_ID, &SIM_STEP as &dyn Kernel),
        (SNAPSHOT_KERNEL_ID, &SNAPSHOT as &dyn Kernel),
        (STRIP_KERNEL_ID, &RENDER_STRIP as &dyn Kernel),
    ] {
        let spec = kernel.manifest();
        assert_eq!(spec.id, id);
        assert_eq!(spec.semantic_version, 1);
        assert_eq!(spec.abi_version, 1);
        assert!(spec.modes.contains(&MODE_CONSENSUS_V1));
        assert!(!spec.state.is_none());
        assert!(DOOM_DCG_APPLICATION_MANIFEST.resolve(id, 1, 1).is_some());
        assert!(DOOM_DCG_APPLICATION_MANIFEST.supports_mode(id, 1, 1, MODE_CONSENSUS_V1));
    }

    assert_eq!(
        SIM_STEP.manifest().state.unwrap().max_bytes,
        SIM_ENGINE_STATE_BUNDLE_BYTES
    );
    assert_eq!(SNAPSHOT.manifest().output.max_bytes, SNAPSHOT_ACCOUNT_BYTES);
    assert_eq!(
        RENDER_STRIP.manifest().output.max_bytes,
        STRIP_ACCOUNT_BYTES
    );
}

#[test]
fn kernel_methods_fail_closed_until_the_svm_account_adapter_exists() {
    let mut output = vec![0; STRIP_ACCOUNT_BYTES as usize];
    let mut next_state = vec![0; SIM_ENGINE_STATE_BUNDLE_BYTES as usize];
    assert_eq!(
        SIM_STEP.initial_state(&mut next_state),
        Err(KernelError::Refused)
    );
    assert_eq!(
        SIM_STEP.transition(&[0; 8], &[], &mut output, &mut next_state),
        Err(KernelError::Refused)
    );
    assert_eq!(
        SNAPSHOT.transition(&[0; 56], &[], &mut output, &mut next_state),
        Err(KernelError::Refused)
    );
    assert_eq!(
        RENDER_STRIP.transition(&[0; 128], &[], &mut output, &mut next_state),
        Err(KernelError::Refused)
    );
    assert_eq!(
        DOOM_DCG_APPLICATION_MANIFEST.execute(
            SIM_KERNEL_ID,
            1,
            1,
            MODE_CONSENSUS_V1,
            &[0; 8],
            &mut output,
        ),
        Err(ManifestRunError::Kernel(KernelError::Refused))
    );
}

#[allow(dead_code)]
fn type_check_stateful_bounds() {
    let _: &dyn StatefulKernel = &DoomSimStepKernel;
    let _: &dyn StatefulKernel = &DoomSnapshotKernel;
    let _: &dyn StatefulKernel = &DoomRenderStripKernel;
}
