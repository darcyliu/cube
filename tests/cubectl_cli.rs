use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::process::{self, Command};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

fn review_test_dir(label: &str) -> std::path::PathBuf {
    let stamp = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    let dir = std::env::temp_dir().join(format!("cubectl-{label}-{}-{stamp}", process::id()));
    fs::create_dir_all(&dir).unwrap();
    dir.canonicalize().unwrap()
}

#[test]
fn test_delete_removes_vm_directory_but_preserves_external_boot_files() {
    let root = review_test_dir("safe-delete");
    let vm = root.join("vms/example");
    fs::create_dir_all(&vm).unwrap();
    let kernel = root.join("shared-kernel");
    let module = root.join("shared-module");
    for path in [&kernel, &module] { fs::write(path, b"shared").unwrap(); }
    // Exercise canonical containment through both parent traversal and symlinks.
    std::os::unix::fs::symlink(&module, vm.join("module-link")).unwrap();
    for name in ["kernel", "installer.iso", "readonly.img", "notes.txt", "disk.img", "local-module"] {
        fs::write(vm.join(name), b"data").unwrap();
    }
    let config = vm.join("example.json");
    fs::write(&config, serde_json::json!({
        "name": "example",
        "boot": {"mode": "freebsd", "kernel": "../../shared-kernel",
                 "modules": ["module-link", "local-module"]},
        "disks": ["disk.img", {"path": "installer.iso", "read_only": true},
                  {"path": "readonly.img", "read_only": true}]
    }).to_string()).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_cubectl"))
        .env("CUBE_VMS_DIR", root.join("vms"))
        .arg("--state-root").arg(root.join("state"))
        .arg("delete").arg(&config).output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    for path in [&kernel, &module] { assert!(path.exists(), "lost {}", path.display()); }
    assert!(!vm.exists(), "VM directory and all local artifacts must be deleted");
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn test_delete_keep_disks_preserves_vm_directory_contents() {
    let root = review_test_dir("keep-disks");
    let vm = root.join("vms/example");
    fs::create_dir_all(&vm).unwrap();
    for name in ["kernel", "disk.img", "installer.iso", "notes.txt"] {
        fs::write(vm.join(name), b"data").unwrap();
    }
    let config = vm.join("example.json");
    fs::write(&config, serde_json::json!({
        "name": "example",
        "boot": {"mode": "freebsd", "kernel": "kernel"},
        "disks": ["disk.img", {"path": "installer.iso", "read_only": true}]
    }).to_string()).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_cubectl"))
        .env("CUBE_VMS_DIR", root.join("vms"))
        .arg("--state-root").arg(root.join("state"))
        .arg("delete").arg(&config).arg("--keep-disks").output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    assert!(!config.exists());
    for name in ["kernel", "disk.img", "installer.iso", "notes.txt"] {
        assert!(vm.join(name).exists(), "lost {name}");
    }
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn test_list_discovers_vms_without_state_directory() {
    let root = review_test_dir("fresh-list");
    let vm = root.join("Cube/fresh");
    fs::create_dir_all(&vm).unwrap();
    fs::write(vm.join("fresh.json"), serde_json::json!({
        "name": "fresh", "boot": {"mode": "uefi", "firmware": "fw.fd"}, "disks": []
    }).to_string()).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_cubectl"))
        .env("HOME", &root).env_remove("CUBE_VMS_DIR")
        .args(["list", "--json"]).output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let items: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(items[0]["name"], "fresh");
    assert_eq!(items[0]["state"], "stopped");
    assert!(!root.join(".local/state/cube").exists());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn test_custom_destination_is_listed_and_can_start_by_name() {
    let root = review_test_dir("custom-destination");
    let state = root.join("state");
    let destination = root.join("custom/example");
    let firmware = root.join("fw.fd");
    fs::write(&firmware, b"firmware").unwrap();
    let fake_cube = root.join("fake-cube");
    fs::write(&fake_cube, "#!/bin/sh\nexec sleep 60\n").unwrap();
    fs::set_permissions(&fake_cube, fs::Permissions::from_mode(0o755)).unwrap();
    let cli = || {
        let mut command = Command::new(env!("CARGO_BIN_EXE_cubectl"));
        command.env("HOME", &root).env("CUBE_VMS_DIR", root.join("default-vms"))
            .arg("--state-root").arg(&state);
        command
    };
    let created = cli().args(["create", "example", "--net", "none", "--disk", "disk.img:1M"])
        .arg("--dest").arg(&destination).arg("--firmware").arg(&firmware)
        .arg("--binary").arg(&fake_cube).output().unwrap();
    assert!(created.status.success(), "{}", String::from_utf8_lossy(&created.stderr));
    let listed = cli().args(["list", "--json"]).output().unwrap();
    assert!(listed.status.success());
    let items: serde_json::Value = serde_json::from_slice(&listed.stdout).unwrap();
    assert_eq!(items[0]["name"], "example");
    assert_eq!(items[0]["config"], destination.join("example.json").to_str().unwrap());
    let started = cli().args(["start", "example"]).output().unwrap();
    // Always stop before asserting, so failed assertions cannot leak a guest.
    let stopped = cli().args(["stop", "example", "--timeout", "1"]).output().unwrap();
    assert!(started.status.success(), "{}", String::from_utf8_lossy(&started.stderr));
    assert!(stopped.status.success(), "{}", String::from_utf8_lossy(&stopped.stderr));
    let deleted = cli().args(["delete", "example"]).output().unwrap();
    assert!(deleted.status.success(), "{}", String::from_utf8_lossy(&deleted.stderr));
    assert!(!destination.join("example.json").exists());
    assert!(!state.join("example").exists());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn test_cubectl_cli_lifecycle_and_conflicts() {
    let stamp = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    let temp_dir = std::env::temp_dir().join(format!("cubectl-test-{}-{stamp}", process::id()));
    fs::create_dir_all(&temp_dir).unwrap();
    let state_root = temp_dir.join("state");
    let vms_dir = temp_dir.join("vms");
    fs::create_dir_all(&vms_dir).unwrap();

    // 1. Create a dummy fake binary that simulates a guest running
    let fake_cube = temp_dir.join("fake-cube");
    fs::write(
        &fake_cube,
        "#!/bin/sh\nprintf '{\"version\":1,\"instance_id\":\"%s\",\"ip\":null}' \"$CUBE_INSTANCE_ID\" > \"$CUBE_RUNTIME_STATUS_PATH\"\nwhile true; do sleep 0.1; done\n",
    )
    .unwrap();
    fs::set_permissions(&fake_cube, fs::Permissions::from_mode(0o755)).unwrap();

    // Create a dummy disk and firmware
    let dummy_disk = temp_dir.join("disk1.img");
    fs::write(&dummy_disk, b"dummy disk").unwrap();
    let dummy_fw = temp_dir.join("firmware.fd");
    fs::write(&dummy_fw, b"dummy firmware").unwrap();

    let cubectl_bin = env!("CARGO_BIN_EXE_cubectl");

    // 2. Test cubectl list on empty root
    let list_out = Command::new(cubectl_bin)
        .args(["--state-root", state_root.to_str().unwrap(), "list"])
        .output()
        .unwrap();
    assert!(list_out.status.success());
    let list_str = String::from_utf8_lossy(&list_out.stdout);
    assert!(list_str.contains("No VMs configured"));

    // 3. Write a valid config
    let config1 = vms_dir.join("vm-one.json");
    let json_content1 = serde_json::json!({
        "name": "vm-one",
        "binary": fake_cube.to_str().unwrap(),
        "boot": {
            "mode": "uefi",
            "firmware": dummy_fw.to_str().unwrap()
        },
        "disks": [
            {"path": dummy_disk.to_str().unwrap(), "cow": false, "read_only": false}
        ],
        "ram_mb": 1024,
        "cpus": 2,
        "network": {
            "mode": "vmnet-proxy",
            "ssh_port": 2222
        },
        "vnc": {
            "bind": "127.0.0.1:5901"
        }
    });
    fs::write(&config1, serde_json::to_string_pretty(&json_content1).unwrap()).unwrap();

    // 4. Start vm-one
    let start_out = Command::new(cubectl_bin)
        .args([
            "--state-root", state_root.to_str().unwrap(),
            "start", config1.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    let start_stdout = String::from_utf8_lossy(&start_out.stdout);
    let start_stderr = String::from_utf8_lossy(&start_out.stderr);
    assert!(
        start_out.status.success(),
        "cubectl start failed:\nstdout: {}\nstderr: {}",
        start_stdout, start_stderr
    );
    let start_json: serde_json::Value = serde_json::from_str(&start_stdout).expect("valid json output");
    assert_eq!(start_json.get("state").and_then(|s| s.as_str()), Some("running"));
    assert_eq!(start_json.get("name").and_then(|n| n.as_str()), Some("vm-one"));
    assert_eq!(start_json.get("ip"), Some(&serde_json::Value::Null));

    let runtime_path = state_root.join("vm-one/runtime.json");
    let runtime_deadline = Instant::now() + Duration::from_secs(5);
    let initial_runtime: serde_json::Value = loop {
        if let Ok(bytes) = fs::read(&runtime_path) {
            if let Ok(value) = serde_json::from_slice(&bytes) {
                break value;
            }
        }
        assert!(
            Instant::now() < runtime_deadline,
            "fake VM did not publish valid runtime status at {}",
            runtime_path.display()
        );
        std::thread::sleep(Duration::from_millis(10));
    };
    assert_eq!(initial_runtime["instance_id"], start_json["id"], "manager must pass launch identity to cube");
    // A misleading log message must not override structured runtime status.
    fs::write(state_root.join("vm-one/vmm.log"),
        "[VirtIO-Net] Guest DHCP address: 192.168.64.99 (--ssh-port destination)\n".repeat(2000)).unwrap();
    fs::write(&runtime_path, serde_json::json!({
        "version": 1, "instance_id": start_json["id"], "ip": "192.168.64.3"
    }).to_string()).unwrap();

    // 5. Query status by name
    let status_out = Command::new(cubectl_bin)
        .args([
            "--state-root", state_root.to_str().unwrap(),
            "status", "vm-one",
        ])
        .output()
        .unwrap();
    assert!(status_out.status.success());
    let status_json: serde_json::Value = serde_json::from_str(&String::from_utf8_lossy(&status_out.stdout)).unwrap();
    assert_eq!(status_json.get("state").and_then(|s| s.as_str()), Some("running"));
    assert_eq!(status_json["ip"], "192.168.64.3");

    // 6. Test resource collision: try to start a second VM claiming the same disk and port
    let config2 = vms_dir.join("vm-two.json");
    let json_content2 = serde_json::json!({
        "name": "vm-two",
        "binary": fake_cube.to_str().unwrap(),
        "boot": {
            "mode": "uefi",
            "firmware": dummy_fw.to_str().unwrap()
        },
        "disks": [
            {"path": dummy_disk.to_str().unwrap(), "cow": false, "read_only": false}
        ],
        "ram_mb": 512,
        "cpus": 1,
        "network": {
            "mode": "vmnet-proxy",
            "ssh_port": 2222
        }
    });
    fs::write(&config2, serde_json::to_string_pretty(&json_content2).unwrap()).unwrap();

    let collision_out = Command::new(cubectl_bin)
        .args([
            "--state-root", state_root.to_str().unwrap(),
            "start", config2.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(!collision_out.status.success(), "expected collision failure");
    let collision_err = String::from_utf8_lossy(&collision_out.stderr);
    assert!(collision_err.contains("resource already used by a running VM"));

    // 7. Verify list shows vm-one as running
    let list_json_out = Command::new(cubectl_bin)
        .args([
            "--state-root", state_root.to_str().unwrap(),
            "list", "--json",
        ])
        .output()
        .unwrap();
    assert!(list_json_out.status.success());
    let list_items: Vec<serde_json::Value> = serde_json::from_str(&String::from_utf8_lossy(&list_json_out.stdout)).unwrap();
    assert_eq!(list_items.len(), 2);
    assert_eq!(list_items[0].get("name").and_then(|n| n.as_str()), Some("vm-one"));
    assert_eq!(list_items[0].get("state").and_then(|s| s.as_str()), Some("running"));
    assert_eq!(list_items[0]["ip"], "192.168.64.3");
    assert_eq!(list_items[1].get("name").and_then(|n| n.as_str()), Some("vm-two"));
    assert_eq!(list_items[1].get("state").and_then(|s| s.as_str()), Some("stopped"));
    assert_eq!(list_items[1].get("ip"), Some(&serde_json::Value::Null));

    // 8. Stop vm-one
    let stop_out = Command::new(cubectl_bin)
        .args([
            "--state-root", state_root.to_str().unwrap(),
            "stop", "vm-one", "--timeout", "5",
        ])
        .output()
        .unwrap();
    assert!(stop_out.status.success());
    let stop_json: serde_json::Value = serde_json::from_str(&String::from_utf8_lossy(&stop_out.stdout)).unwrap();
    assert_eq!(stop_json.get("ok").and_then(|o| o.as_bool()), Some(true));

    // Wait briefly for manager to write last.json
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        let status = Command::new(cubectl_bin)
            .args(["--state-root", state_root.to_str().unwrap(), "status", "vm-one"])
            .output()
            .unwrap();
        let val: serde_json::Value = serde_json::from_str(&String::from_utf8_lossy(&status.stdout)).unwrap();
        if val.get("state").and_then(|s| s.as_str()) == Some("stopped") {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }

    // Verify status is stopped
    let final_status = Command::new(cubectl_bin)
        .args([
            "--state-root", state_root.to_str().unwrap(),
            "status", "vm-one",
        ])
        .output()
        .unwrap();
    let final_json: serde_json::Value = serde_json::from_str(&String::from_utf8_lossy(&final_status.stdout)).unwrap();
    assert_eq!(final_json.get("state").and_then(|s| s.as_str()), Some("stopped"));
    assert_eq!(final_json.get("ip"), Some(&serde_json::Value::Null));

    // Cleanup
    let _ = fs::remove_dir_all(&temp_dir);
}

#[test]
fn test_cubectl_create_command() {
    let stamp = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    let temp_dir = std::env::temp_dir().join(format!("cubectl-create-test-{}-{stamp}", process::id()));
    fs::create_dir_all(&temp_dir).unwrap();
    let vm_dest = temp_dir.join("my-test-vm");
    let dummy_fw = temp_dir.join("QEMU_EFI.fd");
    fs::write(&dummy_fw, b"firmware").unwrap();

    let cubectl_bin = env!("CARGO_BIN_EXE_cubectl");

    let create_out = Command::new(cubectl_bin)
        .args([
            "create", "my-test-vm",
            "--state-root", temp_dir.join("state").to_str().unwrap(),
            "--dest", vm_dest.to_str().unwrap(),
            "--boot", "uefi",
            "--firmware", dummy_fw.to_str().unwrap(),
            "--disk", "rootfs.img:10G",
            "--ram", "4096",
            "--cpus", "4",
            "--ssh-port", "2223",
            "--vnc", "127.0.0.1:5902",
        ])
        .output()
        .unwrap();

    let create_stdout = String::from_utf8_lossy(&create_out.stdout);
    let create_stderr = String::from_utf8_lossy(&create_out.stderr);
    assert!(
        create_out.status.success(),
        "cubectl create failed:\nstdout: {}\nstderr: {}",
        create_stdout, create_stderr
    );
    assert!(create_stdout.contains("Successfully created VM 'my-test-vm'"));

    // Verify generated config file
    let config_file = vm_dest.join("my-test-vm.json");
    assert!(config_file.is_file(), "config file was not created");
    let config_val: serde_json::Value = serde_json::from_str(&fs::read_to_string(&config_file).unwrap()).unwrap();
    assert_eq!(config_val.get("name").and_then(|n| n.as_str()), Some("my-test-vm"));
    assert_eq!(config_val.get("ram_mb").and_then(|r| r.as_u64()), Some(4096));
    assert_eq!(config_val.get("cpus").and_then(|c| c.as_u64()), Some(4));
    assert_eq!(config_val["network"].get("ssh_port").and_then(|s| s.as_u64()), Some(2223));

    // Verify sparse disk creation
    let disk_file = vm_dest.join("rootfs.img");
    assert!(disk_file.is_file(), "sparse disk file was not created");
    let metadata = fs::metadata(&disk_file).unwrap();
    assert_eq!(metadata.len(), 10 * 1024 * 1024 * 1024);

    // Verify duplicate creation is rejected
    let dup_out = Command::new(cubectl_bin)
        .args([
            "create", "my-test-vm",
            "--state-root", temp_dir.join("state").to_str().unwrap(),
            "--dest", vm_dest.to_str().unwrap(),
            "--firmware", dummy_fw.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(!dup_out.status.success(), "expected failure on duplicate creation");
    assert!(String::from_utf8_lossy(&dup_out.stderr).contains("already exists"));

    let _ = fs::remove_dir_all(&temp_dir);
}

#[test]
fn test_cubectl_iso_workflow_and_eject() {
    let stamp = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    let temp_dir = std::env::temp_dir().join(format!("cubectl-iso-test-{}-{stamp}", process::id()));
    fs::create_dir_all(&temp_dir).unwrap();

    let vm_dest = temp_dir.join("ubuntu-vm");
    let dummy_fw = temp_dir.join("QEMU_EFI.fd");
    fs::write(&dummy_fw, b"firmware").unwrap();

    let dummy_iso = temp_dir.join("ubuntu-24.04-arm64.iso");
    fs::write(&dummy_iso, b"dummy iso image").unwrap();

    let cubectl_bin = env!("CARGO_BIN_EXE_cubectl");

    // 1. Create VM with --iso
    let create_out = Command::new(cubectl_bin)
        .args([
            "create", "ubuntu-vm",
            "--state-root", temp_dir.join("state").to_str().unwrap(),
            "--dest", vm_dest.to_str().unwrap(),
            "--firmware", dummy_fw.to_str().unwrap(),
            "--iso", dummy_iso.to_str().unwrap(),
        ])
        .output()
        .unwrap();

    let create_stdout = String::from_utf8_lossy(&create_out.stdout);
    assert!(create_out.status.success(), "create failed: {}", String::from_utf8_lossy(&create_out.stderr));
    assert!(create_stdout.contains("installer ISO, read-only"));

    // 2. Check JSON configuration
    let config_file = vm_dest.join("ubuntu-vm.json");
    let config_val: serde_json::Value = serde_json::from_str(&fs::read_to_string(&config_file).unwrap()).unwrap();
    let vnc_bind = config_val["vnc"]["bind"].as_str().unwrap();
    let disks = config_val.get("disks").and_then(|d| d.as_array()).unwrap();
    assert_eq!(disks.len(), 2, "expected primary disk and iso disk");
    assert_eq!(disks[1].get("read_only").and_then(|ro| ro.as_bool()), Some(true));
    let address: std::net::SocketAddr = vnc_bind.parse().unwrap();
    assert!(create_stdout.contains(&format!(
        "vncviewer {}::{}",
        address.ip(),
        address.port()
    )));
    assert_eq!(address.ip(), std::net::Ipv4Addr::LOCALHOST);
    assert!(address.port() >= 5901);

    // 3. Eject the ISO
    let eject_out = Command::new(cubectl_bin)
        .args(["eject", config_file.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(eject_out.status.success());
    let eject_stdout = String::from_utf8_lossy(&eject_out.stdout);
    assert!(eject_stdout.contains("Ejected installer ISO from \"ubuntu-vm\""));

    // 4. Verify config now has only 1 disk
    let updated_val: serde_json::Value = serde_json::from_str(&fs::read_to_string(&config_file).unwrap()).unwrap();
    let updated_disks = updated_val.get("disks").and_then(|d| d.as_array()).unwrap();
    assert_eq!(updated_disks.len(), 1, "expected only primary disk after eject");

    // 5. Ejecting again reports no ISO attached
    let eject2_out = Command::new(cubectl_bin)
        .args(["eject", config_file.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(eject2_out.status.success());
    assert!(String::from_utf8_lossy(&eject2_out.stdout).contains("No installer ISO disks found"));

    let _ = fs::remove_dir_all(&temp_dir);
}

#[test]
fn test_cubectl_unattended_install_cli_options() {
    let cubectl_bin = env!("CARGO_BIN_EXE_cubectl");

    // 1. Verify create --help has unattended flags
    let help_create = Command::new(cubectl_bin)
        .args(["create", "--help"])
        .output()
        .unwrap();
    assert!(help_create.status.success());
    let help_create_str = String::from_utf8_lossy(&help_create.stdout);
    assert!(help_create_str.contains("--unattended"));
    assert!(help_create_str.contains("--filesystem"));
    assert!(help_create_str.contains("--ssh-key"));
    assert!(help_create_str.contains("--password"));

    // 2. Verify install --help has install options
    let help_inst = Command::new(cubectl_bin)
        .args(["install", "--help"])
        .output()
        .unwrap();
    assert!(help_inst.status.success());
    let help_inst_str = String::from_utf8_lossy(&help_inst.stdout);
    assert!(help_inst_str.contains("--iso"));
    assert!(help_inst_str.contains("--filesystem"));
    assert!(help_inst_str.contains("--password"));

    // 3. Test missing ISO with --unattended
    let err_out = Command::new(cubectl_bin)
        .args(["create", "test-vm", "--dest", "target/test-vm-unattended", "--unattended", "--iso", "non-existent-iso.iso"])
        .output()
        .unwrap();
    assert!(!err_out.status.success());
    let stderr_str = String::from_utf8_lossy(&err_out.stderr);
    assert!(stderr_str.contains("ISO file not found"));
    let _ = fs::remove_dir_all("target/test-vm-unattended");
}

#[test]
fn test_cubectl_delete_command() {
    let cubectl_bin = env!("CARGO_BIN_EXE_cubectl");
    let test_dir = std::path::PathBuf::from("target/test-delete-vm");
    let _ = fs::remove_dir_all(&test_dir);
    fs::create_dir_all(&test_dir).unwrap();

    let disk_file = test_dir.join("rootfs.img");
    fs::write(&disk_file, b"disk data").unwrap();

    let config_file = test_dir.join("del-vm.json");
    let config_json = serde_json::json!({
        "name": "del-vm",
        "boot": { "mode": "uefi", "firmware": "fw.fd" },
        "disks": [disk_file.to_str().unwrap()]
    });
    fs::write(&config_file, serde_json::to_string_pretty(&config_json).unwrap()).unwrap();

    assert!(config_file.is_file());
    assert!(disk_file.is_file());

    // Run cubectl delete
    let del_out = Command::new(cubectl_bin)
        .args(["delete", config_file.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(del_out.status.success(), "delete failed: {}", String::from_utf8_lossy(&del_out.stderr));

    // Verify config and disk were deleted
    assert!(!config_file.exists());
    assert!(!disk_file.exists());

    let _ = fs::remove_dir_all(&test_dir);
}

#[test]
fn test_cubectl_delete_kernel_and_directory() {
    let cubectl_bin = env!("CARGO_BIN_EXE_cubectl");
    let test_root = std::path::PathBuf::from("target/test-vms-delete-root");
    let _ = fs::remove_dir_all(&test_root);
    fs::create_dir_all(&test_root).unwrap();

    let vm_dir = test_root.join("test-bsd-vm");
    fs::create_dir_all(&vm_dir).unwrap();

    let disk_file = vm_dir.join("disk.raw");
    fs::write(&disk_file, b"disk data").unwrap();

    let kernel_file = vm_dir.join("kernel");
    fs::write(&kernel_file, b"kernel data").unwrap();

    let marker_file = vm_dir.join(".installed");
    fs::write(&marker_file, b"installed marker").unwrap();

    let config_file = vm_dir.join("test-bsd-vm.json");
    let config_json = serde_json::json!({
        "name": "test-bsd-vm",
        "boot": { "mode": "freebsd", "kernel": "kernel" },
        "disks": ["disk.raw"]
    });
    fs::write(&config_file, serde_json::to_string_pretty(&config_json).unwrap()).unwrap();

    assert!(config_file.is_file());
    assert!(kernel_file.is_file());
    assert!(disk_file.is_file());
    assert!(marker_file.is_file());

    // Run cubectl delete with CUBE_VMS_DIR pointing to test_root
    let del_out = Command::new(cubectl_bin)
        .env("CUBE_VMS_DIR", test_root.to_str().unwrap())
        .args(["delete", "test-bsd-vm"])
        .output()
        .unwrap();
    assert!(del_out.status.success(), "delete failed: {}", String::from_utf8_lossy(&del_out.stderr));

    // Verify VM directory and its contents (kernel, disk, installed marker) were deleted
    assert!(!vm_dir.exists(), "VM directory should have been removed");
    assert!(!kernel_file.exists(), "kernel file should have been removed");

    let _ = fs::remove_dir_all(&test_root);
}

#[test]
fn test_cubectl_start_no_reboot_option() {
    let cubectl_bin = env!("CARGO_BIN_EXE_cubectl");

    // 1. Verify start --help displays --no-reboot
    let help_out = Command::new(cubectl_bin)
        .args(["start", "--help"])
        .output()
        .unwrap();
    assert!(help_out.status.success());
    let help_str = String::from_utf8_lossy(&help_out.stdout);
    assert!(help_str.contains("--no-reboot"));

    // 2. Verify that --no-reboot is passed into launch.json command
    let test_dir = std::path::PathBuf::from("target/test-no-reboot");
    let _ = fs::remove_dir_all(&test_dir);
    fs::create_dir_all(&test_dir).unwrap();

    let state_root = test_dir.join("state");
    fs::create_dir_all(&state_root).unwrap();

    let disk_file = test_dir.join("disk.img");
    fs::write(&disk_file, b"test").unwrap();

    let fw_file = test_dir.join("fw.fd");
    fs::write(&fw_file, b"test firmware").unwrap();

    let fake_cube = test_dir.join("fake-cube");
    fs::write(&fake_cube, "#!/bin/sh\nexit 0\n").unwrap();
    use std::os::unix::fs::PermissionsExt;
    let _ = fs::set_permissions(&fake_cube, fs::Permissions::from_mode(0o755));

    let config_file = test_dir.join("no-reboot-vm.json");
    let config_json = serde_json::json!({
        "name": "no-reboot-vm",
        "binary": fake_cube.to_str().unwrap(),
        "boot": { "mode": "uefi", "firmware": "fw.fd" },
        "disks": ["disk.img"]
    });
    fs::write(&config_file, serde_json::to_string_pretty(&config_json).unwrap()).unwrap();

    let state_dir = state_root.join("no-reboot-vm");
    fs::create_dir_all(&state_dir).unwrap();

    // Start with --no-reboot (it will fail to spawn or timeout, but launch.json is written first)
    let start_out = Command::new(cubectl_bin)
        .args([
            "--state-root", state_root.to_str().unwrap(),
            "start", config_file.to_str().unwrap(),
            "--no-reboot",
        ])
        .output()
        .unwrap();

    let launch_file = state_dir.join("launch.json");
    assert!(launch_file.is_file(), "launch.json was not written: {}", String::from_utf8_lossy(&start_out.stderr));
    let launch_val: serde_json::Value = serde_json::from_str(&fs::read_to_string(&launch_file).unwrap()).unwrap();
    let cmd_args = launch_val.get("command").and_then(|c| c.as_array()).unwrap();
    let has_no_reboot = cmd_args.iter().any(|arg| arg.as_str() == Some("--no-reboot"));
    assert!(has_no_reboot, "launch spec command must include --no-reboot: {:?}", cmd_args);

    // The fake VM can acknowledge startup just before exiting. Wait for the
    // manager to release its resources before reusing the same disk.
    let deadline = Instant::now() + Duration::from_secs(5);
    while !state_dir.join("last.json").is_file() {
        assert!(Instant::now() < deadline, "fake VM did not exit");
        std::thread::sleep(Duration::from_millis(20));
    }

    // 3. Verify auto-detection of --no-reboot when an ISO is attached (without explicit --no-reboot CLI flag)
    let iso_file = test_dir.join("installer.iso");
    fs::write(&iso_file, b"fake iso").unwrap();

    let iso_config_file = test_dir.join("iso-vm.json");
    let iso_config_json = serde_json::json!({
        "name": "iso-vm",
        "binary": fake_cube.to_str().unwrap(),
        "boot": { "mode": "uefi", "firmware": "fw.fd" },
        "disks": ["disk.img", "installer.iso"]
    });
    fs::write(&iso_config_file, serde_json::to_string_pretty(&iso_config_json).unwrap()).unwrap();

    let iso_state_dir = state_root.join("iso-vm");
    fs::create_dir_all(&iso_state_dir).unwrap();

    let _ = Command::new(cubectl_bin)
        .args([
            "--state-root", state_root.to_str().unwrap(),
            "start", iso_config_file.to_str().unwrap(),
        ])
        .output()
        .unwrap();

    let iso_launch_file = iso_state_dir.join("launch.json");
    assert!(iso_launch_file.is_file());
    let iso_launch_val: serde_json::Value = serde_json::from_str(&fs::read_to_string(&iso_launch_file).unwrap()).unwrap();
    let iso_cmd_args = iso_launch_val.get("command").and_then(|c| c.as_array()).unwrap();
    let iso_has_no_reboot = iso_cmd_args.iter().any(|arg| arg.as_str() == Some("--no-reboot"));
    assert!(iso_has_no_reboot, "launch spec command must automatically include --no-reboot when ISO is attached: {:?}", iso_cmd_args);

    let _ = fs::remove_dir_all(&test_dir);
}

#[test]
fn test_cubectl_preseed_initrd_subcommand() {
    let cubectl_bin = env!("CARGO_BIN_EXE_cubectl");
    let test_dir = std::path::PathBuf::from("target/test-preseed-initrd");
    let _ = fs::remove_dir_all(&test_dir);
    fs::create_dir_all(&test_dir).unwrap();

    // 1. Create a minimal valid CPIO archive containing "hello.txt"
    let file_content = b"Hello from initrd!";
    let name_bytes = b"hello.txt\0";
    let header = format!(
        "070701{:08X}{:08X}{:08X}{:08X}{:08X}{:08X}{:08X}{:08X}{:08X}{:08X}{:08X}{:08X}{:08X}",
        1, 0o100644, 0, 0, 1, 1000, file_content.len(), 0, 0, 0, 0, name_bytes.len(), 0
    );
    let mut cpio = header.into_bytes();
    cpio.extend_from_slice(name_bytes);
    while cpio.len() % 4 != 0 { cpio.push(0); }
    cpio.extend_from_slice(file_content);
    while cpio.len() % 4 != 0 { cpio.push(0); }

    // Trailer
    let trailer_name = b"TRAILER!!!\0";
    let trailer_header = format!(
        "070701{:08X}{:08X}{:08X}{:08X}{:08X}{:08X}{:08X}{:08X}{:08X}{:08X}{:08X}{:08X}{:08X}",
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, trailer_name.len(), 0
    );
    cpio.extend_from_slice(trailer_header.as_bytes());
    cpio.extend_from_slice(trailer_name);
    while cpio.len() % 512 != 0 { cpio.push(0); }

    // Compress with gzip
    use std::io::Write;
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    encoder.write_all(&cpio).unwrap();
    let gz_data = encoder.finish().unwrap();

    let in_initrd = test_dir.join("initrd.gz");
    fs::write(&in_initrd, &gz_data).unwrap();

    let preseed_file = test_dir.join("preseed.cfg");
    fs::write(&preseed_file, b"d-i debian-installer/locale string en_US\n").unwrap();

    let out_initrd = test_dir.join("initrd-preseeded.gz");

    // 2. Run cubectl preseed-initrd
    let status = Command::new(cubectl_bin)
        .args([
            "preseed-initrd",
            in_initrd.to_str().unwrap(),
            out_initrd.to_str().unwrap(),
            preseed_file.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(status.status.success(), "cubectl preseed-initrd failed: {}", String::from_utf8_lossy(&status.stderr));

    // 3. Decompress and verify preseed.cfg is present
    use std::io::Read;
    let out_gz = fs::read(&out_initrd).unwrap();
    let mut decoder = flate2::read::GzDecoder::new(&out_gz[..]);
    let mut decompressed = Vec::new();
    decoder.read_to_end(&mut decompressed).unwrap();

    let decompressed_str = String::from_utf8_lossy(&decompressed);
    assert!(decompressed_str.contains("preseed.cfg"), "missing preseed.cfg entry in output cpio");
    assert!(decompressed_str.contains("d-i debian-installer/locale string en_US"), "missing preseed content");
    assert!(decompressed_str.contains("hello.txt"), "original file lost");
    assert!(decompressed_str.contains("TRAILER!!!"), "trailer missing");

    let _ = fs::remove_dir_all(&test_dir);
}

#[test]
fn test_cubectl_extract_kernel_and_patch_shadow() {
    let cubectl_bin = env!("CARGO_BIN_EXE_cubectl");
    let cloud_img = std::path::Path::new("debian-cloud.img");
    if !cloud_img.exists() {
        return;
    }

    let temp_dir = std::env::temp_dir().join(format!("cubectl-ext4-test-{}", std::process::id()));
    fs::create_dir_all(&temp_dir).unwrap();

    let out_k = temp_dir.join("test-vmlinuz");
    let out_i = temp_dir.join("test-initrd");
    let out_r = temp_dir.join("test-rootpart");

    let status = Command::new(cubectl_bin)
        .args([
            "extract-kernel",
            cloud_img.to_str().unwrap(),
            out_k.to_str().unwrap(),
            out_i.to_str().unwrap(),
            "--rootpart",
            out_r.to_str().unwrap(),
        ])
        .output()
        .unwrap();

    assert!(status.status.success(), "extract-kernel failed: {}", String::from_utf8_lossy(&status.stderr));
    assert!(out_k.is_file(), "kernel file not created");
    assert!(out_i.is_file(), "initrd file not created");
    assert!(out_r.is_file(), "rootpart file not created");
    assert!(fs::metadata(&out_k).unwrap().len() > 10 * 1024 * 1024);
    assert!(fs::metadata(&out_i).unwrap().len() > 10 * 1024 * 1024);

    let rootpart_str = fs::read_to_string(&out_r).unwrap();
    assert!(rootpart_str.starts_with("/dev/vda"));

    let _ = fs::remove_dir_all(&temp_dir);
}

#[test]
fn test_cubectl_create_with_version_and_existing_cache() {
    let cubectl_bin = env!("CARGO_BIN_EXE_cubectl");
    let temp_dir = std::env::temp_dir().join(format!("cubectl-ver-test-{}", std::process::id()));
    fs::create_dir_all(&temp_dir).unwrap();

    // Verify invalid version is rejected
    let bad_ver = Command::new(cubectl_bin)
        .args(["create", "bad-ver-vm", "--version", "99.9", "--dest", temp_dir.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(!bad_ver.status.success());
    assert!(String::from_utf8_lossy(&bad_ver.stderr).contains("unknown FreeBSD version"));

    let _ = fs::remove_dir_all(&temp_dir);
}

#[test]
fn test_cubectl_config_commands() {
    let cubectl_bin = env!("CARGO_BIN_EXE_cubectl");
    let state_dir = std::env::temp_dir().join(format!("cubectl-cfg-test-{}", std::process::id()));
    let custom_vms = state_dir.join("MyCustomVMs");
    fs::create_dir_all(&state_dir).unwrap();

    // 1. List default config
    let list_out = Command::new(cubectl_bin)
        .args(["--state-root", state_dir.to_str().unwrap(), "config", "list"])
        .output()
        .unwrap();
    assert!(list_out.status.success());
    let list_str = String::from_utf8_lossy(&list_out.stdout);
    assert!(list_str.contains("vms-dir ="));

    // 2. Set custom vms-dir
    let set_out = Command::new(cubectl_bin)
        .args(["--state-root", state_dir.to_str().unwrap(), "config", "set", "vms-dir", custom_vms.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(set_out.status.success());
    assert!(custom_vms.is_dir());

    // 3. Verify settings.json exists
    let settings_file = state_dir.join("settings.json");
    assert!(settings_file.is_file());

    // 4. Reset vms-dir to default
    let reset_out = Command::new(cubectl_bin)
        .args(["--state-root", state_dir.to_str().unwrap(), "config", "set", "vms-dir", "default"])
        .output()
        .unwrap();
    assert!(reset_out.status.success());

    let _ = fs::remove_dir_all(&state_dir);
}

#[test]
fn test_cubectl_ssh_command_behavior() {
    let cubectl_bin = env!("CARGO_BIN_EXE_cubectl");
    let root = review_test_dir("ssh-test");

    // 1. Verify --help output contains expected options
    let help_out = Command::new(cubectl_bin)
        .args(["ssh", "--help"])
        .output()
        .unwrap();
    assert!(help_out.status.success());
    let help_str = String::from_utf8_lossy(&help_out.stdout);
    assert!(help_str.contains("-u, --user <USER>"));
    assert!(help_str.contains("-i, --identity <IDENTITY>"));
    assert!(help_str.contains("-p, --port <PORT>"));
    assert!(help_str.contains("-w, --wait"));
    assert!(help_str.contains("[COMMAND]..."));

    // 2. Calling ssh on a stopped / non-existent VM returns clear error
    let ssh_out = Command::new(cubectl_bin)
        .args(["--state-root", root.join("state").to_str().unwrap(), "ssh", "inactive-vm"])
        .output()
        .unwrap();
    assert!(!ssh_out.status.success());
    let stderr = String::from_utf8_lossy(&ssh_out.stderr);
    assert!(stderr.contains("VM 'inactive-vm' is not running. Start it first with 'cubectl start inactive-vm'"));

    let _ = fs::remove_dir_all(&root);
}

