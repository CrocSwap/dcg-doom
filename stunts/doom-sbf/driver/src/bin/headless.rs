//! Plan-only headless Doom step-6 driver.
//!
//! This binary reads the retained step-3 account image and host capture, then
//! emits the fixed-account `init -> load_level -> tic -> render` instruction
//! sequence.  It does not start ProgramTest, submit a transaction, or call
//! the SBF image.  The resulting plan is deliberately useful for review while
//! the action-pointer and persistent-context gates remain open.

use std::{
    env,
    error::Error,
    fs,
    path::{Path, PathBuf},
};

use doom_sbf::{
    bring_up::{CHUNK_BYTES, PAYLOAD_BYTES, WAD_BYTES},
    engine::{self, Request},
};
use solana_instruction::{account_meta::AccountMeta, Instruction};
use solana_pubkey::Pubkey;

const PROGRAM: Pubkey = Pubkey::new_from_array([0xd0; 32]);
const DEMO1_NAME: &[u8; 5] = b"DEMO1";
const DEMO1_LENGTH: usize = 20_118;
const GOLDEN_STATE: &str = "checkpoint-0.st1";

#[derive(Clone, Copy)]
struct Role {
    name: &'static str,
    key: Pubkey,
}

fn role(index: usize) -> Role {
    let name = match index {
        engine::CONTEXT_ACCOUNT => "context",
        engine::CONFIG_ACCOUNT => "config",
        2 => "wad-000",
        3 => "wad-001",
        4 => "wad-002",
        5 => "wad-003",
        6 => "wad-004",
        engine::LEVEL_ACCOUNT => "level_state",
        engine::MOBJ_ACCOUNT => "mobj_pool",
        engine::CONTROLLER_ACCOUNT => "controller",
        engine::SCRATCH_ACCOUNT => "render_scratch",
        engine::FRAME_ACCOUNT => "frame_output",
        _ => "unknown",
    };
    let mut bytes = [0u8; 32];
    bytes[0] = 0xd0;
    bytes[1] = index as u8;
    Role {
        name,
        key: Pubkey::new_from_array(bytes),
    }
}

fn read(path: &Path) -> Result<Vec<u8>, Box<dyn Error>> {
    Ok(fs::read(path).map_err(|error| format!("{}: {error}", path.display()))?)
}

fn u32_at(data: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(data[offset..offset + 4].try_into().unwrap())
}

fn load_wad(loader: &Path) -> Result<Vec<u8>, Box<dyn Error>> {
    let mut wad = Vec::with_capacity(WAD_BYTES);
    for index in 0..5 {
        let path = loader.join("account").join(format!("wad-{index:03}.bin"));
        let account = read(&path)?;
        if account.len() != CHUNK_BYTES
            || &account[..8] != b"DOOM-WC1"
            || u32_at(&account, 8) != 1
            || u32_at(&account, 12) != 32
            || u32_at(&account, 16) != CHUNK_BYTES as u32
            || u32_at(&account, 20) != index as u32
        {
            return Err(format!(
                "refusing non-captured or malformed WAD account {}",
                path.display()
            )
            .into());
        }
        let length = u32_at(&account, 24) as usize;
        if length > PAYLOAD_BYTES || 32 + length > account.len() {
            return Err(format!("WAD account {} payload is out of bounds", path.display()).into());
        }
        if account[32 + length..].iter().any(|&byte| byte != 0) {
            return Err(format!("WAD account {} has nonzero padding", path.display()).into());
        }
        wad.extend_from_slice(&account[32..32 + length]);
    }
    if wad.len() != WAD_BYTES || &wad[..4] != b"IWAD" {
        return Err("captured WAD image is not the pinned IWAD length/header".into());
    }
    Ok(wad)
}

