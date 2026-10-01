#![cfg(feature = "sbf-programtest")]
#![recursion_limit = "256"]

//! Full composed SBF lifecycle proof for DCG stateful v3. This runs the real
//! engine, checks every retained SIM/2 sweep root, and renders all RSN/3 views.

use dcg_program::{hash::sha256, kernel::Kernel, stateful as sw, stateful::v3};
use doom_dcg::{
    DoomKernel, DOOM_KERNEL_ID, DOOM_RESOURCE_COMMITMENT, ENGINE_CONTEXT_BYTES, MODE_CONSENSUS_V3,
    MP1_ACCOUNT_BYTES, RESOURCE_BYTES, RESOURCE_KEY, RESOURCE_SCHEMA, SNAPSHOT_BYTES,
    ST1_ACCOUNT_BYTES, STATE_SCHEMA, STRIP_BYTES, V3_INITIALIZATION_PHASE_COMPUTE_UNITS,
    V3_RENDER_ARENA_HIGH_WATER_OFFSET, V3_RENDER_ARENA_MAX_BYTES, V3_SNAPSHOT_ABI,
    V3_SNAPSHOT_CACHE_BYTES, V3_SNAPSHOT_CACHE_OFFSET, V3_STRIP_ABIS, V3_VIEW_PHASE_BYTES,
    V3_VIEW_PHASE_COMPUTE_CEILING, V3_VIEW_WORKSPACE_BYTES,
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
const STREAM_FINAL_CAPACITY: u32 = 1_024;
const COMMAND_WIDTH: usize = 4;
const WRITE_BATCH: usize = 8;
const RESOURCE_GROW_BATCH: usize = 8;
const RESOURCE_UPLOAD_BATCH: usize = 4;
const STATE_GROW_BATCH: usize = 8;
const VIEW_GROW_BATCH: usize = 8;
const TRANSACTION_COMPUTE_LIMIT: u32 = 1_400_000;
const TRANSACTION_PACKET_LIMIT: usize = 1_232;
const ACCOUNT_HEADER_BYTES: usize = 128;
const ACCOUNT_CURSOR_OFFSET: usize = 112;
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

fn grow_stream(payer: Pubkey, session: Pubkey, new_capacity: u32) -> Instruction {
    let mut payload = vec![v3::WIRE_VERSION, 1];
    payload.extend_from_slice(&new_capacity.to_le_bytes());
    instruction(
        sw::TAG_CREATE_STREAM,
        payload,
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

fn close_child(session: Pubkey, target: Pubkey, refund: Pubkey, kind: u8) -> Instruction {
    instruction(
        sw::TAG_CLOSE_ACCOUNT,
        vec![v3::WIRE_VERSION, kind],
        vec![
            AccountMeta::new(session, false),
            AccountMeta::new(target, false),
            AccountMeta::new(refund, false),
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

fn workspace_first_publication_accounts(
    authority: Pubkey,
    session: Pubkey,
    outputs_writable: bool,
    workspace_writable: bool,
    scratch_writable: bool,
) -> Vec<AccountMeta> {
    let workspace = view_pda(&session, v3::WORKSPACE_ROLE);
    let mut accounts = vec![
        if workspace_writable {
            AccountMeta::new(workspace, false)
        } else {
            AccountMeta::new_readonly(workspace, false)
        },
        AccountMeta::new_readonly(authority, true),
        AccountMeta::new(session, false),
        AccountMeta::new_readonly(resource_pda(&session), false),
        AccountMeta::new_readonly(state_pda(&session, 0), false),
        AccountMeta::new_readonly(state_pda(&session, 1), false),
        AccountMeta::new_readonly(state_pda(&session, 2), false),
    ];
    for role in 0..=8 {
        let key = view_pda(&session, role);
        accounts.push(if outputs_writable {
            AccountMeta::new(key, false)
        } else {
            AccountMeta::new_readonly(key, false)
        });
    }
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

fn run_view_phase_with_state_rights(
    authority: Pubkey,
    session: Pubkey,
    cursor: u32,
    states_writable: bool,
) -> Instruction {
    let mut payload = vec![v3::WIRE_VERSION, 1];
    payload.extend_from_slice(&cursor.to_le_bytes());
    payload.extend_from_slice(&V3_VIEW_PHASE_COMPUTE_CEILING.to_le_bytes());
    instruction(
        sw::TAG_PUBLISH_VIEWS,
        payload,
        publication_accounts(authority, session, states_writable, false, true, true),
    )
}

fn run_view_phase(authority: Pubkey, session: Pubkey, cursor: u32) -> Instruction {
    run_view_phase_with_state_rights(authority, session, cursor, false)
}

fn run_view_phase_workspace_first(authority: Pubkey, session: Pubkey, cursor: u32) -> Instruction {
    let mut payload = vec![v3::WIRE_VERSION, 1];
    payload.extend_from_slice(&cursor.to_le_bytes());
    payload.extend_from_slice(&V3_VIEW_PHASE_COMPUTE_CEILING.to_le_bytes());
    instruction(
        sw::TAG_PUBLISH_VIEWS,
        payload,
        workspace_first_publication_accounts(authority, session, false, true, true),
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

fn abort_views(authority: Pubkey, session: Pubkey, cursor: u32) -> Instruction {
    let mut payload = vec![v3::WIRE_VERSION, 3];
    payload.extend_from_slice(&cursor.to_le_bytes());
    instruction(
        sw::TAG_PUBLISH_VIEWS,
        payload,
        vec![
            AccountMeta::new_readonly(authority, true),
            AccountMeta::new(session, false),
        ],
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

async fn account(context: &mut ProgramTestContext, key: Pubkey) -> Account {
    context
        .banks_client
        .get_account(key)
        .await
        .unwrap()
        .expect("expected account")
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

fn changed_bytes_within(before: &[u8], after: &[u8], allowed: &[(usize, usize)]) -> usize {
    assert_eq!(before.len(), after.len());
    let mut changed = 0;
    let mut unexpected = Vec::new();
    for (offset, (old, new)) in before.iter().zip(after).enumerate() {
        if old == new {
            continue;
        }
        if !allowed
            .iter()
            .any(|(start, end)| *start <= offset && offset < *end)
        {
            unexpected.push((offset, *old, *new));
        }
        changed += 1;
    }
    let mut unexpected_ranges: Vec<(usize, usize)> = Vec::new();
    for (offset, _, _) in &unexpected {
        if let Some((_, end)) = unexpected_ranges.last_mut() {
            if *end == *offset {
                *end += 1;
                continue;
            }
        }
        unexpected_ranges.push((*offset, *offset + 1));
    }
    assert!(
        unexpected.is_empty(),
        "unexpected writes: {} bytes in {} ranges, first 256 ranges: {:?}",
        unexpected.len(),
        unexpected_ranges.len(),
        &unexpected_ranges[..unexpected_ranges.len().min(256)]
    );
    changed
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

#[derive(Clone, Debug, Eq, PartialEq)]
struct TickObservation {
    cursor: u32,
    root: [u8; 32],
    st1_sha256: [u8; 32],
    mp1_sha256: [u8; 32],
}

#[derive(Debug)]
struct FrameObservation {
    pixel_sha256: [u8; 32],
    transaction_cu: Vec<u64>,
    workspace_bytes: usize,
    arena_high_water: u32,
    snapshot_payload_bytes: usize,
    abort_cached_strips: u8,
    abort_cache_cursor_refusals: u8,
    fresh_workspace_reset: bool,
    render_blockhash_retries: u32,
}

#[derive(Debug)]
struct CadenceObservation {
    ticks: Vec<TickObservation>,
    frames: Vec<FrameObservation>,
}

async fn capture_replay_accounts(
    context: &mut ProgramTestContext,
    session: Pubkey,
) -> Vec<(Pubkey, Account)> {
    let mut keys = vec![
        context.payer.pubkey(),
        session,
        stream_pda(&session),
        view_pda(&session, v3::WORKSPACE_ROLE),
        view_pda(&session, v3::SCRATCH_ROLE),
    ];
    for index in 0..3 {
        keys.push(state_pda(&session, index));
    }
    for role in 0..=8 {
        keys.push(view_pda(&session, role));
    }
    let mut snapshot = Vec::with_capacity(keys.len());
    for key in keys {
        snapshot.push((
            key,
            context
                .banks_client
                .get_account(key)
                .await
                .unwrap()
                .expect("replay account exists"),
        ));
    }
    snapshot
}

fn restore_replay_accounts(context: &mut ProgramTestContext, snapshot: &[(Pubkey, Account)]) {
    for (key, account) in snapshot {
        context.set_account(key, &AccountSharedData::from(account.clone()));
    }
}

async fn observe_tick(
    context: &mut ProgramTestContext,
    session: Pubkey,
    cursor: u32,
) -> TickObservation {
    let (st1, mp1) = state_accounts(context, session).await;
    TickObservation {
        cursor,
        root: sim2_state_root(&st1.data[128..], &mp1.data[128..]),
        st1_sha256: sha256(&[&st1.data[128..]]),
        mp1_sha256: sha256(&[&mp1.data[128..]]),
    }
}

async fn publish_observed_frame(
    context: &mut ProgramTestContext,
    authority: &Keypair,
    session: Pubkey,
    state_cursor: u32,
    abort_and_restart: bool,
) -> FrameObservation {
    let frame_bytes = SNAPSHOT_BYTES + STRIP_BYTES * 8;
    let mut transaction_cu = Vec::new();
    let mut render_blockhash_retries = 0u32;
    let mut abort_cached_strips = 0;
    let mut abort_cache_cursor_refusals = 0;
    let mut fresh_workspace_reset = false;
    if abort_and_restart {
        let output_views_before_abort = (0..=8)
            .map(|role| view_pda(&session, role))
            .collect::<Vec<_>>();
        let output_images_before_abort = {
            let mut images = Vec::with_capacity(output_views_before_abort.len());
            for key in &output_views_before_abort {
                images.push(
                    context
                        .banks_client
                        .get_account(*key)
                        .await
                        .unwrap()
                        .expect("output view before abort attempt"),
                );
            }
            images
        };
        transaction_cu.push(
            send(
                context,
                begin_views(authority.pubkey(), session, state_cursor),
                &[authority],
                Ok(()),
            )
            .await,
        );

        // Complete the snapshot stream, then execute two workspace-first
        // phases. They render both first strip caches, but the view outputs
        // stay unpublished because COMMIT_PHASE is not sent.
        let snapshot_strip_boundary = SNAPSHOT_BYTES / V3_VIEW_PHASE_BYTES * V3_VIEW_PHASE_BYTES;
        let mut cursor = 0u32;
        while cursor < snapshot_strip_boundary {
            let phase_count =
                ((snapshot_strip_boundary - cursor) / V3_VIEW_PHASE_BYTES).min(13) as usize;
            assert!(phase_count > 0);
            let phases = (0..phase_count)
                .map(|index| {
                    run_view_phase(
                        authority.pubkey(),
                        session,
                        cursor + index as u32 * V3_VIEW_PHASE_BYTES,
                    )
                })
                .collect();
            let (cu, _) = send_many(context, phases, &[authority]).await;
            transaction_cu.push(cu);
            cursor += phase_count as u32 * V3_VIEW_PHASE_BYTES;
        }
        let workspace_key = view_pda(&session, v3::WORKSPACE_ROLE);
        let mut snapshot_workspace = context
            .banks_client
            .get_account(workspace_key)
            .await
            .unwrap()
            .expect("workspace after snapshot render");
        let snapshot_cursor_offset = ACCOUNT_HEADER_BYTES + V3_SNAPSHOT_CACHE_OFFSET + 20;
        assert_eq!(
            u32_at(&snapshot_workspace.data, snapshot_cursor_offset),
            state_cursor
        );
        put_u32(
            &mut snapshot_workspace.data,
            snapshot_cursor_offset,
            state_cursor ^ 1,
        );
        context.set_account(
            &workspace_key,
            &AccountSharedData::from(snapshot_workspace.clone()),
        );
        transaction_cu.push(
            send(
                context,
                run_view_phase_workspace_first(
                    authority.pubkey(),
                    session,
                    snapshot_strip_boundary,
                ),
                &[authority],
                Err(v3::REFUSAL_KERNEL),
            )
            .await,
        );
        abort_cache_cursor_refusals += 1;
        put_u32(
            &mut snapshot_workspace.data,
            snapshot_cursor_offset,
            state_cursor,
        );
        context.set_account(&workspace_key, &AccountSharedData::from(snapshot_workspace));
        transaction_cu.push(
            send(
                context,
                run_view_phase_workspace_first(
                    authority.pubkey(),
                    session,
                    snapshot_strip_boundary,
                ),
                &[authority],
                Ok(()),
            )
            .await,
        );

        let first_strip_cache_start =
            ACCOUNT_HEADER_BYTES + V3_SNAPSHOT_CACHE_OFFSET + V3_SNAPSHOT_CACHE_BYTES;
        let first_strip_cursor_offset = first_strip_cache_start + 20;
        let first_strip_workspace = context
            .banks_client
            .get_account(workspace_key)
            .await
            .unwrap()
            .expect("workspace after first strip render");
        assert_eq!(
            u32_at(&first_strip_workspace.data, first_strip_cursor_offset),
            state_cursor
        );
        let mut corrupted_strip_workspace = first_strip_workspace.clone();
        put_u32(
            &mut corrupted_strip_workspace.data,
            first_strip_cursor_offset,
            state_cursor ^ 1,
        );
        context.set_account(
            &workspace_key,
            &AccountSharedData::from(corrupted_strip_workspace),
        );
        transaction_cu.push(
            send(
                context,
                run_view_phase_workspace_first(
                    authority.pubkey(),
                    session,
                    snapshot_strip_boundary + V3_VIEW_PHASE_BYTES,
                ),
                &[authority],
                Err(v3::REFUSAL_KERNEL),
            )
            .await,
        );
        abort_cache_cursor_refusals += 1;
        context.set_account(
            &workspace_key,
            &AccountSharedData::from(first_strip_workspace),
        );
        transaction_cu.push(
            send(
                context,
                run_view_phase_workspace_first(
                    authority.pubkey(),
                    session,
                    snapshot_strip_boundary + V3_VIEW_PHASE_BYTES,
                ),
                &[authority],
                Ok(()),
            )
            .await,
        );

        let partial_workspace = context
            .banks_client
            .get_account(workspace_key)
            .await
            .unwrap()
            .expect("workspace after partial strip render");
        let strip_cache_start =
            ACCOUNT_HEADER_BYTES + V3_SNAPSHOT_CACHE_OFFSET + V3_SNAPSHOT_CACHE_BYTES;
        for strip_id in 0..2usize {
            let strip_start = strip_cache_start + strip_id * STRIP_BYTES as usize;
            assert_eq!(
                &partial_workspace.data[strip_start..strip_start + 8],
                b"DOOM-RS1"
            );
        }
        abort_cached_strips = 2;
        transaction_cu.push(
            send(
                context,
                abort_views(authority.pubkey(), session, state_cursor),
                &[authority],
                Ok(()),
            )
            .await,
        );
        for (key, before) in output_views_before_abort
            .iter()
            .zip(output_images_before_abort.iter())
        {
            let after = context
                .banks_client
                .get_account(*key)
                .await
                .unwrap()
                .expect("output view after aborted partial render");
            assert_eq!(
                after.data, before.data,
                "ABORT_PHASE must not publish a view"
            );
        }

        // Start the comparison render from the workspace's zero-initialized
        // payload, preserving its authenticated child header and cursor.
        let mut fresh_workspace = partial_workspace;
        fresh_workspace.data[ACCOUNT_HEADER_BYTES..].fill(0);
        context.set_account(&workspace_key, &AccountSharedData::from(fresh_workspace));
        fresh_workspace_reset = true;
    }

    let primary_before = context
        .banks_client
        .get_account(state_pda(&session, 0))
        .await
        .unwrap()
        .expect("primary state before observed frame");
    transaction_cu.push(
        send(
            context,
            begin_views(authority.pubkey(), session, state_cursor),
            &[authority],
            Ok(()),
        )
        .await,
    );

    let boundary = SNAPSHOT_BYTES / V3_VIEW_PHASE_BYTES * V3_VIEW_PHASE_BYTES;
    let mut view_cursor = 0u32;
    while view_cursor < frame_bytes {
        let remaining = (frame_bytes - view_cursor + V3_VIEW_PHASE_BYTES - 1) / V3_VIEW_PHASE_BYTES;
        let layout_remaining = if view_cursor < boundary {
            (boundary - view_cursor) / V3_VIEW_PHASE_BYTES
        } else {
            remaining
        };
        let mut candidate = remaining.min(layout_remaining).min(13);
        assert!(candidate > 0);
        let mut blockhash_retries_for_cursor = 0u8;
        loop {
            let blockhash = context.get_new_latest_blockhash().await.unwrap();
            let phase_instructions = (0..candidate)
                .map(|index| {
                    let cursor = view_cursor + index * V3_VIEW_PHASE_BYTES;
                    if cursor < boundary {
                        run_view_phase(authority.pubkey(), session, cursor)
                    } else {
                        run_view_phase_workspace_first(authority.pubkey(), session, cursor)
                    }
                })
                .collect::<Vec<_>>();
            let mut instructions = Vec::with_capacity(phase_instructions.len() + 1);
            instructions.push(ComputeBudgetInstruction::set_compute_unit_limit(
                TRANSACTION_COMPUTE_LIMIT,
            ));
            instructions.extend(phase_instructions);
            let signers = vec![&context.payer, authority];
            let tx = Transaction::new(
                &signers,
                solana_message::Message::new(&instructions, Some(&context.payer.pubkey())),
                blockhash,
            );
            if bincode::serialized_size(&tx).unwrap() as usize > TRANSACTION_PACKET_LIMIT {
                assert!(candidate > 1, "one render phase exceeds packet limit");
                candidate -= 1;
                continue;
            }
            let simulation = context
                .banks_client
                .simulate_transaction(tx.clone())
                .await
                .unwrap();
            let result = simulation.result.expect("render simulation result");
            if matches!(&result, Err(TransactionError::BlockhashNotFound)) {
                assert!(
                    blockhash_retries_for_cursor < 3,
                    "render simulation repeatedly rejected a refreshed blockhash at cursor {view_cursor}"
                );
                blockhash_retries_for_cursor += 1;
                render_blockhash_retries += 1;
                continue;
            }
            let logs = simulation
                .simulation_details
                .as_ref()
                .map(|details| details.logs.join("\n"))
                .unwrap_or_default();
            let over_limit = matches!(
                &result,
                Err(TransactionError::InstructionError(
                    _,
                    InstructionError::ComputationalBudgetExceeded
                ))
            ) || (matches!(
                &result,
                Err(TransactionError::InstructionError(
                    _,
                    InstructionError::ProgramFailedToComplete
                ))
            ) && logs.contains("exceeded CUs meter"));
            if over_limit {
                assert!(candidate > 1, "one render phase exceeds CU limit");
                candidate -= 1;
                continue;
            }
            if let Err(error) = result {
                panic!("render simulation failed: {error:?}\n{logs}");
            }
            let simulated_cu = simulation
                .simulation_details
                .as_ref()
                .expect("render simulation details")
                .units_consumed;
            // ProgramTest can advance its bank while simulating this large
            // render transaction. Sign the execution copy against the latest
            // bank hash instead of reusing the simulation transaction's hash.
            drop(signers);
            let execution_blockhash = context.get_new_latest_blockhash().await.unwrap();
            let execution_signers = vec![&context.payer, authority];
            let execution_tx = Transaction::new(
                &execution_signers,
                solana_message::Message::new(&instructions, Some(&context.payer.pubkey())),
                execution_blockhash,
            );
            let execution = context
                .banks_client
                .process_transaction_with_metadata(execution_tx)
                .await
                .unwrap();
            if matches!(&execution.result, Err(TransactionError::BlockhashNotFound)) {
                assert!(
                    blockhash_retries_for_cursor < 3,
                    "render execution repeatedly rejected a refreshed blockhash at cursor {view_cursor}"
                );
                blockhash_retries_for_cursor += 1;
                render_blockhash_retries += 1;
                continue;
            }
            if let Err(error) = execution.result {
                let logs = execution
                    .metadata
                    .as_ref()
                    .map(|meta| meta.log_messages.join("\n"))
                    .unwrap_or_default();
                panic!("render transaction failed: {error:?}\n{logs}");
            }
            let used = execution
                .metadata
                .expect("render transaction metadata")
                .compute_units_consumed;
            assert_eq!(used, simulated_cu);
            assert!(used <= TRANSACTION_COMPUTE_LIMIT as u64);
            transaction_cu.push(used);
            view_cursor = (view_cursor + candidate * V3_VIEW_PHASE_BYTES).min(frame_bytes);
            break;
        }
    }
    transaction_cu.push(
        send(
            context,
            commit_views(authority.pubkey(), session, state_cursor, frame_bytes),
            &[authority],
            Ok(()),
        )
        .await,
    );

    let primary_after = context
        .banks_client
        .get_account(state_pda(&session, 0))
        .await
        .unwrap()
        .expect("primary state after observed frame");
    assert_eq!(primary_before.data, primary_after.data);
    let mut strips = Vec::with_capacity(8);
    for role in 1..=8 {
        let account = context
            .banks_client
            .get_account(view_pda(&session, role))
            .await
            .unwrap()
            .expect("committed frame strip");
        assert_eq!(&account.data[128..136], b"DOOM-RS1");
        strips.push(account.data[256..].to_vec());
    }
    let mut pixels = Vec::with_capacity(320 * 200);
    for y in 0..200 {
        for strip in &strips {
            pixels.extend_from_slice(&strip[y * 40..(y + 1) * 40]);
        }
    }
    let workspace = context
        .banks_client
        .get_account(view_pda(&session, v3::WORKSPACE_ROLE))
        .await
        .unwrap()
        .expect("workspace after observed frame");
    let high_water = u32_at(
        &workspace.data,
        ACCOUNT_HEADER_BYTES + V3_RENDER_ARENA_HIGH_WATER_OFFSET as usize,
    );
    assert!(high_water <= V3_RENDER_ARENA_MAX_BYTES);
    let snapshot_payload_bytes = u32_at(
        &workspace.data,
        ACCOUNT_HEADER_BYTES + V3_SNAPSHOT_CACHE_OFFSET + 28,
    ) as usize;
    FrameObservation {
        pixel_sha256: sha256(&[&pixels]),
        transaction_cu,
        workspace_bytes: workspace.data.len(),
        arena_high_water: high_water,
        snapshot_payload_bytes,
        abort_cached_strips,
        abort_cache_cursor_refusals,
        fresh_workspace_reset,
        render_blockhash_retries,
    }
}

async fn run_80_tic_trace(
    context: &mut ProgramTestContext,
    authority: &Keypair,
    session: Pubkey,
    render_every_8: bool,
    abort_and_rerender: bool,
) -> CadenceObservation {
    let mut ticks = Vec::with_capacity(80);
    let mut frames = Vec::new();
    for batch_start in (0..80u32).step_by(8) {
        let first_input = if batch_start == 0 {
            1 // sequence zero was preloaded before initialization
        } else {
            batch_start + 1
        };
        let inputs = (first_input..batch_start + 9)
            .map(|sequence| {
                write_input(
                    authority.pubkey(),
                    session,
                    sequence,
                    command_at(sequence as usize),
                )
            })
            .collect::<Vec<_>>();
        send_many(context, inputs, &[authority]).await;
        for sequence in batch_start..batch_start + 8 {
            send(
                context,
                advance(authority.pubkey(), session, sequence, 1),
                &[authority],
                Ok(()),
            )
            .await;
            let cursor = sequence + 1;
            ticks.push(observe_tick(context, session, cursor).await);
            if render_every_8 && cursor % 8 == 0 {
                frames.push(
                    publish_observed_frame(context, authority, session, cursor, abort_and_rerender)
                        .await,
                );
            }
        }
    }
    CadenceObservation { ticks, frames }
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

    // Keep the ring backlog within the initial 512-slot allocation. Commands are
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

    let (view_create_cu, view_create_instruction_cu, view_grow_cu, view_grow_instruction_cu) =
        create_and_grow_views(&mut context, session).await;
    let workspace_rent_lamports = context
        .banks_client
        .get_account(view_pda(&session, v3::WORKSPACE_ROLE))
        .await
        .unwrap()
        .expect("workspace account after allocation")
        .lamports;
    let replay_baseline = capture_replay_accounts(&mut context, session).await;

    let no_render_trace = run_80_tic_trace(&mut context, &authority, session, false, false).await;
    assert_eq!(no_render_trace.ticks.len(), 80);
    assert!(no_render_trace.frames.is_empty());
    restore_replay_accounts(&mut context, &replay_baseline);

    let cadence_trace = run_80_tic_trace(&mut context, &authority, session, true, false).await;
    assert_eq!(cadence_trace.frames.len(), 10);
    assert_eq!(no_render_trace.ticks, cadence_trace.ticks);
    let cadence_frame_hashes = cadence_trace
        .frames
        .iter()
        .map(|frame| frame.pixel_sha256)
        .collect::<Vec<_>>();
    let workspace_allocation = ACCOUNT_HEADER_BYTES + V3_VIEW_WORKSPACE_BYTES as usize;
    assert!(cadence_trace
        .frames
        .iter()
        .all(|frame| frame.workspace_bytes == workspace_allocation));
    assert!(cadence_trace
        .frames
        .iter()
        .all(|frame| frame.arena_high_water <= V3_RENDER_ARENA_MAX_BYTES));
    restore_replay_accounts(&mut context, &replay_baseline);

    let abort_rerender_trace =
        run_80_tic_trace(&mut context, &authority, session, true, true).await;
    assert_eq!(abort_rerender_trace.frames.len(), 10);
    assert_eq!(cadence_trace.ticks, abort_rerender_trace.ticks);
    let abort_frame_hashes = abort_rerender_trace
        .frames
        .iter()
        .map(|frame| frame.pixel_sha256)
        .collect::<Vec<_>>();
    assert_eq!(cadence_frame_hashes, abort_frame_hashes);
    assert!(abort_rerender_trace
        .frames
        .iter()
        .all(|frame| frame.workspace_bytes == workspace_allocation));
    assert!(abort_rerender_trace
        .frames
        .iter()
        .all(|frame| frame.abort_cached_strips == 2));
    assert!(abort_rerender_trace
        .frames
        .iter()
        .all(|frame| frame.abort_cache_cursor_refusals == 2));
    assert!(abort_rerender_trace
        .frames
        .iter()
        .all(|frame| frame.fresh_workspace_reset));
    assert!(abort_rerender_trace
        .frames
        .iter()
        .skip(1)
        .zip(cadence_trace.frames.iter().skip(1))
        .all(|(fresh, reused)| fresh.pixel_sha256 == reused.pixel_sha256));
    assert!(cadence_trace
        .frames
        .iter()
        .all(|frame| { frame.snapshot_payload_bytes + 128 <= V3_SNAPSHOT_CACHE_BYTES }));
    assert!(abort_rerender_trace
        .frames
        .iter()
        .all(|frame| { frame.snapshot_payload_bytes + 128 <= V3_SNAPSHOT_CACHE_BYTES }));
    let cadence_frame_transaction_count = cadence_trace
        .frames
        .iter()
        .map(|frame| frame.transaction_cu.len() as u64)
        .sum::<u64>();
    let cadence_frame_cu = cadence_trace
        .frames
        .iter()
        .flat_map(|frame| frame.transaction_cu.iter().copied())
        .sum::<u64>();
    let cadence_transactions_per_frame =
        cadence_frame_transaction_count as f64 / cadence_trace.frames.len() as f64;
    let estimated_pictures_per_second_at_60_tps = 60.0 / (cadence_transactions_per_frame + 9.0); // 8 advances + 1 batched input write
    restore_replay_accounts(&mut context, &replay_baseline);

    let mut adv_cu = Vec::new();
    let mut stream_grow_cu = Vec::new();
    let mut cursor = 0u32;
    let mut stream_capacity = STREAM_CAPACITY;
    for boundary in golden["sim2"]["sweep_boundaries"].as_array().unwrap() {
        let target = boundary["cursor"].as_u64().unwrap() as u32;
        while cursor < target {
            if cursor == stream_capacity && cursor < target {
                let new_capacity =
                    (stream_capacity + v3::MAX_STREAM_GROWTH_SLOTS).min(STREAM_FINAL_CAPACITY);
                let cu = send(
                    &mut context,
                    grow_stream(payer, session, new_capacity),
                    &[],
                    Ok(()),
                )
                .await;
                stream_grow_cu.push(json!({
                    "old_capacity": stream_capacity,
                    "new_capacity": new_capacity,
                    "cursor": cursor,
                    "cu": cu
                }));
                stream_capacity = new_capacity;
            }
            let write_target = target.min(cursor.saturating_add(8)).min(stream_capacity);
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

    let frame_bytes = SNAPSHOT_BYTES + STRIP_BYTES * 8;
    let view_begin_cu = send(
        &mut context,
        begin_views(authority.pubkey(), session, cursor),
        &[&authority],
        Ok(()),
    )
    .await;
    let writable_state_view_attempt_cu = send(
        &mut context,
        run_view_phase_with_state_rights(authority.pubkey(), session, 0, true),
        &[&authority],
        // DCG accepts writable state-account privileges in a view message;
        // Doom still rejects the attempt in its own kernel validator.
        Err(v3::REFUSAL_KERNEL),
    )
    .await;
    let context_before_render = context
        .banks_client
        .get_account(state_pda(&session, 0))
        .await
        .unwrap()
        .expect("primary state before view phases");
    let st1_before_render = context
        .banks_client
        .get_account(state_pda(&session, 1))
        .await
        .unwrap()
        .expect("ST1 before view phases");
    let mp1_before_render = context
        .banks_client
        .get_account(state_pda(&session, 2))
        .await
        .unwrap()
        .expect("MP1 before view phases");
    let resource_before_render = context
        .banks_client
        .get_account(resource_pda(&session))
        .await
        .unwrap()
        .expect("sealed resource before view phases");
    let workspace_before_render = context
        .banks_client
        .get_account(view_pda(&session, v3::WORKSPACE_ROLE))
        .await
        .unwrap()
        .expect("workspace after begin and before view phases");
    let scratch_before_render = context
        .banks_client
        .get_account(view_pda(&session, v3::SCRATCH_ROLE))
        .await
        .unwrap()
        .expect("publication scratch after begin and before view phases");
    let mut output_views_before_render = Vec::with_capacity(9);
    for role in 0..=8 {
        output_views_before_render.push(
            context
                .banks_client
                .get_account(view_pda(&session, role))
                .await
                .unwrap()
                .expect("view account before render"),
        );
    }
    let mut view_phase_cu = Vec::new();
    let mut view_transaction_cu = Vec::new();
    let mut view_batch_sizes = Vec::new();
    let mut view_transaction_bytes = Vec::new();
    let mut view_cursor = 0u32;
    let workspace_first_boundary = SNAPSHOT_BYTES / V3_VIEW_PHASE_BYTES * V3_VIEW_PHASE_BYTES;
    while view_cursor < frame_bytes {
        let remaining_phases =
            (frame_bytes - view_cursor + V3_VIEW_PHASE_BYTES - 1) / V3_VIEW_PHASE_BYTES;
        let mut candidate = if view_cursor < workspace_first_boundary {
            remaining_phases.min((workspace_first_boundary - view_cursor) / V3_VIEW_PHASE_BYTES)
        } else {
            remaining_phases
        };
        assert!(candidate > 0, "view batch crossed workspace-first boundary");
        loop {
            let blockhash = context.get_new_latest_blockhash().await.unwrap();
            let phase_instructions = (0..candidate)
                .map(|index| {
                    let cursor = view_cursor + index * V3_VIEW_PHASE_BYTES;
                    if cursor < workspace_first_boundary {
                        run_view_phase(authority.pubkey(), session, cursor)
                    } else {
                        run_view_phase_workspace_first(authority.pubkey(), session, cursor)
                    }
                })
                .collect::<Vec<_>>();
            let mut instructions = Vec::with_capacity(phase_instructions.len() + 1);
            instructions.push(ComputeBudgetInstruction::set_compute_unit_limit(
                TRANSACTION_COMPUTE_LIMIT,
            ));
            instructions.extend(phase_instructions);
            let signers = vec![&context.payer, &authority];
            let tx = Transaction::new(
                &signers,
                solana_message::Message::new(&instructions, Some(&context.payer.pubkey())),
                blockhash,
            );
            let tx_bytes = bincode::serialized_size(&tx).unwrap() as usize;
            if tx_bytes > TRANSACTION_PACKET_LIMIT {
                assert!(candidate > 1, "one view phase exceeds the packet limit");
                candidate -= 1;
                continue;
            }
            let simulation = context
                .banks_client
                .simulate_transaction(tx.clone())
                .await
                .unwrap();
            let simulation_result = simulation
                .result
                .expect("ProgramTest returns a view-batch simulation result");
            let simulation_logs = simulation
                .simulation_details
                .as_ref()
                .map(|details| details.logs.join("\n"))
                .unwrap_or_default();
            let instruction_cu_limit = matches!(
                &simulation_result,
                Err(TransactionError::InstructionError(
                    _,
                    InstructionError::ComputationalBudgetExceeded
                ))
            ) || (matches!(
                &simulation_result,
                Err(TransactionError::InstructionError(
                    _,
                    InstructionError::ProgramFailedToComplete
                ))
            ) && simulation_logs.contains("exceeded CUs meter"));
            if instruction_cu_limit {
                assert!(candidate > 1, "one view phase exceeds the CU limit");
                candidate -= 1;
                continue;
            }
            if let Err(error) = simulation_result {
                panic!("view-batch simulation failed: {error:?}\n{simulation_logs}");
            }
            let simulated_cu = simulation
                .simulation_details
                .as_ref()
                .expect("ProgramTest returns simulation details")
                .units_consumed;
            assert!(simulated_cu <= TRANSACTION_COMPUTE_LIMIT as u64);

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
                panic!("view-batch transaction failed: {error:?}\n{logs}");
            }
            let metadata = result.metadata.expect("view-batch transaction metadata");
            assert_eq!(metadata.compute_units_consumed, simulated_cu);
            assert!(metadata.compute_units_consumed <= TRANSACTION_COMPUTE_LIMIT as u64);
            let program_prefix = format!("Program {PROGRAM} consumed ");
            let instruction_cu = metadata
                .log_messages
                .iter()
                .filter_map(|line| line.strip_prefix(&program_prefix))
                .filter_map(|line| line.split_once(" of "))
                .filter_map(|(used, _)| used.parse::<u64>().ok())
                .collect::<Vec<_>>();
            assert_eq!(instruction_cu.len(), candidate as usize);
            for (index, cu) in instruction_cu.iter().enumerate() {
                view_phase_cu.push(json!({
                    "cursor": view_cursor + index as u32 * V3_VIEW_PHASE_BYTES,
                    "cu": cu,
                }));
            }
            view_cursor = (view_cursor + candidate * V3_VIEW_PHASE_BYTES).min(frame_bytes);
            view_transaction_cu.push(metadata.compute_units_consumed);
            view_batch_sizes.push(candidate);
            view_transaction_bytes.push(tx_bytes);
            break;
        }
    }
    assert_eq!(view_cursor, frame_bytes);

    let context_after_render = context
        .banks_client
        .get_account(state_pda(&session, 0))
        .await
        .unwrap()
        .expect("primary state after view phases");
    let st1_after_render = context
        .banks_client
        .get_account(state_pda(&session, 1))
        .await
        .unwrap()
        .expect("ST1 after view phases");
    let mp1_after_render = context
        .banks_client
        .get_account(state_pda(&session, 2))
        .await
        .unwrap()
        .expect("MP1 after view phases");
    let resource_after_render = context
        .banks_client
        .get_account(resource_pda(&session))
        .await
        .unwrap()
        .expect("sealed resource after view phases");
    let context_render_writes = changed_bytes_within(
        &context_before_render.data,
        &context_after_render.data,
        &[(0, ENGINE_CONTEXT_BYTES as usize)],
    );
    assert_eq!(
        context_before_render.data.len(),
        ENGINE_CONTEXT_BYTES as usize
    );
    assert_eq!(st1_before_render.data, st1_after_render.data);
    assert_eq!(mp1_before_render.data, mp1_after_render.data);
    assert_eq!(resource_before_render.data, resource_after_render.data);
    let workspace_after_render = context
        .banks_client
        .get_account(view_pda(&session, v3::WORKSPACE_ROLE))
        .await
        .unwrap()
        .expect("workspace after view phases");
    assert_eq!(
        workspace_before_render.data[..ACCOUNT_HEADER_BYTES],
        workspace_after_render.data[..ACCOUNT_HEADER_BYTES]
    );
    assert_eq!(
        workspace_after_render.data.len(),
        ACCOUNT_HEADER_BYTES + V3_VIEW_WORKSPACE_BYTES as usize
    );
    let workspace_changed_bytes = changed_bytes_within(
        &workspace_before_render.data,
        &workspace_after_render.data,
        &[(ACCOUNT_HEADER_BYTES, workspace_after_render.data.len())],
    );
    let mut disallowed_workspace_probe = workspace_after_render.data.clone();
    disallowed_workspace_probe[ACCOUNT_HEADER_BYTES - 1] ^= 1;
    assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        changed_bytes_within(
            &workspace_before_render.data,
            &disallowed_workspace_probe,
            &[(ACCOUNT_HEADER_BYTES, workspace_after_render.data.len())],
        )
    }))
    .is_err());
    let scratch_after_render = context
        .banks_client
        .get_account(view_pda(&session, v3::SCRATCH_ROLE))
        .await
        .unwrap()
        .expect("publication scratch after view phases");
    let scratch_header_changed_bytes = changed_bytes_within(
        &scratch_before_render.data[..ACCOUNT_HEADER_BYTES],
        &scratch_after_render.data[..ACCOUNT_HEADER_BYTES],
        &[(ACCOUNT_CURSOR_OFFSET, ACCOUNT_CURSOR_OFFSET + 4)],
    );
    assert_eq!(
        u32::from_le_bytes(
            scratch_after_render.data[ACCOUNT_CURSOR_OFFSET..ACCOUNT_CURSOR_OFFSET + 4]
                .try_into()
                .unwrap()
        ),
        cursor
    );
    assert_eq!(
        scratch_after_render.data.len(),
        ACCOUNT_HEADER_BYTES + frame_bytes as usize
    );
    let scratch_changed_bytes = changed_bytes_within(
        &scratch_before_render.data[ACCOUNT_HEADER_BYTES..],
        &scratch_after_render.data[ACCOUNT_HEADER_BYTES..],
        &[(0, scratch_after_render.data.len() - ACCOUNT_HEADER_BYTES)],
    );
    let mut output_views_after_render = Vec::with_capacity(9);
    for role in 0..=8 {
        output_views_after_render.push(
            context
                .banks_client
                .get_account(view_pda(&session, role))
                .await
                .unwrap()
                .expect("view account after render, before commit"),
        );
    }
    for (before, after) in output_views_before_render
        .iter()
        .zip(output_views_after_render.iter())
    {
        assert_eq!(before.data, after.data, "RUN_PHASE must not publish output");
    }
    let view_commit_cu = send(
        &mut context,
        commit_views(authority.pubkey(), session, cursor, frame_bytes),
        &[&authority],
        Ok(()),
    )
    .await;
    let context_after_commit = context
        .banks_client
        .get_account(state_pda(&session, 0))
        .await
        .unwrap()
        .expect("primary state after publication commit");
    let st1_after_commit = context
        .banks_client
        .get_account(state_pda(&session, 1))
        .await
        .unwrap()
        .expect("ST1 after publication commit");
    let mp1_after_commit = context
        .banks_client
        .get_account(state_pda(&session, 2))
        .await
        .unwrap()
        .expect("MP1 after publication commit");
    assert_eq!(context_after_render.data, context_after_commit.data);
    assert_eq!(st1_after_render.data, st1_after_commit.data);
    assert_eq!(mp1_after_render.data, mp1_after_commit.data);
    let mut frame_transaction_cu = vec![view_begin_cu];
    frame_transaction_cu.extend_from_slice(&view_transaction_cu);
    frame_transaction_cu.push(view_commit_cu);
    let frame_total_cu = frame_transaction_cu.iter().sum::<u64>();
    let mut strips = Vec::with_capacity(8);
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
        strips.push(strip_pixels.to_vec());
    }
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
    let mut pixels = Vec::with_capacity(320 * 200);
    for y in 0..200 {
        for strip in &strips {
            pixels.extend_from_slice(&strip[y * 40..(y + 1) * 40]);
        }
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
    let workspace_key = view_pda(&session, v3::WORKSPACE_ROLE);
    let workspace_before_close = account(&mut context, workspace_key).await;
    let authority_balance_before_close = context
        .banks_client
        .get_account(authority.pubkey())
        .await
        .unwrap()
        .map(|account| account.lamports)
        .unwrap_or(0);
    let workspace_reclaim_cu = send(
        &mut context,
        close_child(
            session,
            workspace_key,
            authority.pubkey(),
            v3::KIND_WORKSPACE,
        ),
        &[],
        Ok(()),
    )
    .await;
    let authority_balance_after_close = context
        .banks_client
        .get_account(authority.pubkey())
        .await
        .unwrap()
        .map(|account| account.lamports)
        .unwrap_or(0);
    let workspace_rent_returned_lamports =
        authority_balance_after_close - authority_balance_before_close;
    assert_eq!(
        workspace_rent_returned_lamports,
        workspace_before_close.lamports
    );
    assert!(context
        .banks_client
        .get_account(workspace_key)
        .await
        .unwrap()
        .is_none());

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
            "schema": "doom-dcg-r7-programtest/2",
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
                "stream_growth_transactions": stream_grow_cu,
                "compute_units_by_transaction": adv_cu,
            },
            "frame": {
                "bytes": frame_bytes,
                "view_phase_bytes": V3_VIEW_PHASE_BYTES,
                "run_phase_count": view_phase_cu.len(),
                "transactions_per_frame": view_transaction_cu.len() + 2,
                "phase_compute_units": view_phase_cu,
                "run_phase_transactions": view_transaction_cu,
                "phases_per_run_transaction": view_batch_sizes,
                "serialized_transaction_bytes": view_transaction_bytes,
                "compute_units_per_transaction": frame_transaction_cu,
                "compute_units_total": frame_total_cu,
                "compute_units_mean_per_transaction": frame_total_cu as f64
                    / (view_batch_sizes.len() + 2) as f64,
                "snapshot_create_cu": view_begin_cu,
                "writable_state_view_attempt_cu": writable_state_view_attempt_cu,
                "publish_commit_cu": view_commit_cu,
                "state_write_audit": {
                    "context_allowed_range": [0, ENGINE_CONTEXT_BYTES],
                    "context_changed_bytes": context_render_writes,
                    "st1_unchanged": true,
                    "mp1_unchanged": true,
                    "resource_unchanged": true,
                    "workspace_changed_bytes": workspace_changed_bytes,
                    "publication_scratch_header_changed_bytes": scratch_header_changed_bytes,
                    "publication_scratch_changed_bytes": scratch_changed_bytes,
                    "view_outputs_unchanged_until_commit": true,
                },
                "pixel_sha256": frame_hash,
            },
            "render_cadence": {
                "tape_tics": 80,
                "render_every_tics": 8,
                "frames_in_a_row": cadence_trace.frames.len(),
                "same_st1_mp1_roots_after_every_tic": true,
                "same_st1_mp1_account_hashes_after_every_tic": true,
                "same_rendered_frame_hashes_after_abort_fresh_workspace_rerender": true,
                "aborted_partial_render_cached_strips": abort_rerender_trace.frames.iter().map(|f| f.abort_cached_strips).collect::<Vec<_>>(),
                "aborted_cache_cursor_refusals": abort_rerender_trace.frames.iter().map(|f| f.abort_cache_cursor_refusals).collect::<Vec<_>>(),
                "fresh_workspace_reset_before_comparison_render": abort_rerender_trace.frames.iter().map(|f| f.fresh_workspace_reset).collect::<Vec<_>>(),
                "frames_2_through_10_match_fresh_workspace_render": abort_rerender_trace.frames.iter().skip(1).zip(cadence_trace.frames.iter().skip(1)).all(|(fresh, reused)| fresh.pixel_sha256 == reused.pixel_sha256),
                "state_roots_after_each_tic": cadence_trace.ticks.iter().map(|tick| json!({
                    "cursor": tick.cursor,
                    "root": tick.root,
                    "st1_sha256": tick.st1_sha256,
                    "mp1_sha256": tick.mp1_sha256,
                })).collect::<Vec<_>>(),
                "frame_hashes": cadence_frame_hashes,
                "abort_rerender_frame_hashes": abort_frame_hashes,
                "abort_rerender_transaction_counts": abort_rerender_trace.frames.iter().map(|f| f.transaction_cu.len()).collect::<Vec<_>>(),
                "render_blockhash_retries_per_frame": cadence_trace.frames.iter().map(|f| f.render_blockhash_retries).collect::<Vec<_>>(),
                "transactions_per_frame_average": cadence_transactions_per_frame,
                "transactions_for_ten_frames": cadence_frame_transaction_count,
                "compute_units_for_ten_frames_including_snapshot_seed": cadence_frame_cu,
                "snapshot_cache_bytes": V3_SNAPSHOT_CACHE_BYTES,
                "snapshot_payload_bytes_per_frame": cadence_trace.frames.iter().map(|f| f.snapshot_payload_bytes).collect::<Vec<_>>(),
                "snapshot_payload_and_header_bytes_per_frame": cadence_trace.frames.iter().map(|f| f.snapshot_payload_bytes + 128).collect::<Vec<_>>(),
                "snapshot_cache_exceeded_frame_numbers": cadence_trace.frames.iter().enumerate().filter_map(|(index, f)| (f.snapshot_payload_bytes + 128 > V3_SNAPSHOT_CACHE_BYTES).then_some(index + 1)).collect::<Vec<_>>(),
                "snapshot_cache_oversize_behavior": "render callback returns InvalidInput, mapped by DCG to REFUSAL_KERNEL; RUN_PHASE transaction rolls back",
                "workspace_account_bytes": workspace_allocation,
                "workspace_bytes_after_each_frame": cadence_trace.frames.iter().map(|f| f.workspace_bytes).collect::<Vec<_>>(),
                "render_arena_high_water_bytes": cadence_trace.frames.iter().map(|f| f.arena_high_water).collect::<Vec<_>>(),
                "workspace_rent_lamports": workspace_rent_lamports,
                "estimated_pictures_per_second_at_60_transactions_per_second": estimated_pictures_per_second_at_60_tps,
            },
            "workspace_reclaim_after_halt": {
                "close_compute_units": workspace_reclaim_cu,
                "lamports_in_workspace_before_close": workspace_before_close.lamports,
                "lamports_received_by_session_authority": workspace_rent_returned_lamports,
                "workspace_account_closed": true,
            },
            "adversarial": {
                "unsealed_resource_begin_cu": unsealed_begin_cu,
                "advance_before_init_cu": early_advance_cu,
                "wrong_primary_cu": wrong_primary_cu,
                "engine_halt_cu": engine_halt_cu,
                "halted_session_advance_cu": after_halt_advance_cu,
                "wrong_wad_cu": bad_wad_cu,
                "writable_state_view_attempt_cu": writable_state_view_attempt_cu,
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
