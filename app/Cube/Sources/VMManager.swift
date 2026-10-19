import Foundation
import AppKit
import Security
import Darwin

struct VMInfo: Decodable {
    let name: String
    let state: String
    let pid: Int?
    let started: Double?
    let ip: String?
    let ssh_port: Int?
    let vnc: String?
    let config: String?
    let os: String?

    var isRunning: Bool {
        return state.lowercased() == "running"
    }
}

struct VMShareConfig {
    var tag: String
    var path: String
    var read_only: Bool
}

struct CreateVMOptions {
    var name: String
    var osVersion: String // "14.4", "15.1", or "custom"
    var customIsoPath: String?
    var isUnattended: Bool
    var cpus: Int
    var ramMB: Int
    var diskSizeGB: Int
    var password: String
    var netMode: String // "vmnet-proxy", "vmnet", "bridge", or "none"
    var sshPort: Int?
    var vncPort: Int?
    var destinationDir: String?
    var sharePath: String?
    var shareTag: String?
    var shareReadOnly: Bool = false
    var memPrealloc: Bool = false
}

class VMManager {
    static let shared = VMManager()

    private(set) var vms: [VMInfo] = []
    private var isRefreshing = false

    var vmsDirectory: String {
        get {
            let home = NSHomeDirectory()
            let settingsUrl = URL(fileURLWithPath: "\(home)/.local/state/cube/settings.json")
            if let data = try? Data(contentsOf: settingsUrl),
               let json = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
               let dir = json["vms_dir"] as? String, !dir.trimmingCharacters(in: .whitespaces).isEmpty {
                return (dir as NSString).expandingTildeInPath
            }
            if let env = ProcessInfo.processInfo.environment["CUBE_VMS_DIR"], !env.isEmpty {
                return (env as NSString).expandingTildeInPath
            }
            return "\(home)/Cube"
        }
        set {
            let home = NSHomeDirectory()
            let stateDir = URL(fileURLWithPath: "\(home)/.local/state/cube")
            try? FileManager.default.createDirectory(at: stateDir, withIntermediateDirectories: true)
            let settingsUrl = stateDir.appendingPathComponent("settings.json")
            var json: [String: Any] = [:]
            if let data = try? Data(contentsOf: settingsUrl),
               let existing = try? JSONSerialization.jsonObject(with: data) as? [String: Any] {
                json = existing
            }
            let trimmed = newValue.trimmingCharacters(in: .whitespacesAndNewlines)
            if trimmed.isEmpty || trimmed == "\(home)/Cube" || trimmed == "~/Cube" {
                json.removeValue(forKey: "vms_dir")
            } else {
                json["vms_dir"] = (trimmed as NSString).expandingTildeInPath
            }
            if let data = try? JSONSerialization.data(withJSONObject: json, options: [.prettyPrinted]) {
                try? data.write(to: settingsUrl)
            }
        }
    }

    struct TerminalAppInfo {
        let name: String
        let path: String
        let bundleId: String?
    }

    var preferredTerminalApp: String {
        get {
            let home = NSHomeDirectory()
            let settingsUrl = URL(fileURLWithPath: "\(home)/.local/state/cube/settings.json")
            if let data = try? Data(contentsOf: settingsUrl),
               let json = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
               let term = json["terminal_app"] as? String, !term.trimmingCharacters(in: .whitespaces).isEmpty {
                return term
            }
            return "Terminal"
        }
        set {
            let home = NSHomeDirectory()
            let stateDir = URL(fileURLWithPath: "\(home)/.local/state/cube")
            try? FileManager.default.createDirectory(at: stateDir, withIntermediateDirectories: true)
            let settingsUrl = stateDir.appendingPathComponent("settings.json")
            var json: [String: Any] = [:]
            if let data = try? Data(contentsOf: settingsUrl),
               let existing = try? JSONSerialization.jsonObject(with: data) as? [String: Any] {
                json = existing
            }
            let trimmed = newValue.trimmingCharacters(in: .whitespacesAndNewlines)
            if trimmed.isEmpty || trimmed == "Terminal" || trimmed == "/System/Applications/Utilities/Terminal.app" {
                json.removeValue(forKey: "terminal_app")
            } else {
                json["terminal_app"] = trimmed
            }
            if let data = try? JSONSerialization.data(withJSONObject: json, options: [.prettyPrinted]) {
                try? data.write(to: settingsUrl)
            }
        }
    }

