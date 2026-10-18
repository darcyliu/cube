use std::fs;
use std::process::{self, Command};
use std::time::{SystemTime, UNIX_EPOCH};
use cube::cow::{CLUSTER_SIZE, MAGIC, TABLE_START};

#[test]
fn inspect_and_export_commands_use_read_only_sources() {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let directory = std::env::temp_dir().join(format!("cube-overlay-cli-{}-{stamp}", process::id()));
    fs::create_dir(&directory).unwrap();
    let base = directory.join("base.img");
    let overlay = directory.join("base.img.cow");
    let output = directory.join("merged.img");
    fs::write(
        &base,
        [&vec![1u8; CLUSTER_SIZE as usize][..], &[2u8; 5]].concat(),
    )
    .unwrap();
    let mut bytes = Vec::new();
    bytes.extend_from_slice(MAGIC);
    bytes.extend_from_slice(&(CLUSTER_SIZE as u32).to_le_bytes());
    bytes.extend_from_slice(&0u32.to_le_bytes());
    bytes.extend_from_slice(&(CLUSTER_SIZE + 5).to_le_bytes());
    bytes.extend_from_slice(&2u64.to_le_bytes());
    bytes.extend_from_slice(&(TABLE_START + 16).to_le_bytes());
    bytes.extend_from_slice(&0u64.to_le_bytes());
    bytes.extend_from_slice(&vec![7u8; CLUSTER_SIZE as usize]);
    fs::write(&overlay, &bytes).unwrap();

    let binary = env!("CARGO_BIN_EXE_cube-overlay");
    let inspected = Command::new(binary)
        .args(["inspect"])
        .arg(&base)
        .arg(&overlay)
        .output()
        .unwrap();
    assert!(inspected.status.success());
    let report = String::from_utf8(inspected.stdout).unwrap();
    assert!(report.contains("Allocated clusters: 1"));
    assert!(report.contains("identity cannot be verified"));

    let exported = Command::new(binary)
        .args(["export"])
        .arg(&base)
        .arg(&overlay)
        .arg(&output)
        .output()
        .unwrap();
    assert!(exported.status.success());
    let result = fs::read(&output).unwrap();
    assert_eq!(result.len(), (CLUSTER_SIZE + 5) as usize);
    assert!(result[..CLUSTER_SIZE as usize]
        .iter()
        .all(|&byte| byte == 7));
    assert_eq!(&result[CLUSTER_SIZE as usize..], &[2u8; 5]);
    assert_eq!(fs::read(&base).unwrap()[0], 1);
    assert_eq!(fs::read(&overlay).unwrap(), bytes);

    let second = Command::new(binary)
        .args(["export"])
        .arg(&base)
        .arg(&overlay)
        .arg(&output)
        .output()
        .unwrap();
    assert!(!second.status.success());
    assert_eq!(fs::read(&output).unwrap(), result);

    let corrupt = directory.join("corrupt.cow");
    let rejected_output = directory.join("rejected.img");
    fs::write(&corrupt, &bytes[..bytes.len() - 1]).unwrap();
    let rejected = Command::new(binary)
        .args(["export"])
        .arg(&base)
        .arg(&corrupt)
        .arg(&rejected_output)
        .output()
        .unwrap();
    assert!(!rejected.status.success());
    assert!(!rejected_output.exists());
    assert_eq!(fs::read(&corrupt).unwrap(), bytes[..bytes.len() - 1]);
    assert_eq!(fs::read(&base).unwrap()[0], 1);
    fs::remove_dir_all(directory).unwrap();
}
