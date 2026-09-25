//! Streaming per-tic oracle for a long deterministic SIM/2 session.
//!
//! Unlike the retained gate oracle, this writes only hashes per tic and keeps
//! one ST1/MP1 pair resident, so a 63,000-tic run does not retain gigabytes.
//!
//! R8l: this IS the chain's controller ingress.  `resource::NativeSimSession`
//! runs the same per-tic contract the SBF step runs, verbatim and source
//! matched (shared pre-tic guard, `DoomControllerArmV2`, `G_Ticker`,
//! `gametic++`, then the DOOM-CI3 receipt), against the real captured DEMO1
//! lump.  The retained `doom-headless --command-stream` path is NOT that
//! ingress: it replaces the lump with a synthetic one built from the command
//! bytes, so its demo_p never reaches the end marker and it cannot exercise
//! what happens past tic 5026.
//!
//! `--frames-at` writes the 64,000-byte player view at the chosen cursors, so
//! the SBF strips can be compared to the oracle pixel for pixel.

#[path = "continuation_split_sbf.rs"]
#[allow(dead_code)]
mod base;
#[path = "../sweep_common.rs"]
#[allow(dead_code)]
mod common;

use std::{env, error::Error, fs, io::Write, path::PathBuf, str::FromStr, time::Instant};

use common::Capture;
use doom_sbf::{resource, resource_sbf, sim2};
use solana_pubkey::Pubkey;