    func detectAvailableTerminalApps() -> [TerminalAppInfo] {
        let candidates: [(name: String, paths: [String], bundleId: String)] = [
            ("Terminal", ["/System/Applications/Utilities/Terminal.app"], "com.apple.Terminal"),
            ("iTerm2", ["/Applications/iTerm.app", "\(NSHomeDirectory())/Applications/iTerm.app"], "com.googlecode.iterm2"),
            ("Ghostty", ["/Applications/Ghostty.app", "\(NSHomeDirectory())/Applications/Ghostty.app"], "com.mitchellh.ghostty"),
            ("Warp", ["/Applications/Warp.app", "\(NSHomeDirectory())/Applications/Warp.app"], "dev.warp.Warp-Stable"),
            ("Alacritty", ["/Applications/Alacritty.app", "\(NSHomeDirectory())/Applications/Alacritty.app"], "org.alacritty"),
            ("Kitty", ["/Applications/kitty.app", "\(NSHomeDirectory())/Applications/kitty.app"], "net.kovidgoyal.kitty")
        ]

        var results: [TerminalAppInfo] = []
        for cand in candidates {
            if let existing = cand.paths.first(where: { FileManager.default.fileExists(atPath: $0) }) {
                results.append(TerminalAppInfo(name: cand.name, path: existing, bundleId: cand.bundleId))
            } else if let url = NSWorkspace.shared.urlForApplication(withBundleIdentifier: cand.bundleId) {
                results.append(TerminalAppInfo(name: cand.name, path: url.path, bundleId: cand.bundleId))
            }
        }
        return results
    }

    func resolveTerminalAppURL() -> URL {
        let pref = preferredTerminalApp
        if pref.hasPrefix("/") && FileManager.default.fileExists(atPath: pref) {
            return URL(fileURLWithPath: pref)
        }
        let available = detectAvailableTerminalApps()
        if let found = available.first(where: { $0.name.lowercased() == pref.lowercased() || $0.path == pref }) {
            return URL(fileURLWithPath: found.path)
        }
        return URL(fileURLWithPath: "/System/Applications/Utilities/Terminal.app")
    }

    func hasNetworkingEntitlement() -> Bool {
        // 1. Check embedded helper bundle
        let helperUrl = Bundle.main.bundleURL.appendingPathComponent("Contents/Helpers/cube.app")
        if FileManager.default.fileExists(atPath: helperUrl.path) {
            var staticCode: SecStaticCode?
            if SecStaticCodeCreateWithPath(helperUrl as CFURL, [], &staticCode) == errSecSuccess, let sCode = staticCode {
                var info: CFDictionary?
                if SecCodeCopySigningInformation(sCode, SecCSFlags(rawValue: kSecCSSigningInformation), &info) == errSecSuccess,
                   let dict = info as? [String: Any],
                   let entitlements = dict["entitlements-dict"] as? [String: Any],
                   (entitlements["com.apple.vm.networking"] as? Bool) == true {
                    return true
                }
            }
        }

        // 2. Check main app bundle
        let mainAppUrl = Bundle.main.bundleURL
        var staticCode: SecStaticCode?
        if SecStaticCodeCreateWithPath(mainAppUrl as CFURL, [], &staticCode) == errSecSuccess, let sCode = staticCode {
            var info: CFDictionary?
            if SecCodeCopySigningInformation(sCode, SecCSFlags(rawValue: kSecCSSigningInformation), &info) == errSecSuccess,
               let dict = info as? [String: Any],
               let entitlements = dict["entitlements-dict"] as? [String: Any],
               (entitlements["com.apple.vm.networking"] as? Bool) == true {
                return true
            }
        }

        return false
    }

