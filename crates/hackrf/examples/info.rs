//! Print library/board/firmware info for the attached HackRF (no RF activity).
fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("libhackrf {}", hackrf::library_version());
    let mut d = hackrf::Device::open(None)?;
    println!(
        "board id {} firmware {}",
        d.board_id()?,
        d.firmware_version()?
    );
    println!("streaming: {}", d.is_streaming());
    Ok(())
}
