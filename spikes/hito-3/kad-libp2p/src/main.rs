//! `kad-libp2p`: Iroh plus `libp2p-kad` on its own libp2p stack.
//!
//! Task 3 of the plan writes the node. Until then the binary only names one
//! item of each dependency, so that it links them all.

fn main() {
    println!("{:?}", spike_common::RECORD_SCHEMAS);
    println!("{}", std::any::type_name::<iroh::Endpoint>());
    println!(
        "{}",
        std::any::type_name::<libp2p::kad::Behaviour<libp2p::kad::store::MemoryStore>>()
    );
}
