#![cfg(feature = "sbf-programtest")]

//! Dispatch seam proof on the composed SBF image. This test intentionally
//! stops before Doom state initialization; the round-5 report explains why.

use dcg_program::{hash::sha256, stateful as sw, stateful::v2};
use doom_dcg::{DOOM_KERNEL_ID, RESOURCE_KEY, RESOURCE_SCHEMA};
use solana_account::{Account, AccountSharedData};
use solana_instruction::{account_meta::AccountMeta, Instruction};
use solana_keypair::Keypair;
use solana_program::{pubkey::Pubkey, system_program};
use solana_program_test::{ProgramTest, ProgramTestContext};
use solana_signer::Signer;
use solana_transaction::Transaction;
use solana_transaction_error::TransactionError;

const PROGRAM: Pubkey = Pubkey::new_from_array([0xd9; 32]);
const RESOURCE: Pubkey = Pubkey::new_from_array(RESOURCE_KEY);
const RESOURCE_BYTES: &[u8] = b"programtest dispatch-only resource placeholder";

fn session_pda(authority: &Pubkey, id: u64) -> Pubkey {
    Pubkey::find_program_address(
        &[b"dcg-session-v2", authority.as_ref(), &id.to_le_bytes()],
        &PROGRAM,
    )
    .0
}

fn stream_pda(session: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(&[b"dcg-input-v2", session.as_ref()], &PROGRAM).0
}

