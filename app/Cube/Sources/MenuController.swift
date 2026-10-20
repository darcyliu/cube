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

    private var vmHeaderMenuItem: NSMenuItem!
    private var vmMenuItems: [NSMenuItem] = []
    private var newVMMenuItem: NSMenuItem!

    
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
        
        // 6. Virtual Machines Section
        let vmHeader = NSMenuItem(title: "Virtual Machines", action: nil, keyEquivalent: "")
        vmHeader.isEnabled = false
        statusMenu.addItem(vmHeader)
        vmHeaderMenuItem = vmHeader

        let dashboardItem = NSMenuItem(title: "Open Dashboard...", action: #selector(openDashboard), keyEquivalent: "d")
        dashboardItem.target = self
        statusMenu.addItem(dashboardItem)

        newVMMenuItem = NSMenuItem(title: "+ New Virtual Machine...", action: #selector(openNewVMWindow), keyEquivalent: "n")
        newVMMenuItem.target = self
        statusMenu.addItem(newVMMenuItem)

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
            VMManager.shared.refreshVMs { [weak self] _ in
                self?.updateVMMenu()
            }
            self?.updateMenuState()
            self?.updateStatusIcon()
        }
        // Initial VM load
        VMManager.shared.refreshVMs { [weak self] _ in
            self?.updateVMMenu()
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
        let binDir = "\(home)/.local/bin"
        try? FileManager.default.createDirectory(atPath: binDir, withIntermediateDirectories: true)
        
        // 1. Always prefer the signed binary inside the app bundle
        let symlinkPath = "\(binDir)/cube"
        let devCube = Bundle.main.bundleURL.deletingLastPathComponent().deletingLastPathComponent()
            .appendingPathComponent("target/debug/cube").path
        let cubeTarget = ServiceManager.shared.findCubeBinary() ??
            (FileManager.default.isExecutableFile(atPath: devCube) ? devCube : nil)
        
        if let target = cubeTarget {
            try? FileManager.default.removeItem(atPath: symlinkPath)
            try? FileManager.default.createSymbolicLink(atPath: symlinkPath, withDestinationPath: target)
        }

        // 2. Always prefer the bundled cubectl binary
        let cubectlSymlink = "\(binDir)/cubectl"
        if let cubectlTarget = VMManager.shared.findCubectlBinary() {
            try? FileManager.default.removeItem(atPath: cubectlSymlink)
            try? FileManager.default.createSymbolicLink(atPath: cubectlSymlink, withDestinationPath: cubectlTarget)
        }
        
        let appSupport = "\(home)/Library/Application Support/cube"
        try? FileManager.default.createDirectory(atPath: appSupport, withIntermediateDirectories: true)
        let scriptPath = "\(appSupport)/open-terminal.command"
        
        // 3. Resolve working directory: prefer ~/Cube (or configured vmsDirectory), fallback to $HOME
        let vmsDir = VMManager.shared.vmsDirectory
        let workingDir = FileManager.default.fileExists(atPath: vmsDir) ? vmsDir : home
        
        let scriptContent = """
        #!/bin/bash
        cd "\(workingDir)"
        export PATH="$HOME/.local/bin:$PATH"
        clear
        echo "=================================================="
        echo "  Cube Environment"
        echo "  Working Directory: $(pwd)"
        echo "  Cube Binary:       $(which cube 2>/dev/null || echo 'not found')"
        echo "  Cubectl Binary:    $(which cubectl 2>/dev/null || echo 'not found')"
        echo "  vmnet Socket:      \(ServiceManager.shared.defaultSocketPath)"
        echo "=================================================="
        echo ""
        echo "Ready commands:"
        echo "  cubectl list               - list virtual machines"
        echo "  cubectl start <vm-name>    - start a virtual machine"
        echo "  cubectl --help             - view cubectl commands"
        echo "  cube --help                - view hypervisor engine options"
        echo ""
        exec "${SHELL:-/bin/zsh}" -l
        """
        
        try? scriptContent.write(toFile: scriptPath, atomically: true, encoding: .utf8)
        let permissions = [FileAttributeKey.posixPermissions: 0o755]
        try? FileManager.default.setAttributes(permissions, ofItemAtPath: scriptPath)
        
        let termAppUrl = VMManager.shared.resolveTerminalAppURL()
        let scriptUrl = URL(fileURLWithPath: scriptPath)
        let config = NSWorkspace.OpenConfiguration()
        NSWorkspace.shared.open([scriptUrl], withApplicationAt: termAppUrl, configuration: config) { _, error in
            if error != nil {
                let proc = Process()
                proc.executableURL = URL(fileURLWithPath: "/usr/bin/open")
                proc.arguments = ["-a", termAppUrl.path, scriptPath]
                do {
                    try proc.run()
                } catch {
                    let fallbackProc = Process()
                    fallbackProc.executableURL = URL(fileURLWithPath: "/usr/bin/open")
                    fallbackProc.arguments = ["-a", "Terminal", scriptPath]
                    try? fallbackProc.run()
                }
            }
        }
    }
    
    @objc private func showAboutWindow() {
        var options: [NSApplication.AboutPanelOptionKey: Any] = [
            .applicationVersion: "Build \(AppVersion.build)",
            .version: AppVersion.version,
            .applicationName: "Cube"
        ]
        if let creditsURL = Bundle.main.url(forResource: "Credits", withExtension: "rtf"),
           let creditsAttr = try? NSAttributedString(url: creditsURL, options: [:], documentAttributes: nil) {
            options[.credits] = creditsAttr
        }
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

    // MARK: - Virtual Machine Menu & Actions

    private func updateVMMenu() {
        for item in vmMenuItems {
            statusMenu.removeItem(item)
        }
        vmMenuItems.removeAll()

        let headerIndex = statusMenu.index(of: vmHeaderMenuItem)
        guard headerIndex >= 0 else { return }
        var insertIndex = headerIndex + 1

        let vms = VMManager.shared.vms
        if vms.isEmpty {
            let emptyItem = NSMenuItem(title: "  (No Virtual Machines)", action: nil, keyEquivalent: "")
            emptyItem.isEnabled = false
            statusMenu.insertItem(emptyItem, at: insertIndex)
            vmMenuItems.append(emptyItem)
        } else {
            for vm in vms {
                let statusDot = vm.isRunning ? "●" : "○"
                let vmItem = NSMenuItem(title: "  \(statusDot) \(vm.name)", action: nil, keyEquivalent: "")

                let attrTitle = NSMutableAttributedString(
                    string: "  \(statusDot) ",
                    attributes: [
                        .foregroundColor: vm.isRunning ? NSColor.systemGreen : NSColor.systemGray,
                        .font: NSFont.boldSystemFont(ofSize: 13)
                    ]
                )
                let os = vm.os ?? "FreeBSD"
                let detailText = vm.isRunning ? "\(vm.name) (\(os), running)" : "\(vm.name) (\(os), stopped)"
                attrTitle.append(NSAttributedString(
                    string: detailText,
                    attributes: [.font: NSFont.systemFont(ofSize: 13)]
                ))
                vmItem.attributedTitle = attrTitle

                let subMenu = NSMenu()

                let inspectItem = NSMenuItem(title: "Inspect in Dashboard...", action: #selector(inspectInDashboardAction(_:)), keyEquivalent: "")
                inspectItem.representedObject = vm
                inspectItem.target = self
                subMenu.addItem(inspectItem)

                let logsItem = NSMenuItem(title: "View Logs...", action: #selector(viewLogsAction(_:)), keyEquivalent: "")
                logsItem.representedObject = vm
                logsItem.target = self
                subMenu.addItem(logsItem)

                let commandsItem = NSMenuItem(title: "View CLI Commands...", action: #selector(viewCommandsAction(_:)), keyEquivalent: "")
                commandsItem.representedObject = vm
                commandsItem.target = self
                subMenu.addItem(commandsItem)
                subMenu.addItem(NSMenuItem.separator())

                if vm.isRunning {
                    let cfg = vm.loadConfig()
                    let effectiveSSH = vm.ssh_port ?? cfg.sshPort
                    if let sshPort = effectiveSSH {
                        let sshItem = NSMenuItem(title: "Connect via SSH (port \(sshPort))", action: #selector(openSSHAction(_:)), keyEquivalent: "")
                        sshItem.representedObject = vm
                        sshItem.target = self
                        subMenu.addItem(sshItem)
                    }

                    let effectiveVNC = vm.vnc ?? cfg.vnc
                    if let vnc = effectiveVNC {
                        let vncItem = NSMenuItem(title: "Open Screen Sharing (VNC: \(vnc))", action: #selector(openVNCAction(_:)), keyEquivalent: "")
                        vncItem.representedObject = vm
                        vncItem.target = self
                        subMenu.addItem(vncItem)
                    }

                    if effectiveSSH != nil || effectiveVNC != nil {
                        subMenu.addItem(NSMenuItem.separator())
                    }

                    let stopItem = NSMenuItem(title: "Stop Virtual Machine", action: #selector(stopVMAction(_:)), keyEquivalent: "")
                    stopItem.representedObject = vm
                    stopItem.target = self
                    subMenu.addItem(stopItem)
                } else {
                    let startItem = NSMenuItem(title: "Start Virtual Machine", action: #selector(startVMAction(_:)), keyEquivalent: "")
                    startItem.representedObject = vm
                    startItem.target = self
                    subMenu.addItem(startItem)

                    let deleteItem = NSMenuItem(title: "Delete Virtual Machine...", action: #selector(deleteVMAction(_:)), keyEquivalent: "")
                    deleteItem.representedObject = vm
                    deleteItem.target = self
                    subMenu.addItem(deleteItem)
                }

                vmItem.submenu = subMenu
                statusMenu.insertItem(vmItem, at: insertIndex)
                vmMenuItems.append(vmItem)
                insertIndex += 1
            }
        }
    }

    @objc private func openDashboard() {
        DashboardWindowController.shared.showWindow(nil)
    }

    @objc private func inspectInDashboardAction(_ sender: NSMenuItem) {
        guard let vm = sender.representedObject as? VMInfo else { return }
        DashboardWindowController.shared.selectVM(name: vm.name)
    }

    @objc private func viewLogsAction(_ sender: NSMenuItem) {
        guard let vm = sender.representedObject as? VMInfo else { return }
        VMLogWindowController.show(for: vm.name)
    }

    @objc private func openNewVMWindow() {
        NewVMWindowController.show()
    }

    @objc private func startVMAction(_ sender: NSMenuItem) {
        guard let vm = sender.representedObject as? VMInfo else { return }
        VMManager.shared.startVM(name: vm.name) { [weak self] success, errorMsg in
            if !success {
                self?.showAlert(title: "Failed to Start VM", message: errorMsg ?? "Unknown error")
            }
            VMManager.shared.refreshVMs { [weak self] _ in
                self?.updateVMMenu()
            }
        }
    }

    @objc private func stopVMAction(_ sender: NSMenuItem) {
        guard let vm = sender.representedObject as? VMInfo else { return }
        VMManager.shared.stopVM(name: vm.name) { [weak self] success, errorMsg in
            if !success {
                self?.showAlert(title: "Failed to Stop VM", message: errorMsg ?? "Unknown error")
            }
            VMManager.shared.refreshVMs { [weak self] _ in
                self?.updateVMMenu()
            }
        }
    }

    @objc private func deleteVMAction(_ sender: NSMenuItem) {
        guard let vm = sender.representedObject as? VMInfo else { return }
        let alert = NSAlert()
        alert.messageText = "Delete Virtual Machine '\(vm.name)'?"
        alert.informativeText = "This will permanently remove the VM directory and its virtual disk image. This action cannot be undone."
        alert.alertStyle = .critical
        alert.addButton(withTitle: "Delete")
        alert.addButton(withTitle: "Cancel")
        if alert.runModal() == .alertFirstButtonReturn {
            VMManager.shared.deleteVM(name: vm.name) { [weak self] success, errorMsg in
                if !success {
                    self?.showAlert(title: "Failed to Delete VM", message: errorMsg ?? "Unknown error")
                }
                VMManager.shared.refreshVMs { [weak self] _ in
                    self?.updateVMMenu()
                }
            }
        }
    }

    @objc private func openSSHAction(_ sender: NSMenuItem) {
        guard let vm = sender.representedObject as? VMInfo else { return }
        VMManager.shared.openTerminalSSH(for: vm)
    }

    @objc private func openVNCAction(_ sender: NSMenuItem) {
        guard let vm = sender.representedObject as? VMInfo else { return }
        VMManager.shared.openVNC(for: vm)
    }

    @objc private func viewCommandsAction(_ sender: NSMenuItem) {
        guard let vm = sender.representedObject as? VMInfo else { return }
        VMCommandsSheetController.show(for: vm)
    }
}