fn main() -> Result<(), Box<dyn Error>> {
    let mut args = env::args().skip(1);
    let capture = PathBuf::from(args.next().ok_or("missing capture directory")?).canonicalize()?;
    let resource_path = PathBuf::from(args.next().ok_or("missing RI2 resource")?).canonicalize()?;
    let output = PathBuf::from(args.next().ok_or("missing output directory")?);
    let context = Pubkey::from_str(&args.next().ok_or("missing context pubkey")?)?;
    let requested: usize = args.next().ok_or("missing tic count")?.parse()?;
    let input_profile = args.next().unwrap_or_else(|| "demo-loop".to_owned());
    let frames_at: Vec<u32> = args
        .next()
        .unwrap_or_default()
        .split(',')
        .filter(|part| !part.is_empty())
        .map(|part| part.parse::<u32>())
        .collect::<Result<Vec<u32>, _>>()?;
    if requested == 0 || requested > sim2::MAX_BUFFER_CAPACITY {
        return Err(format!("tic count must be in 1..={}", sim2::MAX_BUFFER_CAPACITY).into());
    }
    if args.next().is_some() {
        return Err("unexpected argument after --frames-at".into());
    }
    if output.exists() && fs::read_dir(&output)?.next().is_some() {
        return Err("output directory must be empty".into());
    }
    fs::create_dir_all(&output)?;
    let capture_data = Capture::load(&capture, &resource_path)?;
    let (commands, cycle_len) = match input_profile.as_str() {
        "demo-loop" => capture_data.looped_demo_commands(requested)?,
        "neutral" => (vec![0; requested * 4], 0),
        "orbit-fire" => {
            let mut commands = Vec::with_capacity(requested * 4);
            for seq in 0..requested {
                let turn = if (seq / 256) % 2 == 0 { 1u8 } else { 255u8 };
                let side = if (seq / 128) % 2 == 0 { 8u8 } else { 248u8 };
                commands.extend_from_slice(&[16, side, turn, 1]);
            }
            (commands, 0)
        }
        // R8l: every segment moves or turns, the way the R8b sender's
        // KEY_SCRIPTS does.  16 is a signed-char forward ticcmd move; 8/248
        // strafe; 1/255 is a +-1 turn byte (the engine reads it as the high
        // byte of a signed short).
        "r8l-move" => {
            let mut commands = Vec::with_capacity(requested * 4);
            for seq in 0..requested {
                let (forward, side, turn, buttons) = match (seq / 200) % 4 {
                    0 => (16u8, 0u8, 0u8, 0u8),
                    1 => (0, 8, 0, 0),
                    2 => (0, 0, 1, 1),
                    _ => (16, 0, 255, 0),
                };
                commands.extend_from_slice(&[forward, side, turn, buttons]);
            }
            (commands, 0)
        }
        profile => return Err(format!("unsupported input profile {profile:?}").into()),
    };
    let mut session = resource::NativeSimSession::open(
        capture_data.wad_refs(),
        &capture_data.resource,
        &capture_data.state,
    )
    .map_err(|rc| format!("oracle session open rc={rc}"))?;
    let first_tic = base::u32_at(&capture_data.state, 20);
    let initial_state = capture_data.state_account();
    let (_, initial_root) = sim2::verified_state_root(&initial_state, &capture_data.mirror)
        .map_err(|error| format!("initial root: {error:?}"))?;
    let mut chain = sim2::chain_init(&context, first_tic);
    let mut state = vec![0u8; resource_sbf::STATE_MAX_BYTES];
    let mut mirror = vec![0u8; resource_sbf::MIRROR_BYTES];
    let mut tic_rows = fs::File::create(output.join("oracle-per-tic.jsonl"))?;
    let mut frame_rows = fs::File::create(output.join("oracle-frames.jsonl"))?;
    let mut want_frame = frames_at.contains(&0);
    let start = Instant::now();
    let mut completed = 0usize;
    let mut halt = None;
    for (seq, command_bytes) in commands.chunks_exact(4).enumerate() {
        let command: [u8; 4] = command_bytes.try_into().unwrap();
        let rc = session.step(&command);
        if rc != 0 {
            halt = Some(rc);
            break;
        }
        chain = sim2::chain_step(&chain, seq as u32, &command);
        if want_frame {
            // Cursor seq+1 is the state after this tic, which is the state the
            // chain renders at that cursor.
            let frame = session
                .frame()
                .map_err(|rc| format!("frame at cursor {} rc={rc}", seq + 1))?;
            let path = output.join(format!("oracle-frame-{:05}.fb1", seq + 1));
            fs::write(&path, &frame)?;
            writeln!(
                frame_rows,
                "{}",
                serde_json::json!({
                    "cursor": seq + 1,
                    "gametic": first_tic + seq as u32 + 1,
                    "frame_path": path.file_name().unwrap().to_string_lossy(),
                    "frame_sha256": base::sha(&frame),
                    "frame_bytes": frame.len(),
                })
            )?;
            want_frame = false;
        }
        let state_len = session
            .publish(&mut state, &mut mirror)
            .map_err(|rc| format!("publish at tic {seq} rc={rc}"))?;
        let (checked, root) = sim2::verified_state_root(&state, &mirror)
            .map_err(|error| format!("state root at tic {seq}: {error:?}"))?;
        if checked != state_len || base::u32_at(&state, 20) != first_tic + seq as u32 + 1 {
            return Err(format!("state shape mismatch at tic {seq}").into());
        }
        writeln!(
            tic_rows,
            "{}",
            serde_json::json!({
                "seq": seq,
                "cursor": seq + 1,
                "st1_len": state_len,
                "st1_account_sha256": base::sha(&state),
                "mp1_sha256": base::sha(&mirror),
                "state_root": common::hex(&root),
                "input_chain": common::hex(&chain),
                "gametic": base::u32_at(&state, 20),
                "demo_offset": base::u32_at(&state, 24),
            })
        )?;
        completed += 1;
    }
    tic_rows.flush()?;
    fs::write(output.join("commands.bin"), &commands)?;
    if completed > 0 {
        fs::write(output.join("final-state.st1"), &state)?;
        fs::write(output.join("final-mirror.mp1"), &mirror)?;
    }
    frame_rows.flush()?;
    for cursor in &frames_at {
        if *cursor > requested as u32 {
            return Err(format!("--frames-at {cursor} exceeds the {requested}-tic run").into());
        }
    }
    let elapsed = start.elapsed().as_secs_f64();
    let summary = serde_json::json!({
        "requested_tics": requested,
        "completed_tics": completed,
        "first_tic": first_tic,
        "input_profile": input_profile,
        "demo_cycle_commands": cycle_len,
        "elapsed_seconds": elapsed,
        "tics_per_second": completed as f64 / elapsed,
        "initial_state_root": common::hex(&initial_root),
        "final_state_root": if completed > 0 {
            let (.., root) = sim2::verified_state_root(&state, &mirror)
                .map_err(|error| format!("final state root: {error:?}"))?;
            common::hex(&root)
        } else { String::new() },
        "halt_code": halt,
        "streaming": true,
        "frames_requested": frames_at.clone(),
        "frames_written": frames_at.iter().filter(|c| **c <= completed as u32).count(),
        "ingress": "host-session: doom_resource_sim_session_step (guard + DoomControllerArmV2 + G_Ticker + gametic++ + DOOM-CI3 receipt) on the captured DEMO1 lump",
    });
    common::write_json(&output.join("summary.json"), &summary)?;
    println!("{}", serde_json::to_string_pretty(&summary)?);
    Ok(())
}
