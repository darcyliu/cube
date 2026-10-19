import Foundation
import AppKit
import UniformTypeIdentifiers

private class NewVMContentView: NSView {
    override var isFlipped: Bool { return true }
}

class NewVMWindowController: NSWindowController {
    static var shared: NewVMWindowController?

    private var nameField: NSTextField!
    private var osPopUp: NSPopUpButton!
    private var isoPathField: NSTextField!
    private var browseButton: NSButton!
    private var unattendedCheckbox: NSButton!
    private var cpuPopUp: NSPopUpButton!
    private var ramPopUp: NSPopUpButton!
    private var diskField: NSTextField!
    private var passwordField: NSTextField!
    private var netPopUp: NSPopUpButton!
    private var sshPortField: NSTextField!
    private var vncPortField: NSTextField!

    private var progressIndicator: NSProgressIndicator!
    private var statusLabel: NSTextField!
    private var logTextView: NSTextView!
    private var logScrollView: NSScrollView!
    private var copyLogsButton: NSButton!
    private var createButton: NSButton!
    private var cancelButton: NSButton!

    private var storagePathLabel: NSTextField!
    private var customizeStorageButton: NSButton!
    private var customStorageDir: String? = nil

    private var isBusy = false

    init() {
        let window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 520, height: 675),
            styleMask: [.titled, .closable],
            backing: .buffered,
            defer: false
        )
        window.title = "New Virtual Machine"
        window.center()
        window.isReleasedWhenClosed = false
        super.init(window: window)

        setupUI()
    }

    required init?(coder: NSCoder) {
        fatalError("init(coder:) has not been implemented")
    }

    static func show() {
        AppDelegate.ensureRegularActivation()
        if shared == nil {
            shared = NewVMWindowController()
        }
        shared?.resetUI()
        shared?.window?.makeKeyAndOrderFront(nil)
        NSApp.activate(ignoringOtherApps: true)
    }

    private func resetUI() {
        nameField.stringValue = "freebsd-\(Int.random(in: 100...999))"
        osPopUp.selectItem(at: 0)
        isoPathField.stringValue = ""
        isoPathField.isEnabled = false
        browseButton.isEnabled = false
        unattendedCheckbox.state = .on
        cpuPopUp.selectItem(at: 1) // 2 vCPUs
        ramPopUp.selectItem(at: 1) // 2048 MB
        diskField.stringValue = "20"
        passwordField.stringValue = "root"
        netPopUp.selectItem(at: 0)
        sshPortField.stringValue = "\(VMManager.shared.nextAvailableSSHPort())"
        vncPortField.stringValue = "\(VMManager.shared.nextAvailableVNCPort())"
        statusLabel.stringValue = "Ready"
        progressIndicator.stopAnimation(nil)
        progressIndicator.isHidden = true
        logTextView.string = ""
        logScrollView.isHidden = true
        copyLogsButton.isHidden = true
        customStorageDir = nil
        updateStoragePathDisplay()
        setFormEnabled(true)
    }

    private func setupUI() {
        guard let window = self.window else { return }
        let contentView = NewVMContentView(frame: window.contentView!.bounds)
        contentView.autoresizingMask = [.width, .height]
        window.contentView = contentView

        var y: CGFloat = 20

        // Title Header
        let headerLabel = NSTextField(labelWithString: "Create a New Virtual Machine")
        headerLabel.font = NSFont.boldSystemFont(ofSize: 16)
        headerLabel.frame = NSRect(x: 24, y: y, width: 470, height: 22)
        contentView.addSubview(headerLabel)
        y += 30

        // Subtitle
        let subLabel = NSTextField(labelWithString: "Configure and provision an ARM64 guest system using Cube.")
        subLabel.textColor = .secondaryLabelColor
        subLabel.font = NSFont.systemFont(ofSize: 12)
        subLabel.frame = NSRect(x: 24, y: y, width: 470, height: 16)
        contentView.addSubview(subLabel)
        y += 28

        // Box: Machine Identity
        y = addRow(label: "VM Name:", in: contentView, y: y) {
            nameField = NSTextField(frame: NSRect(x: 150, y: y, width: 330, height: 22))
            nameField.placeholderString = "e.g. freebsd-work"
            NotificationCenter.default.addObserver(self, selector: #selector(nameFieldChanged), name: NSControl.textDidChangeNotification, object: nameField)
            return nameField
        }

        // Box: OS Selection
        y = addRow(label: "Operating System:", in: contentView, y: y) {
            osPopUp = NSPopUpButton(frame: NSRect(x: 150, y: y - 2, width: 330, height: 26))
            osPopUp.addItem(withTitle: "FreeBSD 14.4 (RELEASE) - Official Netboot")
            osPopUp.addItem(withTitle: "FreeBSD 15.1 (RELEASE) - Packaged Base")
            osPopUp.addItem(withTitle: "Custom Installer ISO...")
            osPopUp.target = self
            osPopUp.action = #selector(osSelectionChanged)
            return osPopUp
        }

        // Custom ISO Path
        let isoRow = NSView(frame: NSRect(x: 150, y: y, width: 330, height: 24))
        isoPathField = NSTextField(frame: NSRect(x: 0, y: 0, width: 240, height: 22))
        isoPathField.placeholderString = "Select an .iso image"
        isoPathField.isEnabled = false
        isoRow.addSubview(isoPathField)

        browseButton = NSButton(title: "Browse...", target: self, action: #selector(browseIsoClicked))
        browseButton.frame = NSRect(x: 246, y: -2, width: 84, height: 26)
        browseButton.isEnabled = false
        isoRow.addSubview(browseButton)
        contentView.addSubview(isoRow)
        y += 30

        // Unattended checkbox
        unattendedCheckbox = NSButton(checkboxWithTitle: "Run Automated Unattended Installation (Zero Prompts)", target: nil, action: nil)
        unattendedCheckbox.state = .on
        unattendedCheckbox.frame = NSRect(x: 150, y: y, width: 340, height: 18)
        contentView.addSubview(unattendedCheckbox)
        y += 28

        // Separator
        let sep1 = NSBox(frame: NSRect(x: 24, y: y, width: 472, height: 1))
        sep1.boxType = .separator
        contentView.addSubview(sep1)
        y += 14

        // Hardware: vCPUs
        y = addRow(label: "Virtual CPUs:", in: contentView, y: y) {
            cpuPopUp = NSPopUpButton(frame: NSRect(x: 150, y: y - 2, width: 140, height: 26))
            cpuPopUp.addItems(withTitles: ["1 Core", "2 Cores", "4 Cores", "8 Cores"])
            cpuPopUp.selectItem(at: 1)
            return cpuPopUp
        }

        // Hardware: RAM
        y = addRow(label: "Memory (RAM):", in: contentView, y: y) {
            ramPopUp = NSPopUpButton(frame: NSRect(x: 150, y: y - 2, width: 140, height: 26))
            ramPopUp.addItems(withTitles: ["1024 MB (1 GB)", "2048 MB (2 GB)", "4096 MB (4 GB)", "8192 MB (8 GB)", "16384 MB (16 GB)"])
            ramPopUp.selectItem(at: 1)
            return ramPopUp
        }

        // Hardware: Disk Size
        y = addRow(label: "Virtual Disk Size:", in: contentView, y: y) {
            let container = NSView(frame: NSRect(x: 150, y: y, width: 140, height: 22))
            diskField = NSTextField(frame: NSRect(x: 0, y: 0, width: 80, height: 22))
            diskField.stringValue = "20"
            let gbLabel = NSTextField(labelWithString: "GB (sparse)")
            gbLabel.frame = NSRect(x: 85, y: 2, width: 70, height: 18)
            container.addSubview(diskField)
            container.addSubview(gbLabel)
            return container
        }

        // Hardware: Storage Location
        y = addRow(label: "Storage Location:", in: contentView, y: y) {
            let container = NSView(frame: NSRect(x: 150, y: y, width: 330, height: 24))
            storagePathLabel = NSTextField(labelWithString: "")
            storagePathLabel.font = NSFont.monospacedSystemFont(ofSize: 11, weight: .regular)
            storagePathLabel.lineBreakMode = .byTruncatingMiddle
            storagePathLabel.frame = NSRect(x: 0, y: 2, width: 235, height: 18)
            container.addSubview(storagePathLabel)

            customizeStorageButton = NSButton(title: "Customize...", target: self, action: #selector(customizeStorageClicked))
            customizeStorageButton.bezelStyle = .rounded
            customizeStorageButton.font = NSFont.systemFont(ofSize: 11)
            customizeStorageButton.frame = NSRect(x: 238, y: -2, width: 92, height: 26)
            container.addSubview(customizeStorageButton)
            return container
        }

        // Separator
        let sep2 = NSBox(frame: NSRect(x: 24, y: y, width: 472, height: 1))
        sep2.boxType = .separator
        contentView.addSubview(sep2)
        y += 14

        // Networking: Root Password
        y = addRow(label: "Root Password:", in: contentView, y: y) {
            passwordField = NSSecureTextField(frame: NSRect(x: 150, y: y, width: 180, height: 22))
            passwordField.stringValue = "root"
            return passwordField
        }

        // Network Mode Row
        y = addRow(label: "Network Mode:", in: contentView, y: y) {
            let hasEntitlement = VMManager.shared.hasNetworkingEntitlement()
            netPopUp = NSPopUpButton(frame: NSRect(x: 150, y: y, width: 330, height: 26), pullsDown: false)
            if hasEntitlement {
                netPopUp.addItem(withTitle: "Shared Proxy (vmnet-proxy)")
                netPopUp.addItem(withTitle: "Direct vmnet NAT (vmnet) — Entitled ✓")
                netPopUp.addItem(withTitle: "Bridged LAN (bridge) — Entitled ✓")
                netPopUp.addItem(withTitle: "Offline / Disabled (none)")
            } else {
                netPopUp.addItem(withTitle: "Shared Proxy (vmnet-proxy) — Recommended")
                netPopUp.addItem(withTitle: "Direct vmnet NAT (vmnet) — Requires Entitlement")
                netPopUp.addItem(withTitle: "Bridged LAN (bridge) — Requires Entitlement")
                netPopUp.addItem(withTitle: "Offline / Disabled (none)")
            }
            netPopUp.selectItem(at: 0)
            return netPopUp
        }

        // Ports Row
        y = addRow(label: "Network Ports:", in: contentView, y: y) {
            let container = NSView(frame: NSRect(x: 150, y: y, width: 330, height: 22))
            let sshLabel = NSTextField(labelWithString: "SSH:")
            sshLabel.frame = NSRect(x: 0, y: 2, width: 35, height: 18)
            sshPortField = NSTextField(frame: NSRect(x: 36, y: 0, width: 65, height: 22))
            sshPortField.stringValue = "\(VMManager.shared.nextAvailableSSHPort())"

            let vncLabel = NSTextField(labelWithString: "VNC:")
            vncLabel.frame = NSRect(x: 125, y: 2, width: 35, height: 18)
            vncPortField = NSTextField(frame: NSRect(x: 162, y: 0, width: 65, height: 22))
            vncPortField.stringValue = "\(VMManager.shared.nextAvailableVNCPort())"

            container.addSubview(sshLabel)
            container.addSubview(sshPortField)
            container.addSubview(vncLabel)
            container.addSubview(vncPortField)
            return container
        }

        y += 10

        // Live Log output view
        logScrollView = NSScrollView(frame: NSRect(x: 24, y: y, width: 472, height: 75))
        logScrollView.hasVerticalScroller = true
        logScrollView.borderType = .bezelBorder
        logScrollView.isHidden = true
        logTextView = NSTextView(frame: logScrollView.contentView.bounds)
        logTextView.isEditable = false
        logTextView.font = NSFont.userFixedPitchFont(ofSize: 10)
        logScrollView.documentView = logTextView
        contentView.addSubview(logScrollView)

        // Progress bar and status label
        progressIndicator = NSProgressIndicator(frame: NSRect(x: 24, y: y + 80, width: 180, height: 16))
        progressIndicator.isIndeterminate = true
        progressIndicator.style = .bar
        progressIndicator.isHidden = true
        contentView.addSubview(progressIndicator)

        statusLabel = NSTextField(labelWithString: "Ready")
        statusLabel.font = NSFont.systemFont(ofSize: 11)
        statusLabel.textColor = .secondaryLabelColor
        statusLabel.frame = NSRect(x: 24, y: y + 98, width: 180, height: 16)
        contentView.addSubview(statusLabel)

        // Action Buttons
        copyLogsButton = NSButton(title: "Copy Logs", target: self, action: #selector(copyLogsClicked))
        copyLogsButton.bezelStyle = .rounded
        copyLogsButton.frame = NSRect(x: 215, y: y + 85, width: 95, height: 28)
        copyLogsButton.isHidden = true
        contentView.addSubview(copyLogsButton)

        cancelButton = NSButton(title: "Cancel", target: self, action: #selector(cancelClicked))
        cancelButton.frame = NSRect(x: 315, y: y + 85, width: 85, height: 28)
        contentView.addSubview(cancelButton)

        createButton = NSButton(title: "Create VM", target: self, action: #selector(createClicked))
        createButton.bezelStyle = .rounded
        createButton.keyEquivalent = "\r"
        createButton.frame = NSRect(x: 405, y: y + 85, width: 95, height: 28)
        contentView.addSubview(createButton)

        updateStoragePathDisplay()
    }

    private func addRow(label text: String, in view: NSView, y: CGFloat, controlFactory: () -> NSView) -> CGFloat {
        let label = NSTextField(labelWithString: text)
        label.alignment = .right
        label.frame = NSRect(x: 20, y: y + 2, width: 120, height: 18)
        view.addSubview(label)

        let control = controlFactory()
        view.addSubview(control)
        return y + 30
    }

    @objc private func osSelectionChanged() {
        let isCustom = (osPopUp.indexOfSelectedItem == 2)
        isoPathField.isEnabled = isCustom
        browseButton.isEnabled = isCustom
        if isCustom && isoPathField.stringValue.isEmpty {
            browseIsoClicked()
        }
    }

    @objc private func browseIsoClicked() {
        let panel = NSOpenPanel()
        panel.canChooseFiles = true
        panel.canChooseDirectories = false
        panel.allowsMultipleSelection = false
        if #available(macOS 11.0, *) {
            var types: [UTType] = [.diskImage]
            if let isoType = UTType(filenameExtension: "iso") {
                types.append(isoType)
            }
            if let imgType = UTType(filenameExtension: "img") {
                types.append(imgType)
            }
            panel.allowedContentTypes = types
        }
        panel.beginSheetModal(for: self.window!) { response in
            if response == .OK, let url = panel.url {
                self.isoPathField.stringValue = url.path
            }
        }
    }

    @objc private func cancelClicked() {
        if isBusy {
            let alert = NSAlert()
            alert.messageText = "Creation in Progress"
            alert.informativeText = "A VM is currently being provisioned. Please wait for the process to complete."
            alert.runModal()
            return
        }
        self.window?.close()
    }

    @objc private func createClicked() {
        let name = nameField.stringValue.trimmingCharacters(in: .whitespacesAndNewlines)
        if name.isEmpty {
            showError("VM Name cannot be empty.")
            return
        }

        let regex = try! NSRegularExpression(pattern: "^[A-Za-z0-9][A-Za-z0-9_-]{0,47}$")
        if regex.firstMatch(in: name, range: NSRange(location: 0, length: name.utf16.count)) == nil {
            showError("VM Name must be 1-48 letters, numbers, underscores, or hyphens.")
            return
        }

        var osVersion = "14.4"
        var customIso: String? = nil
        switch osPopUp.indexOfSelectedItem {
        case 0: osVersion = "14.4"
        case 1: osVersion = "15.1"
        case 2:
            osVersion = "custom"
            customIso = isoPathField.stringValue.trimmingCharacters(in: .whitespacesAndNewlines)
            if customIso?.isEmpty ?? true || !FileManager.default.fileExists(atPath: customIso!) {
                showError("Please select a valid custom ISO file.")
                return
            }
        default: break
        }

        let cpus = [1, 2, 4, 8][cpuPopUp.indexOfSelectedItem]
        let rams = [1024, 2048, 4096, 8192, 16384][ramPopUp.indexOfSelectedItem]
        let diskGB = Int(diskField.stringValue) ?? 20
        let password = passwordField.stringValue.isEmpty ? "root" : passwordField.stringValue
        let netMode: String
        switch netPopUp.indexOfSelectedItem {
        case 1: netMode = "vmnet"
        case 2: netMode = "bridge"
        case 3: netMode = "none"
        default: netMode = "vmnet-proxy"
        }
        let sshPort = Int(sshPortField.stringValue)
        let vncPort = Int(vncPortField.stringValue)

        let destDir = customStorageDir.map { "\($0)/\(name)" }
        let options = CreateVMOptions(
            name: name,
            osVersion: osVersion,
            customIsoPath: customIso,
            isUnattended: (unattendedCheckbox.state == .on),
            cpus: cpus,
            ramMB: rams,
            diskSizeGB: diskGB,
            password: password,
            netMode: netMode,
            sshPort: sshPort,
            vncPort: vncPort,
            destinationDir: destDir
        )

        isBusy = true
        setFormEnabled(false)
        logScrollView.isHidden = false
        copyLogsButton.isHidden = false
        progressIndicator.isHidden = false
        progressIndicator.startAnimation(nil)
        statusLabel.stringValue = "Creating VM and preparing disk..."
        logTextView.string = "[Cube] Starting creation for '\(name)'...\n"

        VMManager.shared.createVM(options: options, progressHandler: { [weak self] chunk in
            guard let self = self else { return }
            self.logTextView.textStorage?.append(NSAttributedString(string: chunk, attributes: [
                .font: NSFont.userFixedPitchFont(ofSize: 10) ?? NSFont.systemFont(ofSize: 10)
            ]))
            self.logTextView.scrollToEndOfDocument(nil)
            if chunk.contains("Downloading") {
                self.statusLabel.stringValue = "Downloading ISO image..."
            } else if chunk.contains("unattended") || chunk.contains("install") {
                self.statusLabel.stringValue = "Running installation..."
            }
        }) { [weak self] success, error in
            guard let self = self else { return }
            self.isBusy = false
            self.progressIndicator.stopAnimation(nil)
            self.setFormEnabled(true)

            if success {
                self.statusLabel.stringValue = "Created successfully!"
                let alert = NSAlert()
                alert.messageText = "Virtual Machine Ready"
                alert.informativeText = "VM '\(name)' was created successfully. Would you like to start it now?"
                alert.addButton(withTitle: "Start VM")
                alert.addButton(withTitle: "Done")
                let resp = alert.runModal()
                if resp == .alertFirstButtonReturn {
                    VMManager.shared.startVM(name: name) { startSuccess, startError in
                        if !startSuccess {
                            DispatchQueue.main.async {
                                let errAlert = NSAlert()
                                errAlert.messageText = "Failed to Start Virtual Machine"
                                errAlert.informativeText = startError ?? "An unknown error occurred while starting the VM."
                                errAlert.alertStyle = .warning
                                errAlert.runModal()
                            }
                        }
                    }
                }
                self.window?.close()
            } else {
                self.statusLabel.stringValue = "Creation failed"
                self.showError(error ?? "An error occurred during VM creation.")
            }
        }
    }

    private func setFormEnabled(_ enabled: Bool) {
        nameField.isEnabled = enabled
        osPopUp.isEnabled = enabled
        unattendedCheckbox.isEnabled = enabled
        cpuPopUp.isEnabled = enabled
        ramPopUp.isEnabled = enabled
        diskField.isEnabled = enabled
        passwordField.isEnabled = enabled
        netPopUp.isEnabled = enabled
        sshPortField.isEnabled = enabled
        vncPortField.isEnabled = enabled
        customizeStorageButton.isEnabled = enabled
        createButton.isEnabled = enabled
        if enabled {
            osSelectionChanged()
        } else {
            isoPathField.isEnabled = false
            browseButton.isEnabled = false
        }
    }

    private func showError(_ text: String) {
        let alert = NSAlert()
        alert.messageText = "Cannot Create Virtual Machine"
        alert.informativeText = text
        alert.alertStyle = .warning
        alert.runModal()
    }

    @objc private func copyLogsClicked() {
        let text = logTextView.string
        guard !text.isEmpty else { return }
        let pasteboard = NSPasteboard.general
        pasteboard.clearContents()
        pasteboard.setString(text, forType: .string)
        let orig = statusLabel.stringValue
        statusLabel.stringValue = "Logs copied to clipboard!"
        DispatchQueue.main.asyncAfter(deadline: .now() + 2.0) { [weak self] in
            if self?.statusLabel.stringValue == "Logs copied to clipboard!" {
                self?.statusLabel.stringValue = orig
            }
        }
    }

    @objc private func nameFieldChanged() {
        updateStoragePathDisplay()
    }

    private func updateStoragePathDisplay() {
        let name = nameField?.stringValue.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        let vmName = name.isEmpty ? "<name>" : name
        if let custom = customStorageDir {
            let short = custom.replacingOccurrences(of: NSHomeDirectory(), with: "~")
            storagePathLabel?.stringValue = "\(short)/\(vmName)"
            storagePathLabel?.textColor = .labelColor
            customizeStorageButton?.title = "Reset"
        } else {
            let base = VMManager.shared.vmsDirectory.replacingOccurrences(of: NSHomeDirectory(), with: "~")
            storagePathLabel?.stringValue = "\(base)/\(vmName)"
            storagePathLabel?.textColor = .secondaryLabelColor
            customizeStorageButton?.title = "Customize..."
        }
    }

    @objc private func customizeStorageClicked() {
        if customStorageDir != nil {
            customStorageDir = nil
            updateStoragePathDisplay()
            return
        }

        let panel = NSOpenPanel()
        panel.canChooseFiles = false
        panel.canChooseDirectories = true
        panel.allowsMultipleSelection = false
        panel.canCreateDirectories = true
        panel.prompt = "Select"
        panel.message = "Choose Storage Location for VM"
        let currentDefault = VMManager.shared.vmsDirectory
        if FileManager.default.fileExists(atPath: currentDefault) {
            panel.directoryURL = URL(fileURLWithPath: currentDefault)
        }
        if panel.runModal() == .OK, let url = panel.url {
            customStorageDir = url.path
            updateStoragePathDisplay()
        }
    }
}
