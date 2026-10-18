import Foundation
import AppKit
import ServiceManagement
import Darwin

enum ServiceMode: Equatable {
    case shared(subnet: String?)
    case bridged(iface: String)
    
    var description: String {
        switch self {
        case .shared(let subnet):
            if let subnet = subnet, !subnet.isEmpty {
                return "Shared NAT (\(subnet))"
            } else {
                return "Shared NAT (Apple Default)"
            }
        case .bridged(let iface):
            return "Bridged LAN (\(iface))"
        }
    }
}

class ServiceManager {
    static let shared = ServiceManager()
    
    var currentMode: ServiceMode = .shared(subnet: nil)
    private(set) var activeMode: ServiceMode?
    private(set) var isRunning: Bool = false
    private(set) var servicePid: Int32? = nil
    
    let defaultSocketPath: String
    let logPath: String
    private let helperPath: String?
    private let startupTimeout: TimeInterval
    private let stopTimeout: TimeInterval

    private var process: Process?
    private(set) var isBusy = false
    private let daemonPlistPath = "/Library/LaunchDaemons/net.neuralengine.cube.vmnet.plist"
    private let prefsModeKey = "CubeMode"
    private let prefsSubnetKey = "CubeCustomSubnet"
    private let prefsBridgeIfaceKey = "CubeBridgeInterface"
    private let prefsAutoStartKey = "CubeAutoStartService"
    
    var autoStartService: Bool {
        get {
            if UserDefaults.standard.object(forKey: prefsAutoStartKey) == nil {
                return true
            }
            return UserDefaults.standard.bool(forKey: prefsAutoStartKey)
        }
        set {
            UserDefaults.standard.set(newValue, forKey: prefsAutoStartKey)
        }
    }
    