    func findCubectlBinary() -> String? {
        // 1. Check embedded in app bundle
        let bundlePath = Bundle.main.bundleURL.appendingPathComponent("Contents/MacOS/cubectl").path
        if FileManager.default.isExecutableFile(atPath: bundlePath) {
            return bundlePath
        }
        // 2. Check build output directory relative to app
        let devPath = Bundle.main.bundleURL.deletingLastPathComponent().deletingLastPathComponent()
            .appendingPathComponent("target/debug/cubectl").path
        if FileManager.default.isExecutableFile(atPath: devPath) {
            return devPath
        }
        // 3. Current working directory
        let localPath = "target/debug/cubectl"
        if FileManager.default.isExecutableFile(atPath: localPath) {
            return localPath
        }
        // 4. PATH lookup
        let usrLocal = "/usr/local/bin/cubectl"
        if FileManager.default.isExecutableFile(atPath: usrLocal) {
            return usrLocal
        }
        return nil
    }

    func refreshVMs(completion: (([VMInfo]) -> Void)? = nil) {
        guard !isRefreshing else { return }
        guard let bin = findCubectlBinary() else {
            completion?(self.vms)
            return
        }

        isRefreshing = true
        DispatchQueue.global(qos: .userInitiated).async {
            let proc = Process()
            proc.executableURL = URL(fileURLWithPath: bin)
            proc.arguments = ["list", "--json"]

            let pipe = Pipe()
            proc.standardOutput = pipe
            proc.standardError = Pipe()

            do {
                try proc.run()
                proc.waitUntilExit()

                let data = pipe.fileHandleForReading.readDataToEndOfFile()
                if let decoded = try? JSONDecoder().decode([VMInfo].self, from: data) {
                    DispatchQueue.main.async {
                        self.vms = decoded
                        self.isRefreshing = false
                        completion?(decoded)
                    }
                    return
                }
            } catch {
                print("[VMManager] Error listing VMs: \(error)")
            }

            DispatchQueue.main.async {
                self.isRefreshing = false
                completion?(self.vms)
            }
        }
    }

    func startVM(name: String, completion: @escaping (Bool, String?) -> Void) {
        let vm = vms.first(where: { $0.name == name })
        let netMode = getVMNetMode(vm: vm, name: name)

        let launch = { [weak self] in
            self?.runCubectl(["start", name]) { success, output in
                self?.refreshVMs()
                completion(success, output)
            }
        }

        if netMode == "vmnet-proxy" && !ServiceManager.shared.isRunning {
            DispatchQueue.main.async {
                let alert = NSAlert()
                alert.messageText = "Network Service is Stopped"
                alert.informativeText = "VM '\(name)' uses Shared Proxy networking (vmnet-proxy), but the Cube network service is currently stopped. Would you like to start it?"
                alert.addButton(withTitle: "Start Service & Launch")
                alert.addButton(withTitle: "Launch Anyway")
                alert.addButton(withTitle: "Cancel")
                alert.alertStyle = .warning

                let response = alert.runModal()
                if response == .alertFirstButtonReturn {
                    ServiceManager.shared.startService { _, _ in
                        launch()
                    }
                } else if response == .alertSecondButtonReturn {
                    launch()
                } else {
                    completion(false, "Startup cancelled.")
                }
            }
        } else {
            launch()
        }
    }

    private func getVMNetMode(vm: VMInfo?, name: String) -> String {
        var configUrl: URL? = nil
        if let configPath = vm?.config, FileManager.default.fileExists(atPath: configPath) {
            configUrl = URL(fileURLWithPath: configPath)
        } else {
            let candidate1 = URL(fileURLWithPath: "\(vmsDirectory)/\(name)/\(name).json")
            if FileManager.default.fileExists(atPath: candidate1.path) {
                configUrl = candidate1
            } else {
                let home = NSHomeDirectory()
                let candidateCube = URL(fileURLWithPath: "\(home)/Cube/\(name)/\(name).json")
                if FileManager.default.fileExists(atPath: candidateCube.path) {
                    configUrl = candidateCube
                } else {
                    let candidateVMs = URL(fileURLWithPath: "\(home)/VMs/\(name)/\(name).json")
                    if FileManager.default.fileExists(atPath: candidateVMs.path) {
                        configUrl = candidateVMs
                    }
                }
            }
        }

        guard let url = configUrl,
              let data = try? Data(contentsOf: url),
              let json = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
              let net = json["network"] as? [String: Any],
              let mode = net["mode"] as? String else {
            return "vmnet-proxy"
        }
        return mode
    }

