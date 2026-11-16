//! `mainline`: Iroh plus a private Mainline DHT, with the contact resolved
//! through pkarr.
//!
//! Task 5 of the plan writes the node. Until then the binary only names one
//! item of each dependency, so that it links them all.

fn main() {
    println!("{:?}", spike_common::RECORD_SCHEMAS);
    println!("{}", std::any::type_name::<iroh::Endpoint>());
    println!("{}", std::any::type_name::<mainline::Dht>());
    println!("{}", std::any::type_name::<pkarr::SignedPacket>());
    println!(
        "{}",
        std::any::type_name::<iroh_mainline_address_lookup::DhtAddressLookup>()
    );
}
