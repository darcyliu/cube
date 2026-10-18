import Foundation
import AppKit

class MenuController: NSObject {
    private var statusItem: NSStatusItem!
    private var statusMenu: NSMenu!
    private var updateTimer: Timer?
    
    private var statusMenuItem: NSMenuItem!
    private var socketMenuItem: NSMenuItem!
    private var hostIPMenuItem: NSMenuItem!
    private var sharedModeMenuItem: NSMenuItem!
    private var customSubnetMenuItem: NSMenuItem!
    private var bridgeModeMenuItem: NSMenuItem!
    private var actionMenuItem: NSMenuItem!
    private var currentHostIP: String? = nil

    
    override init() {
        super.init()
        setupStatusItem()
        setupMenu()
        startMonitoring()
    }
    
    deinit {
        updateTimer?.invalidate()
    }
    
    private func setupStatusItem() {
        statusItem = NSStatusBar.system.statusItem(withLength: NSStatusItem.variableLength)
        updateStatusIcon()
    }
    
    private func updateStatusIcon() {
        guard let button = statusItem.button else { return }
        
        let isRunning = ServiceManager.shared.isRunning
        let config = NSImage.SymbolConfiguration(pointSize: 14, weight: .semibold)
        
        // Use standard SF Symbols
        var iconName = "network.slash"
        if isRunning {
            switch ServiceManager.shared.activeMode ?? ServiceManager.shared.currentMode {
            case .shared:
                iconName = "network"
            case .bridged:
                iconName = "point.3.connected.trianglepath.dotted"
            }
        }
        
        if let image = NSImage(systemSymbolName: iconName, accessibilityDescription: "Cube")?.withSymbolConfiguration(config) {
            image.isTemplate = true
            button.image = image
            button.title = ""
        } else {
            // Fallback text icon
            button.title = isRunning ? "🧊●" : "🧊○"
        }
    }
    