    func stopVM(name: String, completion: @escaping (Bool, String?) -> Void) {
        runCubectl(["stop", name]) { success, output in
            self.refreshVMs()
            completion(success, output)
        }
    }

    func deleteVM(name: String, completion: @escaping (Bool, String?) -> Void) {
        runCubectl(["delete", name, "--force"]) { success, output in
            self.refreshVMs()
            completion(success, output)
        }
    }

    func createVM(options: CreateVMOptions,
                  progressHandler: @escaping (String) -> Void,
                  completion: @escaping (Bool, String?) -> Void) {
        guard let bin = findCubectlBinary() else {
            completion(false, "Cannot find cubectl binary. Run 'make all' first.")
            return
        }

        var args = ["create", options.name]

        if let dest = options.destinationDir, !dest.isEmpty {
            args.extend(["--dest", dest])
        }

        if options.osVersion == "custom", let iso = options.customIsoPath, !iso.isEmpty {
            args.extend(["--iso", iso])
        } else {
            args.extend(["--version", options.osVersion])
        }

        if options.isUnattended {
            args.append("--unattended")
            args.extend(["--password", options.password])
        }

        args.extend(["--cpus", String(options.cpus)])
        args.extend(["--ram", String(options.ramMB)])
        args.extend(["--disk", "rootfs.img:\(options.diskSizeGB)G"])

        args.extend(["--net", options.netMode])

        if let ssh = options.sshPort {
            args.extend(["--ssh-port", String(ssh)])
        }
        if let vnc = options.vncPort {
            args.extend(["--vnc", "127.0.0.1:\(vnc)"])
        }

        if let sharePath = options.sharePath, !sharePath.isEmpty {
            let tag = (options.shareTag?.trimmingCharacters(in: .whitespaces).isEmpty == false) ? options.shareTag! : "shared"
            if options.shareReadOnly {
                args.extend(["--share", tag, sharePath])
            } else {
                args.extend(["--share-rw", tag, sharePath])
            }
        }

        if options.memPrealloc {
            args.append("--mem-prealloc")
        }

        DispatchQueue.global(qos: .userInitiated).async {
            let proc = Process()
            proc.executableURL = URL(fileURLWithPath: bin)
            proc.arguments = args

            let pipe = Pipe()
            proc.standardOutput = pipe
            proc.standardError = pipe

            pipe.fileHandleForReading.readabilityHandler = { handle in
                let chunk = handle.availableData
                if !chunk.isEmpty, let text = String(data: chunk, encoding: .utf8) {
                    DispatchQueue.main.async {
                        progressHandler(text)
                    }
                }
            }

            do {
                try proc.run()
                proc.waitUntilExit()
                pipe.fileHandleForReading.readabilityHandler = nil

                let success = (proc.terminationStatus == 0)
                DispatchQueue.main.async {
                    self.refreshVMs()
                    completion(success, success ? nil : "Process exited with code \(proc.terminationStatus)")
                }
            } catch {
                pipe.fileHandleForReading.readabilityHandler = nil
                DispatchQueue.main.async {
                    completion(false, error.localizedDescription)
                }
            }
        }
    }

    func updateShares(for vmName: String, shares: [VMShareConfig], completion: @escaping (Bool, String?) -> Void) {
        guard let vm = vms.first(where: { $0.name == vmName }) else {
            completion(false, "Virtual machine not found.")
            return
        }
        if vm.isRunning {
            completion(false, "Cannot modify shared folders while the VM is running. Stop the VM first.")
            return
        }
        let configPath = vm.config ?? "\(vmsDirectory)/\(vmName)/\(vmName).json"
        guard let data = try? Data(contentsOf: URL(fileURLWithPath: configPath)),
              var json = (try? JSONSerialization.jsonObject(with: data)) as? [String: Any] else {
            completion(false, "Failed to read VM configuration file.")
            return
        }

        let sharesDicts = shares.map { ["tag": $0.tag, "path": $0.path, "read_only": $0.read_only] }
        json["shares"] = sharesDicts

        do {
            let updatedData = try JSONSerialization.data(withJSONObject: json, options: [.prettyPrinted, .sortedKeys])
            try updatedData.write(to: URL(fileURLWithPath: configPath), options: .atomic)
            refreshVMs()
            completion(true, nil)
        } catch {
            completion(false, "Failed to write updated configuration: \(error.localizedDescription)")
        }
    }

