#![cfg(feature = "sbf-programtest")]
#![recursion_limit = "256"]

//! Full composed SBF lifecycle proof for DCG stateful v3. This runs the real
//! engine, checks every retained SIM/2 sweep root, and renders all RSN/3 views.

use dcg_program::{hash::sha256, kernel::Kernel, stateful as sw, stateful::v3};
use doom_dcg::{
    DoomKernel, DOOM_KERNEL_ID, DOOM_RESOURCE_COMMITMENT, ENGINE_CONTEXT_BYTES,
    MODE_CONSENSUS_V3, MP1_ACCOUNT_BYTES,
    RESOURCE_BYTES, RESOURCE_KEY, RESOURCE_SCHEMA, SNAPSHOT_BYTES, ST1_ACCOUNT_BYTES, STATE_SCHEMA,
    STRIP_BYTES, V3_INITIALIZATION_PHASE_COMPUTE_UNITS, V3_SNAPSHOT_ABI, V3_STRIP_ABIS,
    V3_VIEW_PHASE_BYTES, V3_VIEW_PHASE_COMPUTE_CEILING, V3_VIEW_WORKSPACE_BYTES,
};
use serde_json::{json, Value};
use solana_account::{Account, AccountSharedData};
use solana_compute_budget_interface::ComputeBudgetInstruction;
use solana_instruction::{account_meta::AccountMeta, error::InstructionError, Instruction};
use solana_keypair::Keypair;
use solana_program::{pubkey::Pubkey, rent::Rent, system_program};
use solana_program_test::{ProgramTest, ProgramTestContext};
use solana_signer::Signer;
use solana_transaction::Transaction;
use solana_transaction_error::TransactionError;

const PROGRAM: Pubkey = Pubkey::new_from_array([0xd9; 32]);
const RESOURCE: Pubkey = Pubkey::new_from_array(RESOURCE_KEY);
const WRONG_RESOURCE: Pubkey = Pubkey::new_from_array([0xd1; 32]);
const SYSTEM: Pubkey = system_program::ID;
const STATE_LENGTHS: [u32; 3] = [ENGINE_CONTEXT_BYTES, ST1_ACCOUNT_BYTES, MP1_ACCOUNT_BYTES];
const STREAM_CAPACITY: u32 = 512;
const COMMAND_WIDTH: usize = 4;
const WRITE_BATCH: usize = 8;
const RESOURCE_GROW_BATCH: usize = 8;
const RESOURCE_UPLOAD_BATCH: usize = 4;
const STATE_GROW_BATCH: usize = 8;
const VIEW_GROW_BATCH: usize = 8;
const TRANSACTION_COMPUTE_LIMIT: u32 = 1_400_000;
const GOLDEN: &str = include_str!("../../../tests/golden/doom/dcg_r0_r2_programtest.json");
const COMMANDS: &[u8] = include_bytes!("fixtures/commands.bin");

fn session_pda(authority: &Pubkey, id: u64) -> Pubkey {
    Pubkey::find_program_address(
        &[b"dcg-session-v3", authority.as_ref(), &id.to_le_bytes()],
        &PROGRAM,
    )
    .0
}

fn stream_pda(session: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(&[b"dcg-input-v3", session.as_ref()], &PROGRAM).0
}

fn state_pda(session: &Pubkey, index: u8) -> Pubkey {
    Pubkey::find_program_address(&[b"dcg-state-v3", session.as_ref(), &[index]], &PROGRAM).0
}

fn resource_pda(session: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(&[b"dcg-resource-v3", session.as_ref()], &PROGRAM).0
}

fn view_pda(session: &Pubkey, role: u8) -> Pubkey {
    Pubkey::find_program_address(&[b"dcg-view-v3", session.as_ref(), &[role]], &PROGRAM).0
}

fn instruction(tag: u8, payload: Vec<u8>, accounts: Vec<AccountMeta>) -> Instruction {
    let mut data = vec![tag];
    data.extend_from_slice(&payload);
    Instruction {
        program_id: PROGRAM,
        accounts,
        data,
    }
}

fn open_session(
    payer: Pubkey,
    authority: Pubkey,
    id: u64,
    source: Pubkey,
    commitment: [u8; 32],
) -> Instruction {
    let manifest = DoomKernel::default().manifest();
    let mut payload = vec![v3::WIRE_VERSION];
    payload.extend_from_slice(&id.to_le_bytes());
    payload.push(0); // indexed input policy
    payload.push(COMMAND_WIDTH as u8);
    payload.extend_from_slice(&STREAM_CAPACITY.to_le_bytes());
    payload.push(1); // maximum steps per ADVANCE
    payload.extend_from_slice(&DOOM_KERNEL_ID.0);
    payload.extend_from_slice(&manifest.semantic_version.to_le_bytes());
    payload.extend_from_slice(&manifest.abi_version.to_le_bytes());
    payload.extend_from_slice(&MODE_CONSENSUS_V3.id.to_le_bytes());
    payload.extend_from_slice(&MODE_CONSENSUS_V3.version.to_le_bytes());
    payload.extend_from_slice(&[0xa5; 32]);
    payload.extend_from_slice(Pubkey::default().as_ref());
    payload.extend_from_slice(source.as_ref());
    payload.extend_from_slice(&RESOURCE_SCHEMA.id.to_le_bytes());
    payload.extend_from_slice(&RESOURCE_SCHEMA.version.to_le_bytes());
    payload.extend_from_slice(&commitment);
    payload.push(1); // headerless primary state at account index zero
    instruction(
        sw::TAG_OPEN_SESSION,
        payload,
        vec![
            AccountMeta::new(payer, true),
            AccountMeta::new_readonly(authority, true),
            AccountMeta::new(session_pda(&authority, id), false),
            AccountMeta::new_readonly(source, false),
            AccountMeta::new(resource_pda(&session_pda(&authority, id)), false),
            AccountMeta::new_readonly(SYSTEM, false),
        ],
    )
}

