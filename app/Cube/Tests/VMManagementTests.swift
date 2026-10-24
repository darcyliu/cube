import Foundation

@main
struct VMManagementTests {
    static func main() throws {
        let fm = FileManager.default
        let dir = fm.temporaryDirectory.appendingPathComponent("cube-management-\(UUID().uuidString)")
        try fm.createDirectory(at: dir, withIntermediateDirectories: true)
        defer { try? fm.removeItem(at: dir) }

        let cache = dir.appendingPathComponent("cache")
        let manager = ImageCacheManager(cacheDirectory: cache.path)
        let cached = cache.appendingPathComponent("installer.iso")
        let original = Data("original image".utf8)
        try original.write(to: cached)
        _ = try manager.importLocalISO(from: cached)
        precondition(tryData(cached) == original, "Importing a cached image must preserve it")

        let sourceDir = dir.appendingPathComponent("source")
        try fm.createDirectory(at: sourceDir, withIntermediateDirectories: true)
        let source = sourceDir.appendingPathComponent("installer.iso")
        try fm.linkItem(at: cached, to: source)
        _ = try manager.importLocalISO(from: source)
        precondition(tryData(cached) == original, "Hard-link aliases must preserve the image")
        try fm.removeItem(at: source)
        do {
            _ = try manager.importLocalISO(from: source)
            preconditionFailure("Missing source should fail")
        } catch {}
        precondition(tryData(cached) == original, "Failed imports must preserve the cached image")
        let replacement = Data("replacement image".utf8)
        try replacement.write(to: source)
        _ = try manager.importLocalISO(from: source)
        precondition(tryData(cached) == replacement)
        precondition(tryData(source) == replacement, "Import must preserve the source")

        let desktopCreate = CreateVMOptions(
            name: "desktop", osVersion: "15.1", customIsoPath: nil,
            isUnattended: true, desktop: "xfce", cpus: 4, ramMB: 4096,
            diskSizeGB: 20, password: "desktop-secret", netMode: "vmnet-proxy",
            sshPort: 2222, vncPort: 5901, destinationDir: nil,
            sharePath: nil, shareTag: nil
        )
        let desktopCreateArgs = VMManager.shared.createArguments(options: desktopCreate)
        precondition(desktopCreateArgs.contains("--unattended"))
        precondition(desktopCreateArgs[desktopCreateArgs.firstIndex(of: "--desktop")! + 1] == "xfce")
        precondition(desktopCreateArgs[desktopCreateArgs.firstIndex(of: "--desktop-user")! + 1] == "cube")
        precondition(desktopCreateArgs[desktopCreateArgs.firstIndex(of: "--desktop-password")! + 1] == "desktop-secret")
        precondition(desktopCreateArgs[desktopCreateArgs.firstIndex(of: "--display")! + 1] == "1280x800")

        let vmDir = dir.appendingPathComponent("VM's files with spaces")
        try fm.createDirectory(at: vmDir, withIntermediateDirectories: true)
        let configURL = vmDir.appendingPathComponent("example.json")
        let marker = dir.appendingPathComponent("must-not-exist")
        let bootargs = "console=ttyAMA0 'quoted' $(touch \(marker.path)); echo unsafe"
        var boot: [String: Any] = [
            "mode": "freebsd", "kernel": "kernel", "modules": ["modules/zfs.ko"],
            "root": "zfs:zroot/ROOT/default", "bootargs": bootargs
        ]
        var includeVNC = true
        func writeConfig() throws {
            var config: [String: Any] = [
                "name": "example", "boot": boot, "network": ["mode": "none"],
                "disks": ["disk with spaces.img"]
            ]
            if includeVNC {
                config["vnc"] = ["bind": "127.0.0.1:5999", "display": "1440x900"]
            }
            try JSONSerialization.data(withJSONObject: config).write(to: configURL)
        }
        try writeConfig()
        let vm = VMInfo(name: "example", state: "stopped", pid: nil, started: nil,
                        ip: nil, ssh_port: nil, vnc: nil, config: configURL.path, os: nil)
        // Execute the copied shell command against a harmless function to inspect
        // the actual arguments after shell parsing, including metacharacters.
        let args = try shellArguments(vm.generateCubeCommand())
        func value(_ flag: String) -> String {
            args[args.firstIndex(of: flag)! + 1]
        }
        precondition(value("--freebsd") == vmDir.appendingPathComponent("kernel").path)
        precondition(value("--disk") == vmDir.appendingPathComponent("disk with spaces.img").path)
        precondition(value("--bootargs") == bootargs)
        precondition(value("--module") == vmDir.appendingPathComponent("modules/zfs.ko").path)
        precondition(args.contains("--multiuser"), "Default FreeBSD boot must be multiuser")
        precondition(value("--vnc") == "127.0.0.1:5999")
        precondition(!fm.fileExists(atPath: marker.path), "Shell substitutions must remain literal")

        let nativeArgs = try shellArguments(vm.generateCubeCommand(displayMode: .native))
        precondition(!nativeArgs.contains("--vnc"), "Native command must omit VNC")
        precondition(nativeArgs[nativeArgs.firstIndex(of: "--gui")! + 1] == "native")
        precondition(nativeArgs[nativeArgs.firstIndex(of: "--display")! + 1] == "1440x900")
        precondition(vm.managedStartArguments(displayMode: .native) ==
                     ["start", "example", "--gui", "native", "--display", "1440x900"])
        precondition(VMInfo.tigerVNCAddress(for: "127.0.0.1:5999") == "127.0.0.1::5999")
        precondition(VMInfo.tigerVNCAddress(for: "[::1]:5999") == "[::1]::5999")
        precondition(VMInfo.tigerVNCAddress(for: "127.0.0.1:not-a-port") == nil)

        let vncArgs = try shellArguments(vm.generateCubeCommand(displayMode: .vnc))
        precondition(!vncArgs.contains("--gui"), "VNC command must omit native GUI")
        precondition(vncArgs[vncArgs.firstIndex(of: "--vnc")! + 1] == "127.0.0.1:5999")
        precondition(vm.managedStartArguments(displayMode: .vnc) ==
                     ["start", "example", "--vnc", "127.0.0.1:5999", "--display", "1440x900"])

        includeVNC = false
        try writeConfig()
        let fallbackVNCArgs = try shellArguments(vm.generateCubeCommand(displayMode: .vnc))
        precondition(fallbackVNCArgs[fallbackVNCArgs.firstIndex(of: "--vnc")! + 1] == "127.0.0.1:5901")
        precondition(fallbackVNCArgs[fallbackVNCArgs.firstIndex(of: "--display")! + 1] == "1280x720")
        precondition(vm.managedStartArguments(displayMode: .native) ==
                     ["start", "example", "--gui", "native", "--display", "1280x720"])

        boot["multiuser"] = false
        try writeConfig()
        let singleUserArgs = try shellArguments(vm.generateCubeCommand())
        precondition(!singleUserArgs.contains("--multiuser"))
        print("VM management regression checks passed")
    }

    static func tryData(_ url: URL) -> Data {
        try! Data(contentsOf: url)
    }

    static func shellArguments(_ command: String) throws -> [String] {
        let process = Process()
        process.executableURL = URL(fileURLWithPath: "/bin/sh")
        process.arguments = ["-c", "cube() { printf '%s\\0' \"$@\"; }\n" + command]
        let output = Pipe()
        process.standardOutput = output
        try process.run()
        let data = output.fileHandleForReading.readDataToEndOfFile()
        process.waitUntilExit()
        precondition(process.terminationStatus == 0)
        return data.split(separator: 0).map { String(decoding: $0, as: UTF8.self) }
    }
}
