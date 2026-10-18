import Foundation
import Darwin

@main
struct ServiceManagerTests {
    static func wait(_ operation: (@escaping (Bool, String?) -> Void) -> Void) -> (Bool, String?) {
        var result: (Bool, String?)?
        operation { result = ($0, $1) }
        let deadline = Date().addingTimeInterval(10)
        while result == nil && Date() < deadline {
            RunLoop.main.run(until: Date().addingTimeInterval(0.02))
        }
        precondition(result != nil, "Operation never completed")
        return result!
    }

    static func main() throws {
        let dir = URL(fileURLWithPath: NSTemporaryDirectory()).appendingPathComponent("cube-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: dir) }
        // A short socket path is necessary on macOS (sun_path has 104 bytes).
        let socket = "/tmp/cube-\(getpid()).sock"
        defer { try? FileManager.default.removeItem(atPath: socket) }
        let helper = dir.appendingPathComponent("helper with spaces.py")
        try """
        #!/usr/bin/python3
        import os, signal, socket, time
        time.sleep(float(os.environ.get('CUBE_TEST_DELAY', '0')))
        s = socket.socket(socket.AF_UNIX)
        s.bind(os.environ['CUBE_TEST_SOCKET'])
        s.listen(16)
        def stop(*args):
            if os.environ.get('CUBE_TEST_IGNORE_STOP') == '1':
                return
            s.close()
            os.unlink(os.environ['CUBE_TEST_SOCKET'])
            raise SystemExit(0)
        signal.signal(signal.SIGTERM, stop)
        while True:
            c, _ = s.accept()
            c.close()
        """.write(to: helper, atomically: true, encoding: .utf8)
        try FileManager.default.setAttributes([.posixPermissions: 0o755], ofItemAtPath: helper.path)
        setenv("CUBE_TEST_SOCKET", socket, 1)
        let manager = ServiceManager(socketPath: socket, logPath: dir.appendingPathComponent("log").path,
                                     helperPath: helper.path, startupTimeout: 5, stopTimeout: 0.2)
        manager.currentMode = .shared(subnet: nil)
        setenv("CUBE_TEST_DELAY", "2.2", 1)
        precondition(wait(manager.startService).0, "Slow startup should succeed without elevation")
        let pid = manager.servicePid!
        precondition(ServiceManager.socketPeer(at: socket) == pid)
        let other = ServiceManager(socketPath: socket, logPath: dir.appendingPathComponent("other.log").path,
                                   helperPath: helper.path)
        other.currentMode = .shared(subnet: nil)
        precondition(!wait(other.startService).0, "Do not adopt another service")
        precondition(wait(other.stopService).0)
        precondition(kill(pid, 0) == 0, "Stopping an unrelated manager must not kill the child")
        precondition(wait(manager.stopService).0)
        precondition(!FileManager.default.fileExists(atPath: socket))

        setenv("CUBE_TEST_DELAY", "0", 1)
        setenv("CUBE_TEST_IGNORE_STOP", "1", 1)
        precondition(wait(manager.startService).0)
        let stubbornPID = manager.servicePid!
        precondition(!wait(manager.stopService).0, "A live child must not be reported stopped")
        precondition(FileManager.default.fileExists(atPath: socket), "Stop must not unlink a live socket")
        precondition(!wait(manager.restartService).0, "Failed stop must prevent replacement")
        precondition(manager.servicePid == stubbornPID)
        kill(stubbornPID, SIGKILL) // Only our fake test child.
        while kill(stubbornPID, 0) == 0 { RunLoop.main.run(until: Date().addingTimeInterval(0.02)) }
        _ = wait(manager.stopService)
        try FileManager.default.removeItem(atPath: socket)
        unsetenv("CUBE_TEST_IGNORE_STOP")

        let timeout = ServiceManager(socketPath: socket, logPath: dir.appendingPathComponent("timeout.log").path,
                                    helperPath: helper.path, startupTimeout: 0.1, stopTimeout: 1)
        timeout.currentMode = .shared(subnet: nil)
        setenv("CUBE_TEST_DELAY", "2", 1)
        precondition(!wait(timeout.startService).0)
        precondition(timeout.servicePid == nil, "Timed-out startup must reap its child")
        timeout.currentMode = .shared(subnet: "10.0.0.0/24")
        precondition(!wait(timeout.startService).0)
        print("Cube lifecycle checks passed")
    }
}