fn resource_leaf(index: u32, len: u32, bytes: &[u8]) -> [u8; 32] {
    sha256(&[
        b"dcg/resource-chunk/1",
        &index.to_le_bytes(),
        &len.to_le_bytes(),
        bytes,
    ])
}

fn resource_levels(resource: &[u8]) -> Vec<Vec<[u8; 32]>> {
    let chunks: Vec<[u8; 32]> = resource
        .chunks(v3::RESOURCE_CHUNK_BYTES as usize)
        .enumerate()
        .map(|(index, chunk)| resource_leaf(index as u32, resource.len() as u32, chunk))
        .collect();
    let mut levels = vec![chunks];
    while levels.last().unwrap().len() > 1 {
        let current = levels.last().unwrap();
        let next = current
            .chunks(2)
            .map(|pair| {
                let right = pair.get(1).unwrap_or(&pair[0]);
                sha256(&[b"dcg/resource-node/1", &pair[0], right])
            })
            .collect();
        levels.push(next);
    }
    levels
}

fn resource_root(resource: &[u8]) -> [u8; 32] {
    resource_levels(resource).last().unwrap()[0]
}

fn resource_proof(levels: &[Vec<[u8; 32]>], index: u32) -> Vec<[u8; 32]> {
    let mut proof = Vec::new();
    let mut at = index as usize;
    for level in &levels[..levels.len() - 1] {
        let sibling = at ^ 1;
        if sibling < level.len() {
            proof.push(level[sibling]);
        }
        at >>= 1;
    }
    proof
}

fn resource_grow(payer: Pubkey, authority: Pubkey, session: Pubkey, allocated: u32) -> Instruction {
    let mut payload = vec![v3::WIRE_VERSION, v3::RESOURCE_GROW_OP];
    payload.extend_from_slice(&allocated.to_le_bytes());
    instruction(
        v3::RESOURCE_CHUNK_TAG,
        payload,
        vec![
            AccountMeta::new(payer, true),
            AccountMeta::new_readonly(authority, true),
            AccountMeta::new(session, false),
            AccountMeta::new(resource_pda(&session), false),
            AccountMeta::new_readonly(SYSTEM, false),
        ],
    )
}

fn resource_upload(
    authority: Pubkey,
    session: Pubkey,
    source: Pubkey,
    index: u32,
    proof: &[[u8; 32]],
) -> Instruction {
    let mut payload = vec![v3::WIRE_VERSION];
    payload.extend_from_slice(&index.to_le_bytes());
    payload.push(proof.len() as u8);
    for sibling in proof {
        payload.extend_from_slice(sibling);
    }
    instruction(
        v3::RESOURCE_CHUNK_TAG,
        payload,
        vec![
            AccountMeta::new_readonly(authority, true),
            AccountMeta::new(session, false),
            AccountMeta::new(resource_pda(&session), false),
            AccountMeta::new_readonly(source, false),
        ],
    )
}

fn create_stream(payer: Pubkey, session: Pubkey) -> Instruction {
    instruction(
        sw::TAG_CREATE_STREAM,
        vec![v3::WIRE_VERSION, 0],
        vec![
            AccountMeta::new(payer, true),
            AccountMeta::new(session, false),
            AccountMeta::new(stream_pda(&session), false),
            AccountMeta::new_readonly(SYSTEM, false),
        ],
    )
}

fn create_state(payer: Pubkey, session: Pubkey) -> Instruction {
    let mut payload = vec![v3::WIRE_VERSION, STATE_LENGTHS.len() as u8];
    for len in STATE_LENGTHS {
        payload.extend_from_slice(&len.to_le_bytes());
    }
    instruction(
        sw::TAG_CREATE_STATE,
        payload,
        vec![
            AccountMeta::new(payer, true),
            AccountMeta::new(session, false),
            AccountMeta::new_readonly(resource_pda(&session), false),
            AccountMeta::new(state_pda(&session, 0), false),
            AccountMeta::new(state_pda(&session, 1), false),
            AccountMeta::new(state_pda(&session, 2), false),
            AccountMeta::new_readonly(SYSTEM, false),
        ],
    )
}

fn grow_state(payer: Pubkey, session: Pubkey, index: u8) -> Instruction {
    instruction(
        sw::TAG_CREATE_STATE,
        vec![v3::WIRE_VERSION, v3::STATE_OP_GROW, index],
        vec![
            AccountMeta::new(payer, true),
            AccountMeta::new_readonly(session, false),
            AccountMeta::new(state_pda(&session, index), false),
            AccountMeta::new_readonly(SYSTEM, false),
        ],
    )
}

fn begin_initialization(authority: Pubkey, session: Pubkey) -> Instruction {
    let mut payload = vec![v3::WIRE_VERSION, v3::STATE_OP_BEGIN_INITIALIZE];
    payload.extend_from_slice(&(STATE_LENGTHS.iter().sum::<u32>()).to_le_bytes());
    payload.extend_from_slice(&V3_INITIALIZATION_PHASE_COMPUTE_UNITS.to_le_bytes());
    instruction(
        sw::TAG_CREATE_STATE,
        payload,
        vec![
            AccountMeta::new_readonly(authority, true),
            AccountMeta::new(session, false),
            AccountMeta::new_readonly(resource_pda(&session), false),
        ],
    )
}

fn run_initialization(
    authority: Pubkey,
    session: Pubkey,
    cursor: u32,
    primary: Option<Pubkey>,
) -> Instruction {
    let mut payload = vec![v3::WIRE_VERSION, v3::STATE_OP_RUN_INITIALIZE];
    payload.extend_from_slice(&cursor.to_le_bytes());
    payload.extend_from_slice(&V3_INITIALIZATION_PHASE_COMPUTE_UNITS.to_le_bytes());
    instruction(
        sw::TAG_CREATE_STATE,
        payload,
        vec![
            AccountMeta::new(primary.unwrap_or_else(|| state_pda(&session, 0)), false),
            AccountMeta::new_readonly(authority, true),
            AccountMeta::new(session, false),
            AccountMeta::new_readonly(resource_pda(&session), false),
            AccountMeta::new(state_pda(&session, 1), false),
            AccountMeta::new(state_pda(&session, 2), false),
        ],
    )
}