fn verify_captured_state_accounts(loader: &Path) -> Result<(), Box<dyn Error>> {
    let level_path = loader.join("level_state.bin");
    let level = read(&level_path)?;
    if level.len() != engine::LEVEL_BYTES
        || &level[..8] != b"DOOM-LS1"
        || u32_at(&level, 8) != 1
        || u32_at(&level, 12) != 128
        || u32_at(&level, 16) != engine::LEVEL_BYTES as u32
        || u32_at(&level, 20) != 4_096
        || u32_at(&level, 24) != (engine::LEVEL_BYTES - 4_096) as u32
    {
        return Err(format!(
            "refusing non-captured level account {}",
            level_path.display()
        )
        .into());
    }
    let image_bytes = u32_at(&level, 28) as usize;
    if image_bytes > engine::LEVEL_BYTES - 4_096
        || level[4_096 + image_bytes..].iter().any(|&byte| byte != 0)
    {
        return Err(format!(
            "captured level account {} has an invalid image interval",
            level_path.display()
        )
        .into());
    }

    let pool_path = loader.join("mobj_pool.bin");
    let pool = read(&pool_path)?;
    if pool.len() != engine::MOBJ_BYTES
        || &pool[..8] != b"DOOM-MP1"
        || u32_at(&pool, 8) != 1
        || u32_at(&pool, 12) != 4_096
        || u32_at(&pool, 16) != engine::MOBJ_BYTES as u32
        || u32_at(&pool, 20) != 160
        || u32_at(&pool, 24) != 384
    {
        return Err(format!("refusing non-captured mobj account {}", pool_path.display()).into());
    }
    Ok(())
}

fn demo1(wad: &[u8]) -> Result<&[u8], Box<dyn Error>> {
    let count = u32_at(wad, 4) as usize;
    let directory = u32_at(wad, 8) as usize;
    let end = directory
        .checked_add(count.checked_mul(16).ok_or("WAD directory overflow")?)
        .ok_or("WAD directory overflow")?;
    if directory < 12 || end > wad.len() {
        return Err("captured WAD directory is out of bounds".into());
    }
    for index in 0..count {
        let entry = directory + index * 16;
        let offset = u32_at(wad, entry) as usize;
        let length = u32_at(wad, entry + 4) as usize;
        if offset > wad.len() || length > wad.len() - offset {
            return Err("captured WAD lump is out of bounds".into());
        }
        let name = &wad[entry + 8..entry + 16];
        let name_len = name
            .iter()
            .position(|&byte| byte == 0)
            .unwrap_or(name.len());
        if &name[..name_len] == DEMO1_NAME {
            let demo = &wad[offset..offset + length];
            if demo.len() != DEMO1_LENGTH || demo.len() < 17 {
                return Err("captured DEMO1 length does not match the retained host input".into());
            }
            return Ok(demo);
        }
    }
    Err("captured WAD has no DEMO1 lump".into())
}

fn controller_account(gametic: u32, command: &[u8]) -> [u8; engine::CONTROLLER_BYTES] {
    let mut account = [0u8; engine::CONTROLLER_BYTES];
    account[..8].copy_from_slice(engine::CONTROLLER_MAGIC);
    account[8..12].copy_from_slice(&engine::VERSION.to_le_bytes());
    account[12..16].copy_from_slice(&(engine::CONTROLLER_HEADER_BYTES as u32).to_le_bytes());
    account[16..20].copy_from_slice(&(engine::CONTROLLER_BYTES as u32).to_le_bytes());
    account[20..24].copy_from_slice(&gametic.to_le_bytes());
    account[24..28].copy_from_slice(command);
    account
}

fn account_metas(request: Request) -> Vec<AccountMeta> {
    (0..engine::ACCOUNT_COUNT)
        .map(|index| {
            let role = role(index);
            let writable = index == engine::CONTEXT_ACCOUNT
                || match request.operation {
                    engine::INIT => matches!(
                        index,
                        engine::CONFIG_ACCOUNT | engine::LEVEL_ACCOUNT | engine::MOBJ_ACCOUNT
                    ),
                    engine::LOAD_LEVEL | engine::TIC => {
                        matches!(index, engine::LEVEL_ACCOUNT | engine::MOBJ_ACCOUNT)
                    }
                    engine::RENDER => {
                        matches!(index, engine::SCRATCH_ACCOUNT | engine::FRAME_ACCOUNT)
                    }
                    _ => false,
                };
            let signer = request.operation == engine::INIT && index == engine::CONFIG_ACCOUNT;
            AccountMeta {
                pubkey: role.key,
                is_signer: signer,
                is_writable: writable,
            }
        })
        .collect()
}

