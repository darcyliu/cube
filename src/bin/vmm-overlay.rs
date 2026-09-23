//! Offline inspection and export of VMMCOW01 disk overlays.
use std::env;
use std::path::Path;
use std::process;
use vmm::cow::{CowImage, CLUSTER_SIZE};

fn run(args: &[String]) -> Result<(), String> {
    if args.len() < 4 || !matches!(args[1].as_str(), "inspect" | "export") {
        return Err("Usage: vmm-overlay inspect BASE OVERLAY\n       vmm-overlay export BASE OVERLAY OUTPUT".into());
    }
    let action = args[1].as_str();
    if (action == "inspect" && args.len() != 4) || (action == "export" && args.len() != 5) {
        return Err(
            "Wrong number of arguments; run vmm-overlay without arguments for usage".into(),
        );
    }
    let mut image = CowImage::open(Path::new(&args[2]), Path::new(&args[3]))?;
    if action == "inspect" {
        let layout = &image.layout;
        println!("Format: VMMCOW01");
        println!("Base: {}", args[2]);
        println!("Overlay: {}", args[3]);
        println!("Base bytes: {}", layout.base_size);
        println!("Cluster bytes: {CLUSTER_SIZE}");
        println!("Logical clusters: {}", layout.table.len());
        println!("Allocated clusters: {}", layout.allocated_clusters());
        println!("Physical clusters: {}", layout.physical_clusters);
        println!("Unreferenced clusters: {}", layout.unreferenced_clusters());
        println!("Base association: size matches; identity cannot be verified by VMMCOW01");
        println!("Consistency: valid");
    } else {
        let destination = Path::new(&args[4]);
        image.export(destination)?;
        println!(
            "Exported {} bytes to {}",
            image.layout.base_size,
            destination.display()
        );
    }
    Ok(())
}

fn main() {
    if let Err(error) = run(&env::args().collect::<Vec<_>>()) {
        eprintln!("vmm-overlay: {error}");
        process::exit(1);
    }
}