fn write_input(authority: Pubkey, session: Pubkey, sequence: u32, command: &[u8]) -> Instruction {
    let mut payload = vec![v3::WIRE_VERSION];
    payload.extend_from_slice(&sequence.to_le_bytes());
    payload.push(COMMAND_WIDTH as u8);
    payload.extend_from_slice(command);
    instruction(
        sw::TAG_WRITE_INPUT,
        payload,
        vec![
            AccountMeta::new_readonly(authority, true),
            AccountMeta::new(session, false),
            AccountMeta::new(stream_pda(&session), false),
        ],
    )
}

fn advance(authority: Pubkey, session: Pubkey, cursor: u32, steps: u8) -> Instruction {
    let mut payload = vec![v3::WIRE_VERSION];
    payload.extend_from_slice(&cursor.to_le_bytes());
    payload.push(steps);
    instruction(
        sw::TAG_ADVANCE,
        payload,
        vec![
            AccountMeta::new(state_pda(&session, 0), false),
            AccountMeta::new_readonly(authority, true),
            AccountMeta::new(session, false),
            AccountMeta::new(stream_pda(&session), false),
            AccountMeta::new(state_pda(&session, 1), false),
            AccountMeta::new(state_pda(&session, 2), false),
        ],
    )
}

fn create_view(payer: Pubkey, session: Pubkey, role: u8, abi: [u8; 32], len: u32) -> Instruction {
    let mut payload = vec![v3::WIRE_VERSION, role];
    payload.extend_from_slice(&abi);
    payload.extend_from_slice(&0u32.to_le_bytes());
    payload.extend_from_slice(&len.to_le_bytes());
    instruction(
        sw::TAG_CREATE_VIEW,
        payload,
        vec![
            AccountMeta::new(payer, true),
            AccountMeta::new(session, false),
            AccountMeta::new(view_pda(&session, role), false),
            AccountMeta::new_readonly(SYSTEM, false),
        ],
    )
}

fn create_workspace(payer: Pubkey, session: Pubkey) -> Instruction {
    let mut payload = vec![v3::WIRE_VERSION, v3::WORKSPACE_ROLE];
    payload.extend_from_slice(&V3_VIEW_WORKSPACE_BYTES.to_le_bytes());
    instruction(
        sw::TAG_CREATE_VIEW,
        payload,
        vec![
            AccountMeta::new(payer, true),
            AccountMeta::new(session, false),
            AccountMeta::new(view_pda(&session, v3::WORKSPACE_ROLE), false),
            AccountMeta::new_readonly(SYSTEM, false),
        ],
    )
}

fn create_scratch(payer: Pubkey, session: Pubkey, len: u32) -> Instruction {
    let mut payload = vec![v3::WIRE_VERSION, v3::SCRATCH_ROLE];
    payload.extend_from_slice(&[0; 32]);
    payload.extend_from_slice(&0u32.to_le_bytes());
    payload.extend_from_slice(&len.to_le_bytes());
    instruction(
        sw::TAG_CREATE_VIEW,
        payload,
        vec![
            AccountMeta::new(payer, true),
            AccountMeta::new(session, false),
            AccountMeta::new(view_pda(&session, v3::SCRATCH_ROLE), false),
            AccountMeta::new_readonly(SYSTEM, false),
        ],
    )
}

fn grow_view(payer: Pubkey, session: Pubkey, role: u8) -> Instruction {
    instruction(
        sw::TAG_CREATE_VIEW,
        vec![v3::WIRE_VERSION, v3::VIEW_OP_GROW, role],
        vec![
            AccountMeta::new(payer, true),
            AccountMeta::new(session, false),
            AccountMeta::new(view_pda(&session, role), false),
            AccountMeta::new_readonly(SYSTEM, false),
        ],
    )
}

fn publication_accounts(
    authority: Pubkey,
    session: Pubkey,
    states_writable: bool,
    outputs_writable: bool,
    workspace_writable: bool,
    scratch_writable: bool,
) -> Vec<AccountMeta> {
    let state_meta = |index| {
        let key = state_pda(&session, index);
        if states_writable {
            AccountMeta::new(key, false)
        } else {
            AccountMeta::new_readonly(key, false)
        }
    };
    let mut accounts = vec![
        state_meta(0),
        AccountMeta::new_readonly(authority, true),
        AccountMeta::new(session, false),
        AccountMeta::new_readonly(resource_pda(&session), false),
        state_meta(1),
        state_meta(2),
    ];
    for role in 0..=8 {
        let key = view_pda(&session, role);
        accounts.push(if outputs_writable {
            AccountMeta::new(key, false)
        } else {
            AccountMeta::new_readonly(key, false)
        });
    }
    let workspace = view_pda(&session, v3::WORKSPACE_ROLE);
    accounts.push(if workspace_writable {
        AccountMeta::new(workspace, false)
    } else {
        AccountMeta::new_readonly(workspace, false)
    });
    let scratch = view_pda(&session, v3::SCRATCH_ROLE);
    accounts.push(if scratch_writable {
        AccountMeta::new(scratch, false)
    } else {
        AccountMeta::new_readonly(scratch, false)
    });
    accounts
}

fn begin_views(authority: Pubkey, session: Pubkey, cursor: u32) -> Instruction {
    let mut payload = vec![v3::WIRE_VERSION, 0];
    payload.extend_from_slice(&cursor.to_le_bytes());
    payload.extend_from_slice(&V3_VIEW_PHASE_COMPUTE_CEILING.to_le_bytes());
    instruction(
        sw::TAG_PUBLISH_VIEWS,
        payload,
        publication_accounts(authority, session, false, false, true, true),
    )
}