fn instruction_line(label: &str, request: Request) {
    let instruction = Instruction {
        program_id: PROGRAM,
        accounts: account_metas(request),
        data: request.encode().to_vec(),
    };
    let data_hex = instruction
        .data
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    println!(
        "STEP {label} op={} arg0={} arg1={} arg2={} data_hex={data_hex}",
        request.operation, request.arg0, request.arg1, request.arg2
    );
    println!("  accounts={}", instruction.accounts.len());
    for (index, meta) in instruction.accounts.iter().enumerate() {
        println!(
            "  account[{index}] role={} key={} writable={} signer={}",
            role(index).name,
            meta.pubkey,
            meta.is_writable,
            meta.is_signer
        );
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    let mut args = env::args().skip(1);
    let loader =
        PathBuf::from(args.next().ok_or(
            "usage: headless STEP3_ROOT HOST_GOLDENS [STATE_GOLDEN [strip_id] | strip_id]",
        )?);
    let goldens =
        PathBuf::from(args.next().ok_or(
            "usage: headless STEP3_ROOT HOST_GOLDENS [STATE_GOLDEN [strip_id] | strip_id]",
        )?);
    let optional = args.next();
    let (state_path, strip_id) = match optional {
        None => (None, 0),
        Some(value) => match value.parse::<u32>() {
            Ok(strip_id) => (None, strip_id),
            Err(_) => {
                let strip_id = args
                    .next()
                    .map(|value| value.parse::<u32>())
                    .transpose()?
                    .unwrap_or(0);
                (Some(PathBuf::from(value)), strip_id)
            }
        },
    };
    if args.next().is_some() || strip_id >= 8 {
        return Err(
            "optional state-golden and strip_id (0..7) only; no frame-bucket option is accepted"
                .into(),
        );
    }

    let wad = load_wad(&loader)?;
    verify_captured_state_accounts(&loader)?;
    let demo = demo1(&wad)?;
    let state_path = state_path.unwrap_or_else(|| {
        [
            goldens.join(GOLDEN_STATE),
            loader.join("dumped-doom-st1.bin"),
            loader.join("ordinary-doom-st1.bin"),
        ]
        .into_iter()
        .find(|path| path.is_file())
        .unwrap_or_else(|| goldens.join(GOLDEN_STATE))
    });
    let state = read(&state_path)?;
    if state.len() < 24
        || &state[..8] != b"DOOM-ST1"
        || u32_at(&state, 8) != 0
        || u32_at(&state, 20) != 0
    {
        return Err("host golden is not the retained DOOM-ST1 S0 capture".into());
    }
    let frame_golden = ["frames-run1.jsonl", "frames.jsonl"]
        .iter()
        .map(|name| goldens.join(name))
        .find(|path| path.is_file())
        .ok_or("host golden directory has no frames-run1.jsonl or frames.jsonl")?;
    let command = &demo[13..17];
    let controller = controller_account(0, command);

    println!(
        "DOOM-EX1 plan-only version={} program={PROGRAM}",
        engine::VERSION
    );
    println!(
        "SOURCE wad_bytes={} demo_bytes={} first_command={:02x?}",
        wad.len(),
        demo.len(),
        command
    );
    println!(
        "SOURCE level_golden={} frame_golden={}",
        state_path.display(),
        frame_golden.display()
    );
    println!(
        "CONTROLLER account_bytes={} gametic=0 bytes={:02x?}",
        controller.len(),
        controller
    );
    println!("BOUNDARY image loads and CX1 context read/write are retained evidence; lifecycle execution is deliberately not started");
    instruction_line("init", Request::init());
    instruction_line(
        "load_level",
        Request::load_level(demo[2] as u32, demo[3] as u32, demo[1] as u32),
    );
    instruction_line("tic", Request::tic(0));
    instruction_line("render", Request::render(1, strip_id));
    println!("EXECUTION withheld: ctx-engine DOOM-EX1 would return Custom({}) until action-pointer relocation and persistent-state ABI gates close", engine::ENGINE_ACTION_POINTERS_UNRESOLVED);
    Ok(())
}