    private func setupMenu() {
        statusMenu = NSMenu()
        
        // 1. Header & Title
        let titleItem = NSMenuItem(title: "Cube", action: nil, keyEquivalent: "")
        titleItem.attributedTitle = NSAttributedString(
            string: "Cube \(AppVersion.displayString)",
            attributes: [.font: NSFont.boldSystemFont(ofSize: 13)]
        )
        statusMenu.addItem(titleItem)
        
        // 2. Status Label
        statusMenuItem = NSMenuItem(title: "Status: Checking...", action: nil, keyEquivalent: "")
        statusMenuItem.target = nil
        statusMenuItem.action = nil
        statusMenu.addItem(statusMenuItem)
        
        // 3. Socket Path Label
        socketMenuItem = NSMenuItem(title: "Socket: Checking...", action: #selector(copySocketPath), keyEquivalent: "")
        socketMenuItem.target = self
        statusMenu.addItem(socketMenuItem)
        
        // 4. Host IP Label
        hostIPMenuItem = NSMenuItem(title: "Host IP: Checking...", action: #selector(copyHostIP), keyEquivalent: "")
        hostIPMenuItem.target = self
        statusMenu.addItem(hostIPMenuItem)
        
        statusMenu.addItem(NSMenuItem.separator())
        
        // 4. Mode Selection Header
        let modeHeader = NSMenuItem(title: "Operation Mode", action: nil, keyEquivalent: "")
        modeHeader.isEnabled = false
        statusMenu.addItem(modeHeader)
        
        sharedModeMenuItem = NSMenuItem(title: "Shared NAT (Apple Default)", action: #selector(selectSharedMode), keyEquivalent: "1")
        sharedModeMenuItem.target = self
        statusMenu.addItem(sharedModeMenuItem)
        
        customSubnetMenuItem = NSMenuItem(title: "Shared NAT (Custom Subnet: 192.168.18.0/24)", action: #selector(selectCustomSubnetMode), keyEquivalent: "")
        customSubnetMenuItem.target = self
        statusMenu.addItem(customSubnetMenuItem)
        
        bridgeModeMenuItem = NSMenuItem(title: "Bridged LAN (Host Interface: en0)", action: #selector(selectBridgeMode), keyEquivalent: "2")
        bridgeModeMenuItem.target = self
        statusMenu.addItem(bridgeModeMenuItem)
        
        statusMenu.addItem(NSMenuItem.separator())
        
        // 5. Service Action (Start / Stop)
        actionMenuItem = NSMenuItem(title: "Start Service", action: #selector(toggleService), keyEquivalent: "s")
        actionMenuItem.target = self
        statusMenu.addItem(actionMenuItem)
        
        let restartItem = NSMenuItem(title: "Restart Service", action: #selector(restartService), keyEquivalent: "r")
        restartItem.target = self
        statusMenu.addItem(restartItem)
        
        statusMenu.addItem(NSMenuItem.separator())
        
        // 7. Utility Actions
        let copyItem = NSMenuItem(title: "Copy Socket Path to Clipboard", action: #selector(copySocketPath), keyEquivalent: "c")
        copyItem.target = self
        statusMenu.addItem(copyItem)
        
        let logItem = NSMenuItem(title: "Open Service Log...", action: #selector(openLogs), keyEquivalent: "l")
        logItem.target = self
        statusMenu.addItem(logItem)
        
        let termItem = NSMenuItem(title: "Open Terminal (Cube Environment)", action: #selector(openTerminalInProject), keyEquivalent: "t")
        termItem.target = self
        statusMenu.addItem(termItem)
        
        let aboutItem = NSMenuItem(title: "About Cube...", action: #selector(showAboutWindow), keyEquivalent: "")
        aboutItem.target = self
        statusMenu.addItem(aboutItem)
        
        let prefsItem = NSMenuItem(title: "Preferences...", action: #selector(openPreferences), keyEquivalent: ",")
        prefsItem.target = self
        statusMenu.addItem(prefsItem)
        
        statusMenu.addItem(NSMenuItem.separator())
        
        // 8. Quit
        let quitItem = NSMenuItem(title: "Quit Cube", action: #selector(quitApp), keyEquivalent: "q")
        quitItem.target = self
        statusMenu.addItem(quitItem)
        
        statusItem.menu = statusMenu
        updateMenuState()
    }
    
    private func startMonitoring() {
        updateTimer = Timer.scheduledTimer(withTimeInterval: 2.0, repeats: true) { [weak self] _ in
            ServiceManager.shared.checkStatus()
            self?.updateMenuState()
            self?.updateStatusIcon()
        }
    }
    
    private func updateMenuState() {
        let isRunning = ServiceManager.shared.isRunning
        let mode = ServiceManager.shared.currentMode
        
        if isRunning {
            let attr = NSMutableAttributedString(string: "● ", attributes: [
                .foregroundColor: NSColor.systemGreen,
                .font: NSFont.boldSystemFont(ofSize: 13)
            ])
            attr.append(NSAttributedString(string: "Active (\((ServiceManager.shared.activeMode ?? mode).description))", attributes: [
                .foregroundColor: NSColor.labelColor,
                .font: NSFont.systemFont(ofSize: 13)
            ]))
            statusMenuItem.attributedTitle = attr
            actionMenuItem.title = "Stop Service"
        } else {
            let attr = NSMutableAttributedString(string: "○ ", attributes: [
                .foregroundColor: NSColor.systemGray,
                .font: NSFont.systemFont(ofSize: 13)
            ])
            attr.append(NSAttributedString(string: "Stopped", attributes: [
                .foregroundColor: NSColor.secondaryLabelColor,
                .font: NSFont.systemFont(ofSize: 13)
            ]))
            statusMenuItem.attributedTitle = attr
            actionMenuItem.title = "Start Service"
        }
        
        let sock = ServiceManager.shared.defaultSocketPath
        let shortPath = sock.replacingOccurrences(of: NSHomeDirectory(), with: "~")
        socketMenuItem.title = "Socket: \(shortPath)"
        
        let hostInfo = detectHostNetworkInfo()
        if let ip4 = hostInfo.ipv4 {
            currentHostIP = ip4
            hostIPMenuItem.title = "Host IP: \(ip4) (\(hostInfo.interface))"
            hostIPMenuItem.isEnabled = true
        } else {
            currentHostIP = nil
            hostIPMenuItem.title = "Host IP: Not Connected"
            hostIPMenuItem.isEnabled = false
        }
        
        switch mode {
        case .shared(let subnet):
            if let subnet = subnet, !subnet.isEmpty {
                sharedModeMenuItem.state = .off
                customSubnetMenuItem.state = .on
            } else {
                sharedModeMenuItem.state = .on
                customSubnetMenuItem.state = .off
            }
            bridgeModeMenuItem.state = .off
        case .bridged:
            sharedModeMenuItem.state = .off
            customSubnetMenuItem.state = .off
            bridgeModeMenuItem.state = .on
        }
        

    }
    
    @objc private func openPreferences() {
        PreferencesWindowController.shared.showWindow(nil)
    }
    
    @objc private func selectSharedMode() {
        ServiceManager.shared.saveMode(.shared(subnet: nil))
        updateMenuState()
        if ServiceManager.shared.isRunning {
            restartService()
        }
    }
    
    @objc private func selectCustomSubnetMode() {
        showAlert(title: "Custom Subnets Require Privileged Setup",
                  message: "Select Apple Default for unprivileged networking. Use the separately administered cube CLI for custom subnets.")
    }
    
    @objc private func selectBridgeMode() {
        ServiceManager.shared.saveMode(.bridged(iface: ""))
        updateMenuState()
        if ServiceManager.shared.isRunning {
            restartService()
        }
    }
    
    @objc private func toggleService() {
        if ServiceManager.shared.isRunning {
            actionMenuItem.title = "Stopping..."
            ServiceManager.shared.stopService { [weak self] success, error in
                self?.updateMenuState()
                self?.updateStatusIcon()
                if let error = error {
                    self?.showAlert(title: "Error Stopping Service", message: error)
                }
            }
        } else {
            actionMenuItem.title = "Starting..."
            ServiceManager.shared.startService { [weak self] success, error in
                self?.updateMenuState()
                self?.updateStatusIcon()
                if !success, let error = error {
                    self?.showAlert(title: "Error Starting Service", message: error)
                }
            }
        }
    }
    
    @objc private func restartService() {
        ServiceManager.shared.restartService { [weak self] success, error in
            self?.updateMenuState()
            self?.updateStatusIcon()
            if !success, let error = error {
                self?.showAlert(title: "Error Restarting Service", message: error)
            }
        }
    }

    @objc private func copySocketPath() {
        let pasteboard = NSPasteboard.general
        pasteboard.clearContents()
        pasteboard.setString(ServiceManager.shared.defaultSocketPath, forType: .string)
    }
    
    @objc private func copyHostIP() {
        guard let ip = currentHostIP, !ip.isEmpty else { return }
        let pasteboard = NSPasteboard.general
        pasteboard.clearContents()
        pasteboard.setString(ip, forType: .string)
    }
    
    @objc private func openLogs() {
        let logPath = ServiceManager.shared.logPath
        if !FileManager.default.fileExists(atPath: logPath) {
            try? "".write(toFile: logPath, atomically: true, encoding: .utf8)
        }
        NSWorkspace.shared.open(URL(fileURLWithPath: logPath))
    }
    
    @objc private func openTerminalInProject() {
        let home = NSHomeDirectory()
        let projectDir = "\(home)/Dev/vmm"
        
        // Ensure ~/.local/bin/cube symlink exists
        // Prefer the Apple Developer-signed cube inside Cube.app helper bundle, fallback to repo target/debug/cube
        let binDir = "\(home)/.local/bin"
        let symlinkPath = "\(binDir)/cube"
        let helperCube = Bundle.main.bundleURL.appendingPathComponent("Contents/Helpers/cube.app/Contents/MacOS/cube").path
        let debugCube = "\(projectDir)/target/debug/cube"
        let targetPath = FileManager.default.fileExists(atPath: helperCube) ? helperCube : debugCube
        
        if FileManager.default.fileExists(atPath: targetPath) {
            try? FileManager.default.createDirectory(atPath: binDir, withIntermediateDirectories: true)
            try? FileManager.default.removeItem(atPath: symlinkPath)
            try? FileManager.default.createSymbolicLink(atPath: symlinkPath, withDestinationPath: targetPath)
        }
        
        let appSupport = "\(home)/Library/Application Support/cube"
        try? FileManager.default.createDirectory(atPath: appSupport, withIntermediateDirectories: true)
        let scriptPath = "\(appSupport)/open-terminal.command"
        
        let scriptContent = """
        #!/bin/bash
        cd "\(projectDir)"
        export PATH="$HOME/.local/bin:$HOME/.cargo/bin:$PATH"
        clear
        echo "=================================================="
        echo "  Cube Development Environment"
        echo "  Working Directory: $(pwd)"
        echo "  Cube Binary:       $(which cube 2>/dev/null || echo 'not found')"
        echo "  vmnet Socket:      \\(ServiceManager.shared.defaultSocketPath)"
        echo "=================================================="
        echo ""
        echo "The 'cube' command is ready. Try: cube"
        echo ""
        exec "${SHELL:-/bin/zsh}" -l
        """
        
        try? scriptContent.write(toFile: scriptPath, atomically: true, encoding: .utf8)
        let permissions = [FileAttributeKey.posixPermissions: 0o755]
        try? FileManager.default.setAttributes(permissions, ofItemAtPath: scriptPath)
        
        NSWorkspace.shared.open(URL(fileURLWithPath: scriptPath))
    }
    
    @objc private func showAboutWindow() {
        let options: [NSApplication.AboutPanelOptionKey: Any] = [
            .applicationVersion: "Build \(AppVersion.build)",
            .version: AppVersion.version,
            .applicationName: "Cube"
        ]
        NSApp.orderFrontStandardAboutPanel(options: options)
        NSApp.activate(ignoringOtherApps: true)
    }
    
    @objc private func quitApp() {
        NSApplication.shared.terminate(nil)
    }
    
    private func showAlert(title: String, message: String) {
        let alert = NSAlert()
        alert.messageText = title
        alert.informativeText = message
        alert.alertStyle = .warning
        alert.addButton(withTitle: "OK")
        alert.runModal()
    }
}