fn run_view_phase(authority: Pubkey, session: Pubkey, cursor: u32) -> Instruction {
    let mut payload = vec![v3::WIRE_VERSION, 1];
    payload.extend_from_slice(&cursor.to_le_bytes());
    payload.extend_from_slice(&V3_VIEW_PHASE_COMPUTE_CEILING.to_le_bytes());
    instruction(
        sw::TAG_PUBLISH_VIEWS,
        payload,
        publication_accounts(authority, session, true, false, true, true),
    )
}

fn commit_views(authority: Pubkey, session: Pubkey, cursor: u32, total: u32) -> Instruction {
    let mut payload = vec![v3::WIRE_VERSION, 2];
    payload.extend_from_slice(&cursor.to_le_bytes());
    payload.extend_from_slice(&total.to_le_bytes());
    payload.extend_from_slice(&V3_VIEW_PHASE_COMPUTE_CEILING.to_le_bytes());
    instruction(
        sw::TAG_PUBLISH_VIEWS,
        payload,
        publication_accounts(authority, session, false, true, false, false),
    )
}

async fn send(
    context: &mut ProgramTestContext,
    ix: Instruction,
    extra_signers: &[&Keypair],
    expected: Result<(), u32>,
) -> u64 {
    let blockhash = context.get_new_latest_blockhash().await.unwrap();
    let instruction_data = ix.data.clone();
    let mut signers = vec![&context.payer];
    signers.extend_from_slice(extra_signers);
    let instructions = [
        ComputeBudgetInstruction::set_compute_unit_limit(TRANSACTION_COMPUTE_LIMIT),
        ix,
    ];
    let tx = Transaction::new(
        &signers,
        solana_message::Message::new(&instructions, Some(&context.payer.pubkey())),
        blockhash,
    );
    let result = context
        .banks_client
        .process_transaction_with_metadata(tx)
        .await
        .unwrap();
    let units = result
        .metadata
        .as_ref()
        .map(|meta| meta.compute_units_consumed)
        .unwrap_or(0);
    match (expected, result.result) {
        (Ok(()), Ok(())) => {}
        (Err(want), Err(TransactionError::InstructionError(_, InstructionError::Custom(got))))
            if want == got => {}
        (want, got) => {
            let logs = result
                .metadata
                .as_ref()
                .map(|meta| meta.log_messages.join("\n"))
                .unwrap_or_default();
            panic!(
                "expected {want:?}, got {got:?}, instruction_data={:?}\n{logs}",
                instruction_data
            );
        }
    }
    units
}

async fn send_many(
    context: &mut ProgramTestContext,
    instructions: Vec<Instruction>,
    extra_signers: &[&Keypair],
) -> (u64, Vec<u64>) {
    let application_instruction_count = instructions.len();
    let blockhash = context.get_new_latest_blockhash().await.unwrap();
    let mut signers = vec![&context.payer];
    signers.extend_from_slice(extra_signers);
    let mut batched_instructions = Vec::with_capacity(instructions.len() + 1);
    batched_instructions.push(ComputeBudgetInstruction::set_compute_unit_limit(
        TRANSACTION_COMPUTE_LIMIT,
    ));
    batched_instructions.extend(instructions);
    let tx = Transaction::new(
        &signers,
        solana_message::Message::new(&batched_instructions, Some(&context.payer.pubkey())),
        blockhash,
    );
    let result = context
        .banks_client
        .process_transaction_with_metadata(tx)
        .await
        .unwrap();
    if let Err(error) = result.result {
        let logs = result
            .metadata
            .as_ref()
            .map(|meta| meta.log_messages.join("\n"))
            .unwrap_or_default();
        panic!("batch transaction failed: {error:?}\n{logs}");
    }
    let transaction_cu = result
        .metadata
        .as_ref()
        .map(|meta| meta.compute_units_consumed)
        .unwrap_or(0);
    let program_prefix = format!("Program {PROGRAM} consumed ");
    let instruction_cu = result
        .metadata
        .as_ref()
        .unwrap()
        .log_messages
        .iter()
        .filter_map(|line| line.strip_prefix(&program_prefix))
        .filter_map(|line| line.split_once(" of "))
        .filter_map(|(used, _)| used.parse::<u64>().ok())
        .collect::<Vec<_>>();
    assert_eq!(instruction_cu.len(), application_instruction_count);
    (transaction_cu, instruction_cu)
}

async fn start_sbf(good_resource: &[u8], wrong_resource: &[u8]) -> ProgramTestContext {
    let output_dir =
        std::env::var("DOOM_DCG_SBF_DIR").expect("DOOM_DCG_SBF_DIR must contain doom_dcg.so");
    std::env::set_var("BPF_OUT_DIR", &output_dir);
    std::env::set_var("SBF_OUT_DIR", &output_dir);
    let mut test = ProgramTest::default();
    test.prefer_bpf(true);
    test.add_program("doom_dcg", PROGRAM, None);
    for (key, data) in [(RESOURCE, good_resource), (WRONG_RESOURCE, wrong_resource)] {
        test.add_genesis_account(
            key,
            Account {
                lamports: Rent::default().minimum_balance(data.len()),
                data: data.to_vec(),
                owner: PROGRAM,
                executable: false,
                rent_epoch: 0,
            },
        );
    }
    test.start_with_context().await
}

