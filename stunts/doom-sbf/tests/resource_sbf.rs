use doom_sbf::resource_sbf::{Request, Ri2Request, RI2_FRAME_BYTES, RI2_RESOURCE_IMAGE_BYTES};

const STATE_LEN: usize = 59_604;

#[test]
fn ri2_request_round_trips_without_reinterpreting_ri1() {
    let ri2 = Ri2Request {
        resource_bytes: RI2_RESOURCE_IMAGE_BYTES,
        state_len: STATE_LEN,
        wad_bytes: doom_sbf::resource_sbf::WAD_IMAGE_BYTES,
        frame_bytes: RI2_FRAME_BYTES,
    };
    assert_eq!(Ri2Request::decode(&ri2.encode()), Ok(ri2));

    let ri1 = Request {
        resource_bytes: doom_sbf::resource_sbf::RESOURCE_IMAGE_BYTES,
        state_len: STATE_LEN,
        wad_bytes: doom_sbf::resource_sbf::WAD_IMAGE_BYTES,
    };
    assert_eq!(Request::decode(&ri1.encode()), Ok(ri1));
    assert!(Ri2Request::decode(&ri1.encode()).is_err());
    assert!(Request::decode(&ri2.encode()).is_err());
}

#[test]
fn ri2_request_rejects_bad_lengths_and_reserved_bytes() {
    let request = Ri2Request {
        resource_bytes: RI2_RESOURCE_IMAGE_BYTES,
        state_len: STATE_LEN,
        wad_bytes: doom_sbf::resource_sbf::WAD_IMAGE_BYTES,
        frame_bytes: RI2_FRAME_BYTES,
    };
    let mut bytes = request.encode();
    bytes[13] = 1;
    assert!(Ri2Request::decode(&bytes).is_err());
    let mut bytes = request.encode();
    bytes[16..20].copy_from_slice(&(RI2_RESOURCE_IMAGE_BYTES as u32 - 1).to_le_bytes());
    assert!(Ri2Request::decode(&bytes).is_err());
}