fn state_pda(session: &Pubkey, index: u8) -> Pubkey {
    Pubkey::find_program_address(&[b"dcg-state-v2", session.as_ref(), &[index]], &PROGRAM).0
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

fn open_session(payer: Pubkey, authority: Pubkey, id: u64) -> Instruction {
    let mut payload = vec![v2::WIRE_VERSION];
    payload.extend_from_slice(&id.to_le_bytes());
    payload.push(0); // indexed policy
    payload.push(4); // command width
    payload.extend_from_slice(&64u32.to_le_bytes());
    payload.push(8); // up to eight transitions per ADVANCE
    payload.extend_from_slice(&DOOM_KERNEL_ID.0);
    payload.extend_from_slice(&2u16.to_le_bytes());
    payload.extend_from_slice(&1u16.to_le_bytes());
    payload.extend_from_slice(&v2::MODE_CONSENSUS_V2.id.to_le_bytes());
    payload.extend_from_slice(&v2::MODE_CONSENSUS_V2.version.to_le_bytes());
    payload.extend_from_slice(&[0xa5; 32]);
    payload.extend_from_slice(Pubkey::default().as_ref());
    payload.extend_from_slice(RESOURCE.as_ref());
    payload.extend_from_slice(&RESOURCE_SCHEMA.id.to_le_bytes());
    payload.extend_from_slice(&RESOURCE_SCHEMA.version.to_le_bytes());
    payload.extend_from_slice(&sha256(&[RESOURCE_BYTES]));
    instruction(
        sw::TAG_OPEN_SESSION,
        payload,
        vec![
            AccountMeta::new(payer, true),
            AccountMeta::new_readonly(authority, true),
            AccountMeta::new(session_pda(&authority, id), false),
            AccountMeta::new_readonly(RESOURCE, false),
            AccountMeta::new_readonly(system_program::ID, false),
        ],
    )
}

fn create_stream(payer: Pubkey, session: Pubkey) -> Instruction {
    instruction(
        sw::TAG_CREATE_STREAM,
        vec![v2::WIRE_VERSION, 0],
        vec![
            AccountMeta::new(payer, true),
            AccountMeta::new(session, false),
            AccountMeta::new(stream_pda(&session), false),
            AccountMeta::new_readonly(system_program::ID, false),
        ],
    )
}

fn write_input(writer: Pubkey, session: Pubkey, sequence: u32) -> Instruction {
    let mut payload = vec![v2::WIRE_VERSION];
    payload.extend_from_slice(&sequence.to_le_bytes());
    payload.push(4);
    payload.extend_from_slice(&[0, 0, 0, 0]);
    instruction(
        sw::TAG_WRITE_INPUT,
        payload,
        vec![
            AccountMeta::new_readonly(writer, true),
            AccountMeta::new(session, false),
            AccountMeta::new(stream_pda(&session), false),
        ],
    )
}

fn advance(actor: Pubkey, session: Pubkey, stream: Pubkey, expected_cursor: u32) -> Instruction {
    let mut payload = vec![v2::WIRE_VERSION];
    payload.extend_from_slice(&expected_cursor.to_le_bytes());
    payload.push(1);
    instruction(
        sw::TAG_ADVANCE,
        payload,
        vec![
            AccountMeta::new_readonly(actor, true),
            AccountMeta::new(session, false),
            AccountMeta::new(stream, false),
        ],
    )
}

fn close_live_stream(session: Pubkey, stream: Pubkey, refund: Pubkey) -> Instruction {
    instruction(
        sw::TAG_CLOSE_ACCOUNT,
        vec![v2::WIRE_VERSION, v2::KIND_STREAM],
        vec![
            AccountMeta::new(session, false),
            AccountMeta::new(stream, false),
            AccountMeta::new(refund, false),
        ],
    )
}

fn create_one_byte_state(payer: Pubkey, session: Pubkey, state: Pubkey) -> Instruction {
    let mut payload = vec![v2::WIRE_VERSION, 1];
    payload.extend_from_slice(&1u32.to_le_bytes());
    instruction(
        sw::TAG_CREATE_STATE,
        payload,
        vec![
            AccountMeta::new(payer, true),
            AccountMeta::new(session, false),
            AccountMeta::new_readonly(RESOURCE, false),
            AccountMeta::new(state, false),
            AccountMeta::new_readonly(system_program::ID, false),
        ],
    )
}

fn initialize_state(authority: Pubkey, session: Pubkey, state: Pubkey) -> Instruction {
    instruction(
        sw::TAG_CREATE_STATE,
        vec![v2::WIRE_VERSION, v2::STATE_OP_INITIALIZE],
        vec![
            AccountMeta::new_readonly(authority, true),
            AccountMeta::new(session, false),
            AccountMeta::new_readonly(RESOURCE, false),
            AccountMeta::new(state, false),
        ],
    )
}

async fn send(
    context: &mut ProgramTestContext,
    instruction: Instruction,
    extra_signers: &[&Keypair],
    expected: Result<(), u32>,
) -> u64 {
    let blockhash = context.get_new_latest_blockhash().await.unwrap();
    let mut signers = vec![&context.payer];
    signers.extend_from_slice(extra_signers);
    let tx = Transaction::new(
        &signers,
        solana_message::Message::new(&[instruction], Some(&context.payer.pubkey())),
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
        (
            Err(want),
            Err(TransactionError::InstructionError(
                _,
                solana_instruction::error::InstructionError::Custom(got),
            )),
        ) if want == got => {}
        (want, got) => panic!("expected {want:?}, got {got:?}"),
    }
    units
}

async fn start_sbf() -> ProgramTestContext {
    let output_dir =
        std::env::var("DOOM_DCG_SBF_DIR").expect("DOOM_DCG_SBF_DIR must contain doom_dcg.so");
    std::env::set_var("BPF_OUT_DIR", &output_dir);
    std::env::set_var("SBF_OUT_DIR", &output_dir);
    let mut test = ProgramTest::default();
    test.prefer_bpf(true);
    test.add_program("doom_dcg", PROGRAM, None);
    test.add_genesis_account(
        RESOURCE,
        Account {
            lamports: 1_000_000,
            data: RESOURCE_BYTES.to_vec(),
            owner: PROGRAM,
            executable: false,
            rent_epoch: 0,
        },
    );
    test.start_with_context().await
}

#[tokio::test(flavor = "multi_thread")]
async fn composed_sbf_dispatches_v2_and_preserves_wrong_authority_refusal() {
    let mut context = start_sbf().await;
    let authority = Keypair::new_from_array([0x17; 32]);
    let stranger = Keypair::new_from_array([0x55; 32]);
    let session = session_pda(&authority.pubkey(), 17);
    let payer = context.payer.pubkey();

    let open_cu = send(
        &mut context,
        open_session(payer, authority.pubkey(), 17),
        &[&authority],
        Ok(()),
    )
    .await;
    let stream_cu = send(&mut context, create_stream(payer, session), &[], Ok(())).await;
    let wrong_authority_cu = send(
        &mut context,
        write_input(stranger.pubkey(), session, 0),
        &[&stranger],
        Err(v2::REFUSAL_AUTHORITY),
    )
    .await;
    let authorized_write_cu = send(
        &mut context,
        write_input(authority.pubkey(), session, 0),
        &[&authority],
        Ok(()),
    )
    .await;
    let duplicate_input_cu = send(
        &mut context,
        write_input(authority.pubkey(), session, 0),
        &[&authority],
        Err(v2::REFUSAL_DUPLICATE_SLOT),
    )
    .await;
    let stale_cursor_cu = send(
        &mut context,
        advance(authority.pubkey(), session, stream_pda(&session), 1),
        &[&authority],
        Err(v2::REFUSAL_CURSOR),
    )
    .await;
    let live_close_cu = send(
        &mut context,
        close_live_stream(session, stream_pda(&session), authority.pubkey()),
        &[],
        Err(v2::REFUSAL_LIVE),
    )
    .await;
    let state = state_pda(&session, 0);
    let create_state_cu = send(
        &mut context,
        create_one_byte_state(payer, session, state),
        &[],
        Ok(()),
    )
    .await;
    let mut malformed_state = context
        .banks_client
        .get_account(state)
        .await
        .unwrap()
        .expect("state account created");
    malformed_state.data[76..78].copy_from_slice(&2u16.to_le_bytes());
    context.set_account(&state, &AccountSharedData::from(malformed_state));
    let wrong_state_version_cu = send(
        &mut context,
        initialize_state(authority.pubkey(), session, state),
        &[&authority],
        Err(v2::REFUSAL_STATE),
    )
    .await;

    eprintln!(
        "doom-dcg ProgramTest dispatch CU: open={open_cu} stream={stream_cu} wrong-authority={wrong_authority_cu} authorized-write={authorized_write_cu} duplicate-input={duplicate_input_cu} stale-cursor={stale_cursor_cu} close-live={live_close_cu} create-state={create_state_cu} wrong-state-version={wrong_state_version_cu}"
    );
    assert!(
        open_cu > 0
            && stream_cu > 0
            && wrong_authority_cu > 0
            && authorized_write_cu > 0
            && duplicate_input_cu > 0
            && stale_cursor_cu > 0
            && live_close_cu > 0
            && create_state_cu > 0
            && wrong_state_version_cu > 0
    );
}