    private func runCubectl(_ args: [String], completion: @escaping (Bool, String?) -> Void) {
        guard let bin = findCubectlBinary() else {
            completion(false, "cubectl not found")
            return
        }
        DispatchQueue.global(qos: .userInitiated).async {
            let proc = Process()
            proc.executableURL = URL(fileURLWithPath: bin)
            proc.arguments = args

            let pipe = Pipe()
            proc.standardOutput = pipe
            proc.standardError = pipe

            do {
                try proc.run()
                proc.waitUntilExit()

                let data = pipe.fileHandleForReading.readDataToEndOfFile()
                let output = String(data: data, encoding: .utf8)?.trimmingCharacters(in: .whitespacesAndNewlines)
                let success = proc.terminationStatus == 0
                DispatchQueue.main.async {
                    completion(success, output)
                }
            } catch {
                DispatchQueue.main.async {
                    completion(false, error.localizedDescription)
                }
            }
        }
    }

    func openTerminalSSH(for vm: VMInfo) {
        if !vm.isRunning {
            let alert = NSAlert()
            alert.messageText = "Virtual Machine Not Running"
            alert.informativeText = "Virtual Machine '\(vm.name)' is currently stopped. Would you like to start it now?"
            alert.alertStyle = .informational
            alert.addButton(withTitle: "Start and Connect")
            alert.addButton(withTitle: "Cancel")
            if alert.runModal() == .alertFirstButtonReturn {
                startVM(name: vm.name) { [weak self] success, err in
                    if success {
                        // Wait a brief moment for daemon socket setup before launching SSH terminal
                        DispatchQueue.main.asyncAfter(deadline: .now() + 1.0) {
                            self?.refreshVMs { updatedVMs in
                                if let running = updatedVMs.first(where: { $0.name == vm.name }) {
                                    self?.openTerminalSSH(for: running)
                                }
                            }
                        }
                    } else if let e = err {
                        let errAlert = NSAlert()
                        errAlert.messageText = "Failed to Start VM"
                        errAlert.informativeText = e
                        errAlert.alertStyle = .critical
                        errAlert.runModal()
                    }
                }
            }
            return
        }

        let cfg = vm.loadConfig()
        let port: Int
        let host: String

        if let p = vm.ssh_port ?? cfg.sshPort {
            port = p
            host = "127.0.0.1"
        } else if let ip = vm.ip, !ip.isEmpty {
            port = 22
            host = ip
        } else {
            // Default port forward fallback
            port = 2222
            host = "127.0.0.1"
        }

        let home = NSHomeDirectory()
        let appSupport = "\(home)/Library/Application Support/cube"
        try? FileManager.default.createDirectory(atPath: appSupport, withIntermediateDirectories: true)
        let scriptPath = "\(appSupport)/ssh-\(vm.name).command"
        let scriptContent = """
        #!/bin/bash
        clear
        echo "=================================================="
        echo "  Connecting to \(vm.name) via SSH"
        echo "  Target: root@\(host) (Port \(port))"
        echo "=================================================="
        echo ""

        ATTEMPT=1
        while true; do
            ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o GlobalKnownHostsFile=/dev/null -o ConnectTimeout=3 -p \(port) root@\(host)
            EXIT_CODE=$?
            if [ $EXIT_CODE -eq 0 ] || [ $EXIT_CODE -eq 130 ]; then
                break
            fi
            echo ""
            echo "[!] Connection failed (code $EXIT_CODE). VM may still be booting or initializing SSH."
            echo "    Retrying in 2 seconds (Attempt #$ATTEMPT, press Ctrl+C to cancel)..."
            echo ""
            ATTEMPT=$((ATTEMPT + 1))
            sleep 2
        done
        """
        try? scriptContent.write(toFile: scriptPath, atomically: true, encoding: .utf8)
        let permissions = [FileAttributeKey.posixPermissions: 0o755]
        try? FileManager.default.setAttributes(permissions, ofItemAtPath: scriptPath)

        let scriptUrl = URL(fileURLWithPath: scriptPath)
        let termAppUrl = resolveTerminalAppURL()
        let config = NSWorkspace.OpenConfiguration()
        NSWorkspace.shared.open([scriptUrl], withApplicationAt: termAppUrl, configuration: config) { _, error in
            if error != nil {
                let proc = Process()
                proc.executableURL = URL(fileURLWithPath: "/usr/bin/open")
                proc.arguments = ["-a", termAppUrl.path, scriptPath]
                do {
                    try proc.run()
                } catch {
                    // Fallback to default Terminal if custom terminal fails
                    let fallbackProc = Process()
                    fallbackProc.executableURL = URL(fileURLWithPath: "/usr/bin/open")
                    fallbackProc.arguments = ["-a", "Terminal", scriptPath]
                    try? fallbackProc.run()
                }
            }
        }
    }