async fn upload_sealed_resource(
    context: &mut ProgramTestContext,
    authority: &Keypair,
    session: Pubkey,
    source: Pubkey,
    resource: &[u8],
) -> (Vec<u64>, Vec<u64>, Vec<u64>, Vec<u64>) {
    let payer = context.payer.pubkey();
    let mut grow_ops = Vec::new();
    let mut allocated = v3::CHILD_GROW_BYTES;
    while allocated < resource.len() as u32 {
        allocated = (allocated + v3::CHILD_GROW_BYTES).min(resource.len() as u32);
        grow_ops.push(resource_grow(payer, authority.pubkey(), session, allocated));
    }
    let mut grow_cu = Vec::new();
    let mut grow_instruction_cu = Vec::new();
    for batch in grow_ops.chunks(RESOURCE_GROW_BATCH) {
        let (transaction_cu, instruction_cu) =
            send_many(context, batch.to_vec(), &[authority]).await;
        grow_cu.push(transaction_cu);
        grow_instruction_cu.extend(instruction_cu);
    }
    let levels = resource_levels(resource);
    let mut upload_cu = Vec::new();
    let mut upload_instruction_cu = Vec::new();
    for start in (0..levels[0].len()).step_by(RESOURCE_UPLOAD_BATCH) {
        let end = (start + RESOURCE_UPLOAD_BATCH).min(levels[0].len());
        let batch = (start..end)
            .map(|index| {
                resource_upload(
                    authority.pubkey(),
                    session,
                    source,
                    index as u32,
                    &resource_proof(&levels, index as u32),
                )
            })
            .collect();
        let (transaction_cu, instruction_cu) = send_many(context, batch, &[authority]).await;
        upload_cu.push(transaction_cu);
        upload_instruction_cu.extend(instruction_cu);
    }
    (
        grow_cu,
        grow_instruction_cu,
        upload_cu,
        upload_instruction_cu,
    )
}

async fn grow_state_spans(
    context: &mut ProgramTestContext,
    session: Pubkey,
) -> (Vec<u64>, Vec<u64>) {
    let mut grow_ops = Vec::new();
    for (index, desired) in STATE_LENGTHS.into_iter().enumerate() {
        let mut allocated = desired.min(v3::CHILD_GROW_BYTES);
        while allocated < desired {
            allocated = (allocated + v3::CHILD_GROW_BYTES).min(desired);
            grow_ops.push(grow_state(context.payer.pubkey(), session, index as u8));
        }
    }
    let mut units = Vec::new();
    let mut instruction_units = Vec::new();
    for batch in grow_ops.chunks(STATE_GROW_BATCH) {
        let (transaction_cu, instruction_cu) = send_many(context, batch.to_vec(), &[]).await;
        units.push(transaction_cu);
        instruction_units.extend(instruction_cu);
    }
    (units, instruction_units)
}

async fn create_and_grow_views(
    context: &mut ProgramTestContext,
    session: Pubkey,
) -> (u64, Vec<u64>, Vec<u64>, Vec<u64>) {
    let payer = context.payer.pubkey();
    let total = SNAPSHOT_BYTES + STRIP_BYTES * 8;
    let mut create_ops = vec![create_view(
        payer,
        session,
        0,
        V3_SNAPSHOT_ABI,
        SNAPSHOT_BYTES,
    )];
    for role in 1..=8 {
        create_ops.push(create_view(
            payer,
            session,
            role,
            V3_STRIP_ABIS[(role - 1) as usize],
            STRIP_BYTES,
        ));
    }
    create_ops.push(create_workspace(payer, session));
    create_ops.push(create_scratch(payer, session, total));
    let (create_cu, create_instruction_cu) = send_many(context, create_ops, &[]).await;

    let mut grow_ops = Vec::new();
    for (role, len) in [
        (0, SNAPSHOT_BYTES),
        (v3::WORKSPACE_ROLE, V3_VIEW_WORKSPACE_BYTES),
        (v3::SCRATCH_ROLE, total),
    ] {
        let mut allocated = len.min(v3::CHILD_GROW_BYTES);
        while allocated < len {
            allocated = (allocated + v3::CHILD_GROW_BYTES).min(len);
            grow_ops.push(grow_view(payer, session, role));
        }
    }
    let mut grow_cu = Vec::new();
    let mut grow_instruction_cu = Vec::new();
    for batch in grow_ops.chunks(VIEW_GROW_BATCH) {
        let (transaction_cu, instruction_cu) = send_many(context, batch.to_vec(), &[]).await;
        grow_cu.push(transaction_cu);
        grow_instruction_cu.extend(instruction_cu);
    }
    (
        create_cu,
        create_instruction_cu,
        grow_cu,
        grow_instruction_cu,
    )
}

fn command_at(index: usize) -> &'static [u8] {
    let start = index * COMMAND_WIDTH;
    &COMMANDS[start..start + COMMAND_WIDTH]
}

fn u32_at(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
}

