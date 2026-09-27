//! Host-only census for each view in a recorded SIM/2 command stream.

#[path = "continuation_split_sbf.rs"]
#[allow(dead_code)]
mod base;
#[path = "../sweep_common.rs"]
#[allow(dead_code)]
mod common;

use std::{env, error::Error, fs, io::Write, path::PathBuf};

use common::Capture;
use doom_sbf::resource::{self, FRAME_BYTES, MIRROR_BYTES, STATE_MAX_BYTES};

fn main() -> Result<(), Box<dyn Error>> {
    let mut args = env::args().skip(1);
    let capture_path =
        PathBuf::from(args.next().ok_or("missing capture directory")?).canonicalize()?;
    let resource_path = PathBuf::from(args.next().ok_or("missing RI2 resource")?).canonicalize()?;
    let command_path = PathBuf::from(args.next().ok_or("missing command file")?).canonicalize()?;
    let output = PathBuf::from(args.next().ok_or("missing output directory")?);
    let expected_state_path = args.next().map(PathBuf::from);
    if args.next().is_some() {
        return Err("unexpected argument after expected state".into());
    }
    if output.exists() && fs::read_dir(&output)?.next().is_some() {
        return Err("output directory must be empty".into());
    }
    fs::create_dir_all(&output)?;

    let command_bytes = fs::read(&command_path)?;
    if command_bytes.is_empty() || command_bytes.len() % 4 != 0 {
        return Err("command file must contain a nonempty sequence of 4-byte tics".into());
    }
    let tics = command_bytes.len() / 4;
    let (commands, cycle_len) =
        common::input_commands(&format!("command-file:{}", command_path.display()), tics)?;
    let capture = Capture::load(&capture_path, &resource_path)?;
    let mut session =
        resource::NativeSimSession::open(capture.wad_refs(), &capture.resource, &capture.state)
            .map_err(|rc| format!("host census session open rc={rc}"))?;
    let first_tic = base::u32_at(&capture.state, 20);
    let mut rows = fs::File::create(output.join("census.jsonl"))?;

    for (seq, command) in commands.chunks_exact(4).enumerate() {
        let command: [u8; 4] = command.try_into().unwrap();
        let rc = session.step(&command);
        if rc != 0 {
            return Err(format!("host replay step {seq} refused with rc={rc}").into());
        }
        let cursor = seq as u32 + 1;
        let (frame, m) = session
            .frame_with_census()
            .map_err(|rc| format!("host frame/census at cursor {cursor} rc={rc}"))?;
        if frame.len() != FRAME_BYTES {
            return Err(format!("host frame at cursor {cursor} has {} bytes", frame.len()).into());
        }
        writeln!(
            rows,
            "{}",
            serde_json::json!({
                "cursor": cursor,
                "gametic": first_tic + cursor,
                "x": m[12],
                "y": m[13],
                "angle": m[14],
                "floorpic": m[15],
                "wall_pixels": m[0],
                "wall_columns": m[1],
                "span_pixels": m[2],
                "spans": m[3],
                "span_rows": m[4],
                "sky_pixels": m[5],
                "masked_sprite_pixels": m[6],
                "sprites": m[7],
                "drawsegs": m[8],
                "sprite_clip_steps": m[9],
                "visplanes": m[10],
                "masked_midtexture_pixels": m[11],
                "wall_strip_pixels": m[16..24].to_vec(),
                "span_strip_pixels": m[24..32].to_vec(),
                "sky_strip_pixels": m[32..40].to_vec(),
                "masked_strip_pixels": m[40..48].to_vec(),
                "midtexture_strip_pixels": m[48..56].to_vec(),
            })
        )?;
    }
    rows.flush()?;

    let mut state = vec![0u8; STATE_MAX_BYTES];
    let mut mirror = vec![0u8; MIRROR_BYTES];
    let state_len = session
        .publish(&mut state, &mut mirror)
        .map_err(|rc| format!("host final publish rc={rc}"))?;
    let expected_state_equal = if let Some(path) = expected_state_path {
        let expected = fs::read(path)?;
        Some(expected.len() == state.len() && expected.get(..state_len) == state.get(..state_len))
    } else {
        None
    };
    if expected_state_equal == Some(false) {
        return Err("replayed host state differs from the committed account state".into());
    }
    fs::write(output.join("final-state.bin"), &state)?;
    let summary = serde_json::json!({
        "cursors": tics,
        "first_gametic": first_tic,
        "final_gametic": first_tic + tics as u32,
        "command_bytes": commands.len(),
        "command_sha256": base::sha(&commands),
        "command_cycle_len": cycle_len,
        "final_state_bytes": state.len(),
        "final_state_published_length": state_len,
        "final_state_sha256": base::sha(&state),
        "expected_state_equal": expected_state_equal,
        "frame_bytes_per_cursor": FRAME_BYTES,
    });
    fs::write(
        output.join("summary.json"),
        serde_json::to_vec_pretty(&summary)?,
    )?;
    println!("{}", serde_json::to_string(&summary)?);
    Ok(())
}