    func openVNC(for vm: VMInfo) {
        if !vm.isRunning {
            let alert = NSAlert()
            alert.messageText = "Virtual Machine Not Running"
            alert.informativeText = "Virtual Machine '\(vm.name)' is currently stopped. Please start the VM first to open its display."
            alert.alertStyle = .informational
            alert.runModal()
            return
        }

        let cfg = vm.loadConfig()
        guard let vnc = vm.vnc ?? cfg.vnc, let url = URL(string: "vnc://\(vnc)") else {
            let alert = NSAlert()
            alert.messageText = "No VNC Display"
            alert.informativeText = "Virtual Machine '\(vm.name)' does not have a VNC display configured."
            alert.alertStyle = .informational
            alert.runModal()
            return
        }
        NSWorkspace.shared.open(url)
    }

    // MARK: - Smart Port Auto-Allocation

    func isPortAvailable(port: Int) -> Bool {
        guard port > 0 && port <= 65535 else { return false }
        let socketFD = socket(AF_INET, SOCK_STREAM, 0)
        if socketFD == -1 {
            return false
        }
        defer { close(socketFD) }

        var reuse: Int32 = 1
        setsockopt(socketFD, SOL_SOCKET, SO_REUSEADDR, &reuse, socklen_t(MemoryLayout<Int32>.size))

        var addr = sockaddr_in()
        addr.sin_len = UInt8(MemoryLayout<sockaddr_in>.size)
        addr.sin_family = sa_family_t(AF_INET)
        addr.sin_port = in_port_t(UInt16(port).bigEndian)
        addr.sin_addr.s_addr = inet_addr("127.0.0.1")

        let bindResult = withUnsafePointer(to: &addr) {
            $0.withMemoryRebound(to: sockaddr.self, capacity: 1) {
                bind(socketFD, $0, socklen_t(MemoryLayout<sockaddr_in>.size))
            }
        }
        return bindResult == 0
    }

    private func parseVNCPort(from string: String) -> Int? {
        let clean = string.trimmingCharacters(in: .whitespacesAndNewlines)
        if let colonIdx = clean.lastIndex(of: ":") {
            let portStr = String(clean[clean.index(after: colonIdx)...])
            if let val = Int(portStr) {
                return val < 100 ? (5900 + val) : val
            }
        } else if let val = Int(clean) {
            return val < 100 ? (5900 + val) : val
        }
        return nil
    }

