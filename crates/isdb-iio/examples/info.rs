//! Print the libiio version and the context attributes of an IIO device
//! (no RF activity): `cargo run -p isdb-iio --example info -- ip:192.168.2.1`.
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let uri = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "ip:pluto.local".into());
    println!("libiio {}", isdb_iio::library_version());
    let ctx = isdb_iio::Context::open(&uri)?;
    println!("{}", ctx.description());
    for a in ["hw_model", "hw_serial", "fw_version", "ad9361-phy,model"] {
        println!("{a}: {}", ctx.attr(a).unwrap_or_else(|| "-".into()));
    }
    let phy = ctx.device("ad9361-phy")?;
    println!("ensm_mode: {}", phy.read("ensm_mode")?);
    println!("tx_path_rates: {}", phy.read("tx_path_rates")?);
    Ok(())
}