fn put_u16(bytes: &mut [u8], offset: usize, value: u16) {
    bytes[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}

fn put_u32(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn sim2_state_root(st1: &[u8], mp1: &[u8]) -> [u8; 32] {
    let state_len = u32_at(st1, 16) as usize;
    let count = u32_at(mp1, 28) as usize;
    let mirror_used = 4_096 + count * 160;
    sha256(&[
        b"DOOM-SIM/2-STATE\0",
        &(state_len as u32).to_le_bytes(),
        &st1[..state_len],
        &(mirror_used as u32).to_le_bytes(),
        &mp1[..mirror_used],
    ])
}

fn decode_hex32(hex: &str) -> [u8; 32] {
    assert_eq!(hex.len(), 64);
    let mut out = [0u8; 32];
    for (index, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&hex[index * 2..index * 2 + 2], 16).unwrap();
    }
    out
}

async fn state_accounts(context: &mut ProgramTestContext, session: Pubkey) -> (Account, Account) {
    let st1 = context
        .banks_client
        .get_account(state_pda(&session, 1))
        .await
        .unwrap()
        .expect("ST1 account exists");
    let mp1 = context
        .banks_client
        .get_account(state_pda(&session, 2))
        .await
        .unwrap()
        .expect("MP1 account exists");
    (st1, mp1)
}

async fn inject_full_spans_for_bad_resource(
    context: &mut ProgramTestContext,
    session: Pubkey,
    authority: Pubkey,
) {
    let total: u32 = STATE_LENGTHS.iter().sum();
    let mut offset = 0u32;
    for (index, len) in STATE_LENGTHS.into_iter().enumerate() {
        let key = state_pda(&session, index as u8);
        let mut account = context
            .banks_client
            .get_account(key)
            .await
            .unwrap()
            .expect("state account exists");
        if index == 0 {
            account.data.resize(len as usize, 0);
        } else {
            account.data.resize(128 + len as usize, 0);
            account.data[..4].copy_from_slice(b"DSE3");
            put_u16(&mut account.data, 4, v3::WIRE_VERSION as u16);
            account.data[6] = v3::KIND_STATE;
            account.data[8..40].copy_from_slice(session.as_ref());
            account.data[40..72].copy_from_slice(authority.as_ref());
            put_u32(&mut account.data, 72, STATE_SCHEMA.id);
            put_u16(&mut account.data, 76, STATE_SCHEMA.version);
            account.data[78] = index as u8;
            account.data[79] = STATE_LENGTHS.len() as u8;
            put_u32(&mut account.data, 80, offset);
            put_u32(&mut account.data, 84, len);
            put_u32(&mut account.data, 88, 0);
            put_u32(&mut account.data, 92, 0);
            put_u32(&mut account.data, 96, total);
            put_u32(&mut account.data, 100, len);
        }
        account.lamports = account
            .lamports
            .max(Rent::default().minimum_balance(account.data.len()));
        context.set_account(&key, &AccountSharedData::from(account));
        offset += len;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn doom_stateful_v3_matches_sim2_roots_renders_and_rejects_adversarial_inputs() {
    assert_eq!(COMMANDS.len(), 1_000 * COMMAND_WIDTH);
    let golden: Value = serde_json::from_str(GOLDEN).unwrap();
    let resource = std::fs::read(
        std::env::var("DOOM_DCG_RESOURCE_PATH").expect("DOOM_DCG_RESOURCE_PATH must be set"),
    )
    .expect("packed RI2/WAD resource fixture");
    assert_eq!(resource.len(), RESOURCE_BYTES);
    let mut wrong_resource = resource.clone();
    wrong_resource[32 + 261_056] ^= 1; // change one WAD byte but keep RI2's pinned WAD digest
    let mut context = start_sbf(&resource, &wrong_resource).await;

    let authority = Keypair::new_from_array([0x17; 32]);
    let payer = context.payer.pubkey();
    let session = session_pda(&authority.pubkey(), 17);
    let resource_commitment = resource_root(&resource);
    assert_eq!(resource_commitment, DOOM_RESOURCE_COMMITMENT);
    let open_cu = send(
        &mut context,
        open_session(payer, authority.pubkey(), 17, RESOURCE, resource_commitment),
        &[&authority],
        Ok(()),
    )
    .await;
    let stream_cu = send(&mut context, create_stream(payer, session), &[], Ok(())).await;
    let state_create_cu = send(&mut context, create_state(payer, session), &[], Ok(())).await;

    // Keep the ring backlog within its 512-slot allocation. Commands are
    // written up to each next ADVANCE batch rather than filling the whole
    // 1,000-command stream before the engine has consumed any slots.
    let first_write_cu = send(
        &mut context,
        write_input(authority.pubkey(), session, 0, command_at(0)),
        &[&authority],
        Ok(()),
    )
    .await;
    let mut input_write_cu = vec![first_write_cu];
    let mut input_write_instruction_cu = vec![first_write_cu];
    let mut written = 1u32;

    // These two failures happen against the actual unsealed session and before
    // its first initialization phase. The failed transactions roll back.
    let unsealed_begin_cu = send(
        &mut context,
        begin_initialization(authority.pubkey(), session),
        &[&authority],
        Err(v3::REFUSAL_RESOURCE),
    )
    .await;
    let early_advance_cu = send(
        &mut context,
        advance(authority.pubkey(), session, 0, 1),
        &[&authority],
        Err(v3::REFUSAL_STATE),
    )
    .await;

    let (
        resource_grow_cu,
        resource_grow_instruction_cu,
        resource_upload_cu,
        resource_upload_instruction_cu,
    ) = upload_sealed_resource(&mut context, &authority, session, RESOURCE, &resource).await;
    let copied_resource = context
        .banks_client
        .get_account(resource_pda(&session))
        .await
        .unwrap()
        .expect("sealed program-owned resource copy");
    assert_eq!(copied_resource.data.len(), 128 + RESOURCE_BYTES);
    assert_eq!(&copied_resource.data[128..], resource.as_slice());
    let (state_grow_cu, state_grow_instruction_cu) = grow_state_spans(&mut context, session).await;
    let begin_init_cu = send(
        &mut context,
        begin_initialization(authority.pubkey(), session),
        &[&authority],
        Ok(()),
    )
    .await;
    // Use a valid, program-owned but wrong account in index zero. V3's
    // headerless-primary key check must reject it before the Doom callback.
    let wrong_primary_cu = send(
        &mut context,
        run_initialization(authority.pubkey(), session, 0, Some(stream_pda(&session))),
        &[&authority],
        Err(v3::REFUSAL_STATE),
    )
    .await;

    let phase_count = (STATE_LENGTHS.iter().sum::<u32>() + 16_383) / 16_384;
    let wad_end = 3_440_976usize + 4_196_020;
    let first_engine_chunk = (wad_end - 1) / 16_384;
    let mut init_phase_cu = Vec::new();
    for phase_index in 0..phase_count {
        let cursor = phase_index * 16_384;
        let cu = send(
            &mut context,
            run_initialization(authority.pubkey(), session, cursor, None),
            &[&authority],
            Ok(()),
        )
        .await;
        let phase_number = phase_index as usize;
        let category = if phase_number < first_engine_chunk {
            "wad-copy"
        } else if phase_number < first_engine_chunk + 115 {
            "engine"
        } else {
            "tail"
        };
        init_phase_cu.push(json!({
            "cursor": cursor,
            "category": category,
            "engine_phase": if category == "engine" { Some(phase_number - first_engine_chunk) } else { None },
            "cu": cu
        }));
    }
    assert_eq!(phase_count, 606);

    let (st1, mp1) = state_accounts(&mut context, session).await;
    let initial_root = sim2_state_root(&st1.data[128..], &mp1.data[128..]);
    assert_eq!(
        initial_root,
        decode_hex32(golden["input"]["initial_state_root"].as_str().unwrap())
    );

    let mut adv_cu = Vec::new();
    let mut cursor = 0u32;
    for boundary in golden["sim2"]["sweep_boundaries"].as_array().unwrap() {
        let target = boundary["cursor"].as_u64().unwrap() as u32;
        while cursor < target {
            let write_target = target.min(cursor.saturating_add(1));
            while written < write_target {
                let end = (written + WRITE_BATCH as u32).min(write_target);
                let batch = (written..end)
                    .map(|sequence| {
                        write_input(
                            authority.pubkey(),
                            session,
                            sequence,
                            command_at(sequence as usize),
                        )
                    })
                    .collect::<Vec<_>>();
                let (transaction_cu, instruction_cu) =
                    send_many(&mut context, batch, &[&authority]).await;
                input_write_cu.push(transaction_cu);
                input_write_instruction_cu.extend(instruction_cu);
                written = end;
            }
            let steps = (target - cursor).min(1) as u8;
            let cu = send(
                &mut context,
                advance(authority.pubkey(), session, cursor, steps),
                &[&authority],
                Ok(()),
            )
            .await;
            adv_cu.push(json!({"cursor": cursor, "steps": steps, "cu": cu}));
            cursor += steps as u32;
        }
        let (st1, mp1) = state_accounts(&mut context, session).await;
        let root = sim2_state_root(&st1.data[128..], &mp1.data[128..]);
        assert_eq!(root, decode_hex32(boundary["state_root"].as_str().unwrap()));
        assert_eq!(
            sha256(&[&st1.data[128..]]),
            decode_hex32(boundary["st1_account_sha256"].as_str().unwrap())
        );
        assert_eq!(
            sha256(&[&mp1.data[128..]]),
            decode_hex32(boundary["mp1_sha256"].as_str().unwrap())
        );
        assert_eq!(
            u32_at(
                &context
                    .banks_client
                    .get_account(session)
                    .await
                    .unwrap()
                    .unwrap()
                    .data,
                112
            ),
            target
        );
    }
    assert_eq!(cursor, 1_000);

    let (view_create_cu, view_create_instruction_cu, view_grow_cu, view_grow_instruction_cu) =
        create_and_grow_views(&mut context, session).await;
    let frame_bytes = SNAPSHOT_BYTES + STRIP_BYTES * 8;
    let view_begin_cu = send(
        &mut context,
        begin_views(authority.pubkey(), session, cursor),
        &[&authority],
        Ok(()),
    )
    .await;
    let mut view_phase_cu = Vec::new();
    let mut view_cursor = 0u32;
    while view_cursor < frame_bytes {
        let cu = send(
            &mut context,
            run_view_phase(authority.pubkey(), session, view_cursor),
            &[&authority],
            Ok(()),
        )
        .await;
        view_phase_cu.push(json!({"cursor": view_cursor, "cu": cu}));
        view_cursor += V3_VIEW_PHASE_BYTES.min(frame_bytes - view_cursor);
    }
    let view_commit_cu = send(
        &mut context,
        commit_views(authority.pubkey(), session, cursor, frame_bytes),
        &[&authority],
        Ok(()),
    )
    .await;
    let mut pixels = Vec::with_capacity(320 * 200);
    let mut strip_hashes = Vec::new();
    for role in 1..=8 {
        let account = context
            .banks_client
            .get_account(view_pda(&session, role))
            .await
            .unwrap()
            .expect("committed strip account");
        let strip = &account.data[128..];
        assert_eq!(&strip[..8], b"DOOM-RS1");
        let strip_pixels = &strip[128..];
        assert_eq!(strip_pixels.len(), 40 * 200);
        strip_hashes.push(sha256(&[strip_pixels]));
        pixels.extend_from_slice(strip_pixels);
    }
    let frame_hash = sha256(&[&pixels]);
    assert_eq!(
        frame_hash,
        decode_hex32(
            golden["render"]["reconstructed_frame_sha256"]
                .as_str()
                .unwrap()
        )
    );
    for (index, expected) in golden["render"]["strips"]
        .as_array()
        .unwrap()
        .iter()
        .enumerate()
    {
        assert_eq!(
            strip_hashes[index],
            decode_hex32(expected["pixels_sha256"].as_str().unwrap())
        );
    }

    // Exercise a real Doom pre-tic halt. The offset is the host-compiled
    // offsetof(doom_ctx_t, gamestate) from the pinned headers; GS_INTERMISSION
    // is enum value 1. ProgramTest mutates the primary account to construct
    // this otherwise unreachable boundary state.
    let context_key = state_pda(&session, 0);
    let mut context_account = context
        .banks_client
        .get_account(context_key)
        .await
        .unwrap()
        .unwrap();
    let gamestate_offset = 30_764usize;
    put_u32(&mut context_account.data, gamestate_offset, 1);
    context.set_account(&context_key, &AccountSharedData::from(context_account));
    let extra_command = command_at(999);
    let halt_write_cu = send(
        &mut context,
        write_input(authority.pubkey(), session, 1_000, extra_command),
        &[&authority],
        Ok(()),
    )
    .await;
    let engine_halt_cu = send(
        &mut context,
        advance(authority.pubkey(), session, 1_000, 1),
        &[&authority],
        Ok(()),
    )
    .await;
    let halted = context
        .banks_client
        .get_account(session)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(halted.data[6], 2); // STATUS_HALTED
    assert_eq!(u32_at(&halted.data, 1254), 1); // DOOM_SIM2_HALT_GAMESTATE
    assert_eq!(u32_at(&halted.data, 1258), 1_000); // zero-length committed prefix
    let after_halt_advance_cu = send(
        &mut context,
        advance(authority.pubkey(), session, 1_000, 1),
        &[&authority],
        Err(v3::REFUSAL_AUTHORITY),
    )
    .await;

    // A separately sealed resource has a valid DCG chunk root, but the WAD
    // payload digest is wrong. The large state allocations are injected only
    // to reach the app validator without repeating the 1,210 growth steps.
    let bad_authority = Keypair::new_from_array([0x18; 32]);
    let bad_session = session_pda(&bad_authority.pubkey(), 18);
    let bad_resource_commitment = resource_root(&wrong_resource);
    let bad_open_cu = send(
        &mut context,
        open_session(
            payer,
            bad_authority.pubkey(),
            18,
            WRONG_RESOURCE,
            bad_resource_commitment,
        ),
        &[&bad_authority],
        Ok(()),
    )
    .await;
    let bad_state_create_cu =
        send(&mut context, create_state(payer, bad_session), &[], Ok(())).await;
    let (
        bad_resource_grow_cu,
        bad_resource_grow_instruction_cu,
        bad_resource_upload_cu,
        bad_resource_upload_instruction_cu,
    ) = upload_sealed_resource(
        &mut context,
        &bad_authority,
        bad_session,
        WRONG_RESOURCE,
        &wrong_resource,
    )
    .await;
    inject_full_spans_for_bad_resource(&mut context, bad_session, bad_authority.pubkey()).await;
    let bad_begin_cu = send(
        &mut context,
        begin_initialization(bad_authority.pubkey(), bad_session),
        &[&bad_authority],
        Ok(()),
    )
    .await;
    let bad_wad_cu = send(
        &mut context,
        run_initialization(bad_authority.pubkey(), bad_session, 0, None),
        &[&bad_authority],
        Err(v3::REFUSAL_KERNEL),
    )
    .await;

    let metrics_path = std::env::var("DOOM_DCG_METRICS_PATH").ok();
    if let Some(path) = metrics_path {
        let report = json!({
            "schema": "doom-dcg-r6-programtest/1",
            "standalone_golden": "tests/golden/doom/dcg_r0_r2_programtest.json",
            "resource_bytes": resource.len(),
            "resource_merkle_root": resource_commitment,
            "initialization": {
                "phase_count": init_phase_cu.len(),
                "phase_bytes": 16_384,
                "engine_phase_first_chunk": first_engine_chunk,
                "engine_phase_count": 115,
                "cu_by_phase": &init_phase_cu,
                "phase_cu_sum": init_phase_cu.iter().map(|item| item["cu"].as_u64().unwrap()).sum::<u64>(),
            },
            "advance": {
                "tic_count": 1_000,
                "transaction_count": adv_cu.len(),
                "transactions_per_tic": adv_cu.len() as f64 / 1_000.0,
                "compute_units_by_transaction": adv_cu,
            },
            "frame": {
                "bytes": frame_bytes,
                "view_phase_bytes": V3_VIEW_PHASE_BYTES,
                "run_phase_count": view_phase_cu.len(),
                "transactions_per_frame": view_phase_cu.len() + 2,
                "phase_compute_units": view_phase_cu,
                "snapshot_create_cu": view_begin_cu,
                "publish_commit_cu": view_commit_cu,
                "pixel_sha256": frame_hash,
            },
            "adversarial": {
                "unsealed_resource_begin_cu": unsealed_begin_cu,
                "advance_before_init_cu": early_advance_cu,
                "wrong_primary_cu": wrong_primary_cu,
                "engine_halt_cu": engine_halt_cu,
                "halted_session_advance_cu": after_halt_advance_cu,
                "wrong_wad_cu": bad_wad_cu,
            },
            "setup": {
                "open_cu": open_cu,
                "stream_create_cu": stream_cu,
                "state_create_cu": state_create_cu,
                "input_write_tx_count": input_write_cu.len(),
                "input_write_transaction_cu": input_write_cu,
                "input_write_instruction_cu": input_write_instruction_cu,
                "resource_grow_tx_count": resource_grow_cu.len(),
                "resource_grow_transaction_cu": resource_grow_cu,
                "resource_grow_instruction_cu": resource_grow_instruction_cu,
                "resource_upload_tx_count": resource_upload_cu.len(),
                "resource_upload_transaction_cu": resource_upload_cu,
                "resource_upload_instruction_cu": resource_upload_instruction_cu,
                "state_grow_tx_count": state_grow_cu.len(),
                "state_grow_transaction_cu": state_grow_cu,
                "state_grow_instruction_cu": state_grow_instruction_cu,
                "create_views_cu": view_create_cu,
                "grow_view_tx_count": view_grow_cu.len(),
                "create_views_instruction_cu": view_create_instruction_cu,
                "grow_view_transaction_cu": view_grow_cu,
                "grow_view_instruction_cu": view_grow_instruction_cu,
                "begin_init_cu": begin_init_cu,
                "wrong_primary_attempt_cu": wrong_primary_cu,
                "halt_input_write_cu": halt_write_cu,
                "bad_wad": {
                    "open_cu": bad_open_cu,
                    "state_create_cu": bad_state_create_cu,
                    "resource_grow_transaction_cu": bad_resource_grow_cu,
                    "resource_grow_instruction_cu": bad_resource_grow_instruction_cu,
                    "resource_upload_transaction_cu": bad_resource_upload_cu,
                    "resource_upload_instruction_cu": bad_resource_upload_instruction_cu,
                    "begin_init_cu": bad_begin_cu,
                    "run_init_refusal_cu": bad_wad_cu,
                },
            }
        });
        std::fs::write(path, serde_json::to_vec_pretty(&report).unwrap()).unwrap();
    }
}