    func collectAllocatedPorts() -> (ssh: Set<Int>, vnc: Set<Int>) {
        var usedSSH = Set<Int>()
        var usedVNC = Set<Int>()

        for vm in vms {
            let cfg = vm.loadConfig()
            if let p = vm.ssh_port ?? cfg.sshPort {
                usedSSH.insert(p)
            }
            if let vncStr = vm.vnc ?? cfg.vnc, let p = parseVNCPort(from: vncStr) {
                usedVNC.insert(p)
            }
        }

        let vmsDir = URL(fileURLWithPath: vmsDirectory)
        if let entries = try? FileManager.default.contentsOfDirectory(at: vmsDir, includingPropertiesForKeys: nil) {
            for entry in entries {
                let jsonFile = entry.appendingPathComponent("\(entry.lastPathComponent).json")
                if FileManager.default.fileExists(atPath: jsonFile.path),
                   let data = try? Data(contentsOf: jsonFile),
                   let json = try? JSONSerialization.jsonObject(with: data) as? [String: Any] {
                    if let net = json["network"] as? [String: Any], let sp = net["ssh_port"] as? Int {
                        usedSSH.insert(sp)
                    }
                    if let vnc = json["vnc"] as? [String: Any], let bind = vnc["bind"] as? String, let p = parseVNCPort(from: bind) {
                        usedVNC.insert(p)
                    }
                }
            }
        }

        return (usedSSH, usedVNC)
    }

    func nextAvailableSSHPort(startingAt: Int = 2222) -> Int {
        let (usedSSH, _) = collectAllocatedPorts()
        var port = max(1, startingAt)
        let maxProbe = min(65535, port + 500)
        while port <= maxProbe {
            if !usedSSH.contains(port) && isPortAvailable(port: port) {
                return port
            }
            port += 1
        }
        // Fallback: pick the first port not configured in any VM
        var fallback = max(1, startingAt)
        while fallback <= 65535 {
            if !usedSSH.contains(fallback) {
                return fallback
            }
            fallback += 1
        }
        return startingAt
    }

    func nextAvailableVNCPort(startingAt: Int = 5901) -> Int {
        let (_, usedVNC) = collectAllocatedPorts()
        var port = max(1, startingAt)
        let maxProbe = min(65535, port + 500)
        while port <= maxProbe {
            if !usedVNC.contains(port) && isPortAvailable(port: port) {
                return port
            }
            port += 1
        }
        // Fallback: pick the first port not configured in any VM
        var fallback = max(1, startingAt)
        while fallback <= 65535 {
            if !usedVNC.contains(fallback) {
                return fallback
            }
            fallback += 1
        }
        return startingAt
    }
}

struct DetailedVMConfig {
    var os: String?
    var cpus: Int = 2
    var ramMB: Int = 2048
    var bootMode: String = "uefi"
    var diskPath: String?
    var netMode: String = "vmnet-proxy"
    var sshPort: Int?
    var vnc: String?
    var shares: [VMShareConfig] = []
    var memPrealloc: Bool = false
}

extension VMInfo {
    func loadConfig() -> DetailedVMConfig {
        var cfg = DetailedVMConfig()
        let configPath = self.config ?? "\(VMManager.shared.vmsDirectory)/\(self.name)/\(self.name).json"
        if let data = try? Data(contentsOf: URL(fileURLWithPath: configPath)),
           let json = try? JSONSerialization.jsonObject(with: data) as? [String: Any] {
            if let o = json["os"] as? String { cfg.os = o }
            if let c = json["cpus"] as? Int { cfg.cpus = c }
            if let r = json["ram_mb"] as? Int { cfg.ramMB = r }
            if let b = json["boot"] as? [String: Any], let m = b["mode"] as? String { cfg.bootMode = m }
            if let d = json["disks"] as? [[String: Any]], let first = d.first, let p = first["path"] as? String {
                cfg.diskPath = p
            }
            if let n = json["network"] as? [String: Any] {
                if let m = n["mode"] as? String { cfg.netMode = m }
                if let sp = n["ssh_port"] as? Int { cfg.sshPort = sp }
            }
            if let v = json["vnc"] as? [String: Any], let bind = v["bind"] as? String {
                cfg.vnc = bind
            }
            if let sharesList = json["shares"] as? [[String: Any]] {
                cfg.shares = sharesList.compactMap { s in
                    guard let tag = s["tag"] as? String, let p = s["path"] as? String else { return nil }
                    let ro = s["read_only"] as? Bool ?? false
                    return VMShareConfig(tag: tag, path: p, read_only: ro)
                }
            }
            if let mp = json["mem_prealloc"] as? Bool {
                cfg.memPrealloc = mp
            }
        }
        return cfg
    }
}

private extension Array {
    mutating func extend(_ elements: [Element]) {
        self.append(contentsOf: elements)
    }
}