    var isLaunchAtLoginEnabled: Bool {
        if #available(macOS 13.0, *) {
            return SMAppService.mainApp.status == .enabled
        }
        return false
    }
    
    func setLaunchAtLogin(enabled: Bool) throws {
        if #available(macOS 13.0, *) {
            if enabled {
                if SMAppService.mainApp.status != .enabled {
                    try SMAppService.mainApp.register()
                }
            } else {
                if SMAppService.mainApp.status == .enabled {
                    try SMAppService.mainApp.unregister()
                }
            }
        }
    }

    
    init(socketPath: String? = nil, logPath: String? = nil, helperPath: String? = nil,
         startupTimeout: TimeInterval = 30, stopTimeout: TimeInterval = 5) {
        self.defaultSocketPath = socketPath ?? "\(NSHomeDirectory())/Library/Application Support/cube/vmnet.sock"
        self.logPath = logPath ?? "\(NSHomeDirectory())/Library/Application Support/cube/vmnet-service.log"
        self.helperPath = helperPath
        self.startupTimeout = startupTimeout
        self.stopTimeout = stopTimeout
        self.currentMode = loadSavedMode()
        checkStatus()
    }
    
    func loadSavedMode() -> ServiceMode {
        let modeType = UserDefaults.standard.string(forKey: prefsModeKey) ?? "sharedDefault"
        switch modeType {
        case "sharedCustom":
            let subnet = UserDefaults.standard.string(forKey: prefsSubnetKey) ?? "192.168.18.0/24"
            return .shared(subnet: subnet)
        case "bridged":
            let iface = UserDefaults.standard.string(forKey: prefsBridgeIfaceKey) ?? ""
            return .bridged(iface: iface)
        default:
            return .shared(subnet: nil)
        }
    }
    
    func saveMode(_ mode: ServiceMode) {
        self.currentMode = mode
        switch mode {
        case .shared(let subnet):
            if let subnet = subnet, !subnet.isEmpty {
                UserDefaults.standard.set("sharedCustom", forKey: prefsModeKey)
                UserDefaults.standard.set(subnet, forKey: prefsSubnetKey)
            } else {
                UserDefaults.standard.set("sharedDefault", forKey: prefsModeKey)
            }
        case .bridged(let iface):
            UserDefaults.standard.set("bridged", forKey: prefsModeKey)
            UserDefaults.standard.set(iface, forKey: prefsBridgeIfaceKey)
        }
    }
    
    func findCubeBinary() -> String? {
        let path = helperPath ?? Bundle.main.bundleURL
            .appendingPathComponent("Contents/Helpers/cube.app/Contents/MacOS/cube").path
        return FileManager.default.isExecutableFile(atPath: path) ? path : nil
    }

    // Only an owned child may be stopped. Connecting to the socket also verifies
    // that it belongs to that child, rather than trusting a stale directory entry.
    static func socketPeer(at path: String) -> pid_t? {
        var address = sockaddr_un()
        let bytes = Array(path.utf8) + [0]
        guard bytes.count <= MemoryLayout.size(ofValue: address.sun_path) else { return nil }
        address.sun_family = sa_family_t(AF_UNIX)
        address.sun_len = UInt8(MemoryLayout<sockaddr_un>.size)
        withUnsafeMutableBytes(of: &address.sun_path) { buffer in
            buffer.copyBytes(from: bytes)
        }
        let fd = socket(AF_UNIX, SOCK_STREAM, 0)
        guard fd >= 0 else { return nil }
        defer { close(fd) }
        // Never block the menu's main thread on a full listen backlog.
        guard fcntl(fd, F_SETFL, O_NONBLOCK) == 0 else { return nil }
        let connected = withUnsafePointer(to: &address) {
            $0.withMemoryRebound(to: sockaddr.self, capacity: 1) {
                connect(fd, $0, socklen_t(MemoryLayout<sockaddr_un>.size))
            }
        }
        guard connected == 0 else { return nil }
        var uid: uid_t = 0
        var gid: gid_t = 0
        guard getpeereid(fd, &uid, &gid) == 0, uid == getuid() else { return nil }
        var pid: pid_t = 0
        var size = socklen_t(MemoryLayout<pid_t>.size)
        guard getsockopt(fd, SOL_LOCAL, LOCAL_PEERPID, &pid, &size) == 0 else { return nil }
        return pid
    }

    func checkStatus() {
        dispatchPrecondition(condition: .onQueue(.main))
        guard let proc = process, proc.isRunning else {
            isRunning = false
            activeMode = nil
            servicePid = nil
            return
        }
        servicePid = proc.processIdentifier
        isRunning = Self.socketPeer(at: defaultSocketPath) == proc.processIdentifier
    }

    func startService(completion: @escaping (Bool, String?) -> Void) {
        dispatchPrecondition(condition: .onQueue(.main))
        guard !isBusy else { completion(false, "A service operation is already in progress."); return }
        guard !FileManager.default.fileExists(atPath: daemonPlistPath) else {
            completion(false, "A legacy root daemon is installed. Remove it before starting Cube.")
            return
        }
        if let proc = process, proc.isRunning {
            checkStatus()
            completion(isRunning, isRunning ? nil : "The existing service has not become ready. Stop it before retrying.")
            return
        }
        guard Self.socketPeer(at: defaultSocketPath) == nil else {
            completion(false, "Another service owns the socket. Stop it using the tool that started it.")
            return
        }
        guard let cubePath = findCubeBinary() else {
            completion(false, "The bundled cube helper is missing. Rebuild Cube.")
            return
        }
        var args = ["--vmnet-service"]
        switch currentMode {
        case .shared(let subnet):
            if let subnet = subnet, !subnet.isEmpty {
                completion(false, "Custom subnets require privileged PF configuration. Select Apple Default for unprivileged networking.")
                return
            }
        case .bridged(let iface):
            args.append("--bridge")
            if !iface.isEmpty { args.append(iface) }
        }
        do {
            let dir = (logPath as NSString).deletingLastPathComponent
            try FileManager.default.createDirectory(atPath: dir, withIntermediateDirectories: true)
            if !FileManager.default.fileExists(atPath: logPath) {
                guard FileManager.default.createFile(atPath: logPath, contents: nil) else {
                    throw CocoaError(.fileWriteUnknown)
                }
            }
            let log = try FileHandle(forWritingTo: URL(fileURLWithPath: logPath))
            defer { try? log.close() }
            try log.seekToEnd()
            let proc = Process()
            proc.executableURL = URL(fileURLWithPath: cubePath)
            proc.arguments = args
            proc.standardOutput = log
            proc.standardError = log
            try proc.run()
            process = proc
            activeMode = currentMode
            isBusy = true
            waitForReady(proc, deadline: Date().addingTimeInterval(startupTimeout), completion: completion)
        } catch {
            completion(false, "Could not start the bundled helper: \(error.localizedDescription)")
        }
    }

    private func waitForReady(_ proc: Process, deadline: Date,
                              completion: @escaping (Bool, String?) -> Void) {
        checkStatus()
        if isRunning {
            isBusy = false
            completion(true, nil)
        } else if !proc.isRunning {
            isBusy = false
            process = nil
            completion(false, "The helper exited (status \(proc.terminationStatus)). Sign Cube with an Apple-granted VM Networking profile and inspect the service log for details.")
        } else if Date() >= deadline {
            // Retain ownership even if termination times out. Never launch a second child.
            proc.terminate()
            waitForExit(proc, deadline: Date().addingTimeInterval(stopTimeout)) { _, error in
                completion(false, error ?? "Service startup timed out; the helper was stopped. See the service log.")
            }
        } else {
            DispatchQueue.main.asyncAfter(deadline: .now() + 0.1) {
                self.waitForReady(proc, deadline: deadline, completion: completion)
            }
        }
    }

    func stopService(completion: @escaping (Bool, String?) -> Void) {
        dispatchPrecondition(condition: .onQueue(.main))
        guard !isBusy else { completion(false, "A service operation is already in progress."); return }
        guard let proc = process, proc.isRunning else {
            process = nil
            checkStatus()
            completion(true, nil)
            return
        }
        isBusy = true
        proc.terminate()
        waitForExit(proc, deadline: Date().addingTimeInterval(stopTimeout), completion: completion)
    }

    private func waitForExit(_ proc: Process, deadline: Date,
                             completion: @escaping (Bool, String?) -> Void) {
        if !proc.isRunning {
            process = nil
            isBusy = false
            checkStatus()
            completion(true, nil)
        } else if Date() >= deadline {
            isBusy = false
            checkStatus()
            completion(false, "The service has not exited. It remains owned by Cube; retry Stop. Its socket was left intact.")
        } else {
            DispatchQueue.main.asyncAfter(deadline: .now() + 0.1) {
                self.waitForExit(proc, deadline: deadline, completion: completion)
            }
        }
    }

    func restartService(completion: @escaping (Bool, String?) -> Void) {
        stopService { success, error in
            guard success else { completion(false, error); return }
            self.startService(completion: completion)
        }
    }
}
